//! Resolve a stable Web Push identity before accepting browser subscriptions.
//!
//! Browser subscriptions are bound to the public key, so generating a new key
//! on each boot (or on each replica) silently breaks delivery. The database
//! owns the default identity; explicit environment keys remain an override.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use diesel::prelude::*;
use web_push::VapidSignatureBuilder;

use crate::schema::web_push_keys;

// Deliberately no Debug/Serialize: the private key must not reach logs or APIs.
pub(crate) struct VapidKeys {
    pub public_key: String,
    pub private_key: String,
}

impl VapidKeys {
    fn generate() -> Result<Self> {
        let curve = openssl::ec::EcGroup::from_curve_name(openssl::nid::Nid::X9_62_PRIME256V1)
            .context("could not initialize VAPID curve")?;
        let secret =
            openssl::ec::EcKey::generate(&curve).context("could not generate Web Push identity")?;
        let private = secret
            .private_key()
            .to_vec_padded(32)
            .context("could not encode Web Push identity")?;
        Self::from_private_key(&URL_SAFE_NO_PAD.encode(private))
    }

    fn from_private_key(private_key: &str) -> Result<Self> {
        let private_key = private_key.trim();
        let builder = if private_key.contains("BEGIN") {
            VapidSignatureBuilder::from_pem_no_sub(private_key.as_bytes())
        } else {
            VapidSignatureBuilder::from_base64_no_sub(private_key)
        }
        .map_err(|_| anyhow::anyhow!("invalid Web Push VAPID private key"))?;
        Ok(Self {
            public_key: URL_SAFE_NO_PAD.encode(builder.get_public_key()),
            private_key: private_key.to_owned(),
        })
    }
}

/// Private-only overrides derive their public key. A public-only or mismatched
/// override is an error: silently replacing it would strand existing devices.
fn configured_keys(private: Option<&str>, public: Option<&str>) -> Result<Option<VapidKeys>> {
    let private = private.map(str::trim).filter(|key| !key.is_empty());
    let public = public.map(str::trim).filter(|key| !key.is_empty());
    match (private, public) {
        (None, None) => Ok(None),
        (None, Some(_)) => bail!(
            "PORTAL_VAPID_PUBLIC_KEY requires PORTAL_VAPID_PRIVATE_KEY; \
             remove both to use automatic Web Push keys"
        ),
        (Some(private), public) => {
            let keys =
                VapidKeys::from_private_key(private).context("invalid PORTAL_VAPID_PRIVATE_KEY")?;
            if let Some(public) = public {
                // Accept optional base64 padding, but always serve the canonical key.
                if public.trim_end_matches('=') != keys.public_key {
                    bail!("PORTAL_VAPID_PUBLIC_KEY does not match PORTAL_VAPID_PRIVATE_KEY");
                }
            }
            Ok(Some(keys))
        }
    }
}

pub(crate) fn resolve(
    conn: &mut PgConnection,
    private: Option<&str>,
    public: Option<&str>,
) -> Result<VapidKeys> {
    if let Some(keys) = configured_keys(private, public)? {
        tracing::info!("Web Push enabled with administrator-provided VAPID key");
        return Ok(keys);
    }

    let existing = web_push_keys::table
        .find(true)
        .select(web_push_keys::private_key)
        .first::<String>(conn)
        .optional()
        .context("could not load Web Push identity")?;
    let private_key = match existing {
        Some(key) => key,
        None => {
            let candidate = VapidKeys::generate()?;
            diesel::insert_into(web_push_keys::table)
                .values((
                    web_push_keys::id.eq(true),
                    web_push_keys::private_key.eq(candidate.private_key),
                ))
                .on_conflict(web_push_keys::id)
                .do_nothing()
                .execute(conn)
                .context("could not persist Web Push identity")?;
            // A concurrent first boot may have won the insert. A separate
            // SELECT sees its committed row; never serve our losing candidate.
            web_push_keys::table
                .find(true)
                .select(web_push_keys::private_key)
                .first::<String>(conn)
                .context("could not reload Web Push identity")?
        }
    };
    let keys = VapidKeys::from_private_key(&private_key)
        .context("stored Web Push identity is invalid; restore it from a database backup")?;
    tracing::info!("Web Push enabled with persistent database VAPID key");
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::SimpleConnection;
    use std::sync::{Arc, Barrier};

    #[test]
    fn generated_key_signs_for_the_public_key_served_to_browsers() {
        use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
        let keys = VapidKeys::generate().unwrap();
        let public = URL_SAFE_NO_PAD.decode(&keys.public_key).unwrap();
        assert_eq!(public.len(), 65);
        assert_eq!(public[0], 4);
        let sub = web_push::SubscriptionInfo::new("https://push.example.invalid/test", "", "");
        let signature = VapidSignatureBuilder::from_base64(&keys.private_key, &sub)
            .unwrap()
            .build()
            .unwrap();
        let verifier = DecodingKey::from_ec_components(
            &URL_SAFE_NO_PAD.encode(&public[1..33]),
            &URL_SAFE_NO_PAD.encode(&public[33..]),
        )
        .unwrap();
        let mut validation = Validation::new(Algorithm::ES256);
        validation.set_audience(&["https://push.example.invalid"]);
        decode::<serde_json::Value>(&signature.auth_t, &verifier, &validation).unwrap();
        assert_ne!(keys.public_key, VapidKeys::generate().unwrap().public_key);
    }

    #[test]
    fn overrides_derive_and_validate_the_public_key_without_exposing_secrets() {
        let keys = VapidKeys::generate().unwrap();
        for public in [None, Some(keys.public_key.as_str())] {
            let resolved = configured_keys(Some(&keys.private_key), public)
                .unwrap()
                .unwrap();
            assert_eq!(resolved.public_key, keys.public_key);
        }
        let other = VapidKeys::generate().unwrap();
        let error = configured_keys(Some(&keys.private_key), Some(&other.public_key))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("does not match"));
        assert!(!error.contains(&keys.private_key));
        assert!(configured_keys(None, Some(&keys.public_key)).is_err());
        assert!(configured_keys(Some("bad-secret"), None).is_err());
        assert!(configured_keys(Some("  "), Some("")).unwrap().is_none());
        let pem = include_str!("../../../vendor/web-push/resources/vapid_test_key.pem");
        assert!(configured_keys(Some(pem), None).unwrap().is_some());
    }

    /// Separate schema and real connections exercise competing first boots
    /// without touching the application's identity or other tests' data.
    struct TestSchema(String);

    impl TestSchema {
        fn connect(&self) -> PgConnection {
            let mut conn =
                PgConnection::establish(&std::env::var("DATABASE_URL").unwrap()).unwrap();
            conn.batch_execute(&format!("SET search_path TO {}", self.0))
                .unwrap();
            conn
        }
    }

    impl Drop for TestSchema {
        fn drop(&mut self) {
            self.connect()
                .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.0))
                .unwrap();
        }
    }

    #[test]
    fn concurrent_boots_and_restarts_share_one_durable_identity() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let schema = TestSchema(format!("vapid_test_{}", uuid::Uuid::new_v4().simple()));
        pool.get()
            .unwrap()
            .batch_execute(&format!("CREATE SCHEMA {}", schema.0))
            .unwrap();
        schema
            .connect()
            .batch_execute(include_str!(
                "../../migrations/2026-11-07-120000_add_web_push_keys/up.sql"
            ))
            .unwrap();

        let barrier = Arc::new(Barrier::new(8));
        let keys = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let mut conn = schema.connect();
                        barrier.wait();
                        resolve(&mut conn, None, None).unwrap()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>()
        });
        let mut conn = schema.connect();
        let reloaded = resolve(&mut conn, None, None).unwrap();
        for key in keys {
            assert_eq!(key.public_key, reloaded.public_key);
            assert_eq!(key.private_key, reloaded.private_key);
        }
        assert_eq!(
            web_push_keys::table
                .count()
                .get_result::<i64>(&mut conn)
                .unwrap(),
            1
        );

        let custom = VapidKeys::generate().unwrap();
        let override_keys = resolve(
            &mut conn,
            Some(&custom.private_key),
            Some(&custom.public_key),
        )
        .unwrap();
        assert_eq!(override_keys.public_key, custom.public_key);
        assert_eq!(
            resolve(&mut conn, None, None).unwrap().public_key,
            reloaded.public_key
        );

        // Corruption must fail closed, never silently rotate existing devices.
        diesel::update(web_push_keys::table)
            .set(web_push_keys::private_key.eq("corrupt-test-secret"))
            .execute(&mut conn)
            .unwrap();
        let error = resolve(&mut conn, None, None).err().unwrap().to_string();
        assert!(error.contains("restore"));
        assert!(!error.contains("corrupt-test-secret"));
    }
}
