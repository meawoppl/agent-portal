//! Per-version envelope encryption. Immutable opaque identifiers bind both
//! layers; names and ACLs may change without invalidating ciphertext.
use openssl::{
    rand::rand_bytes,
    symm::{decrypt_aead, encrypt_aead, Cipher},
};
use std::{collections::HashMap, fmt, path::Path, sync::Arc};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

pub struct SensitiveValue(pub Zeroizing<Vec<u8>>);
impl fmt::Debug for SensitiveValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl fmt::Display for SensitiveValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl SensitiveValue {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Identity {
    pub organization: Uuid,
    pub secret: Uuid,
    pub version: Uuid,
}
impl Identity {
    fn aad(self) -> Vec<u8> {
        format!(
            "agent-portal-vault:1:{}:{}:{}",
            self.organization, self.secret, self.version
        )
        .into_bytes()
    }
    fn context(self) -> HashMap<String, String> {
        HashMap::from([
            ("organization".into(), self.organization.to_string()),
            ("secret".into(), self.secret.to_string()),
            ("version".into(), self.version.to_string()),
            ("format".into(), "1".into()),
        ])
    }
}
#[derive(Clone)]
pub enum EnvelopeCrypto {
    Kms {
        client: aws_sdk_kms::Client,
        key_id: String,
    },
    Development {
        key: Arc<Zeroizing<Vec<u8>>>,
    },
}
#[derive(Debug, Clone)]
pub struct Envelope {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub tag: Vec<u8>,
    pub wrapped_dek: Vec<u8>,
    pub wrapping_key_id: String,
    pub crypto_format: i32,
}
#[derive(Debug, thiserror::Error)]
#[error("Secret encryption service unavailable or ciphertext authentication failed")]
pub struct CryptoError;
impl EnvelopeCrypto {
    pub async fn from_env(dev_mode: bool) -> anyhow::Result<Option<Self>> {
        let kms = std::env::var("PORTAL_SECRETS_KMS_KEY_ID").ok();
        let file = std::env::var("PORTAL_SECRETS_DEV_KEY_FILE").ok();
        match (kms, file) {
            (None, None) => Ok(None),
            (Some(_), Some(_)) => anyhow::bail!("Configure exactly one secrets wrapping provider"),
            (Some(key_id), None) => {
                if key_id.trim().is_empty()
                    || key_id.len() > 2048
                    || key_id.chars().any(char::is_control)
                {
                    anyhow::bail!("Empty secrets KMS key ID")
                };
                let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
                if config.region().is_none() {
                    anyhow::bail!(
                        "Secrets KMS requires an AWS region (AWS_REGION or workload configuration)"
                    )
                };
                Ok(Some(Self::Kms {
                    client: aws_sdk_kms::Client::new(&config),
                    key_id,
                }))
            }
            (None, Some(file)) => {
                if !dev_mode {
                    anyhow::bail!("Secrets key files require --dev-mode")
                };
                Ok(Some(Self::development_file(Path::new(&file))?))
            }
        }
    }
    pub fn development_file(path: &Path) -> anyhow::Result<Self> {
        if !cfg!(unix) {
            anyhow::bail!("Development key files require Unix permission checks");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_file() || metadata.mode() & 0o077 != 0 {
                anyhow::bail!("Secrets development key must be a regular protected 0600 file")
            };
        }
        use std::io::Read;
        let file = std::fs::File::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let before = std::fs::symlink_metadata(path)?;
            let opened = file.metadata()?;
            if !before.is_file()
                || !opened.is_file()
                || opened.mode() & 0o077 != 0
                || before.ino() != opened.ino()
                || before.dev() != opened.dev()
            {
                anyhow::bail!("Secrets development key must be a stable protected regular file")
            }
        }
        let mut key = Zeroizing::new(Vec::with_capacity(33));
        file.take(33).read_to_end(&mut key)?;
        if key.len() != 32 {
            anyhow::bail!("Secrets development key must contain exactly 32 bytes")
        };
        Ok(Self::Development { key: Arc::new(key) })
    }
    pub async fn seal(
        &self,
        id: Identity,
        value: &SensitiveValue,
    ) -> Result<Envelope, CryptoError> {
        if value.bytes().is_empty() || value.bytes().len() > 65536 {
            return Err(CryptoError);
        }
        let aad = id.aad();
        let (mut dek, wrapped_dek, wrapping_key_id) = match self {
            Self::Kms { client, key_id } => {
                let mut response = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    client
                        .generate_data_key()
                        .key_id(key_id)
                        .key_spec(aws_sdk_kms::types::DataKeySpec::Aes256)
                        .set_encryption_context(Some(id.context()))
                        .send(),
                )
                .await
                .map_err(|_| CryptoError)?
                .map_err(|_| CryptoError)?;
                let key = response.plaintext.take().ok_or(CryptoError)?.into_inner();
                (
                    Zeroizing::new(key),
                    response
                        .ciphertext_blob
                        .take()
                        .ok_or(CryptoError)?
                        .into_inner(),
                    response.key_id.take().ok_or(CryptoError)?,
                )
            }
            Self::Development { key } => {
                let mut dek = Zeroizing::new(vec![0; 32]);
                rand_bytes(&mut dek).map_err(|_| CryptoError)?;
                let mut nonce = vec![0; 12];
                rand_bytes(&mut nonce).map_err(|_| CryptoError)?;
                let mut tag = vec![0; 16];
                let encrypted = encrypt_aead(
                    Cipher::aes_256_gcm(),
                    key,
                    Some(&nonce),
                    &aad,
                    &dek,
                    &mut tag,
                )
                .map_err(|_| CryptoError)?;
                nonce.extend(tag);
                nonce.extend(encrypted);
                (dek, nonce, "development-key-file-v1".into())
            }
        };
        if dek.len() != 32 {
            return Err(CryptoError);
        }
        let mut nonce = vec![0; 12];
        rand_bytes(&mut nonce).map_err(|_| CryptoError)?;
        let mut tag = vec![0; 16];
        let ciphertext = encrypt_aead(
            Cipher::aes_256_gcm(),
            &dek,
            Some(&nonce),
            &aad,
            value.bytes(),
            &mut tag,
        )
        .map_err(|_| CryptoError)?;
        dek.zeroize();
        Ok(Envelope {
            ciphertext,
            nonce,
            tag,
            wrapped_dek,
            wrapping_key_id,
            crypto_format: 1,
        })
    }
    pub async fn open(
        &self,
        id: Identity,
        envelope: &Envelope,
    ) -> Result<SensitiveValue, CryptoError> {
        if envelope.crypto_format != 1 || envelope.nonce.len() != 12 || envelope.tag.len() != 16 {
            return Err(CryptoError);
        }
        let aad = id.aad();
        let dek = match self {
            Self::Kms { client, .. } => {
                let mut response = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    client
                        .decrypt()
                        .key_id(&envelope.wrapping_key_id)
                        .ciphertext_blob(aws_sdk_kms::primitives::Blob::new(
                            envelope.wrapped_dek.clone(),
                        ))
                        .set_encryption_context(Some(id.context()))
                        .send(),
                )
                .await
                .map_err(|_| CryptoError)?
                .map_err(|_| CryptoError)?;
                Zeroizing::new(response.plaintext.take().ok_or(CryptoError)?.into_inner())
            }
            Self::Development { key } => {
                if envelope.wrapping_key_id != "development-key-file-v1"
                    || envelope.wrapped_dek.len() != 60
                {
                    return Err(CryptoError);
                };
                Zeroizing::new(
                    decrypt_aead(
                        Cipher::aes_256_gcm(),
                        key,
                        Some(&envelope.wrapped_dek[..12]),
                        &aad,
                        &envelope.wrapped_dek[28..],
                        &envelope.wrapped_dek[12..28],
                    )
                    .map_err(|_| CryptoError)?,
                )
            }
        };
        if dek.len() != 32 {
            return Err(CryptoError);
        }
        decrypt_aead(
            Cipher::aes_256_gcm(),
            &dek,
            Some(&envelope.nonce),
            &aad,
            &envelope.ciphertext,
            &envelope.tag,
        )
        .map(SensitiveValue::new)
        .map_err(|_| CryptoError)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn envelope_tamper_and_identity_swaps_fail_closed() {
        let crypto = EnvelopeCrypto::Development {
            key: Arc::new(Zeroizing::new(vec![7; 32])),
        };
        let id = Identity {
            organization: Uuid::new_v4(),
            secret: Uuid::new_v4(),
            version: Uuid::new_v4(),
        };
        let value = SensitiveValue::new(b"not-in-a-database-dump\n".to_vec());
        let envelope = crypto.seal(id, &value).await.unwrap();
        assert_eq!(
            crypto.open(id, &envelope).await.unwrap().bytes(),
            value.bytes()
        );
        for wrong in [
            Identity {
                organization: Uuid::new_v4(),
                ..id
            },
            Identity {
                secret: Uuid::new_v4(),
                ..id
            },
            Identity {
                version: Uuid::new_v4(),
                ..id
            },
        ] {
            assert!(crypto.open(wrong, &envelope).await.is_err())
        }
        let mut tampered = envelope.clone();
        tampered.ciphertext[0] ^= 1;
        assert!(crypto.open(id, &tampered).await.is_err());
        assert!(!format!("{value:?}").contains("not-in"));
        assert!(!envelope
            .ciphertext
            .windows(value.bytes().len())
            .any(|s| s == value.bytes()));
    }
}

#[cfg(test)]
mod provider_tests {
    use super::*;
    #[tokio::test]
    async fn different_wrapping_key_and_randomized_versions() {
        let crypto = EnvelopeCrypto::Development {
            key: Arc::new(Zeroizing::new(vec![1; 32])),
        };
        let wrong = EnvelopeCrypto::Development {
            key: Arc::new(Zeroizing::new(vec![2; 32])),
        };
        let id = Identity {
            organization: Uuid::new_v4(),
            secret: Uuid::new_v4(),
            version: Uuid::new_v4(),
        };
        let value = SensitiveValue::new(b"byte-exact\n\0value".to_vec());
        let first = crypto.seal(id, &value).await.unwrap();
        let second = crypto.seal(id, &value).await.unwrap();
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.wrapped_dek, second.wrapped_dek);
        assert_ne!(first.ciphertext, second.ciphertext);
        assert!(wrong.open(id, &first).await.is_err());
        assert_eq!(
            crypto.open(id, &first).await.unwrap().bytes(),
            value.bytes()
        );
    }
    #[test]
    #[cfg(unix)]
    fn development_key_requires_protected_regular_file() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("key");
        std::fs::write(&path, [7; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(EnvelopeCrypto::development_file(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(EnvelopeCrypto::development_file(&path).is_ok());
        let symlink = temp.path().join("link");
        std::os::unix::fs::symlink(&path, &symlink).unwrap();
        assert!(EnvelopeCrypto::development_file(&symlink).is_err());
    }
    #[tokio::test]
    async fn kms_outage_does_not_fall_back_to_local_wrapping() {
        let config = aws_sdk_kms::config::Builder::new()
            .behavior_version_latest()
            .region(aws_sdk_kms::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_kms::config::Credentials::new(
                "test", "test", None, None, "test",
            ))
            .endpoint_url("http://127.0.0.1:1")
            .retry_config(aws_sdk_kms::config::retry::RetryConfig::disabled())
            .build();
        let crypto = EnvelopeCrypto::Kms {
            client: aws_sdk_kms::Client::from_conf(config),
            key_id: "test-key".into(),
        };
        let id = Identity {
            organization: Uuid::new_v4(),
            secret: Uuid::new_v4(),
            version: Uuid::new_v4(),
        };
        assert!(crypto
            .seal(id, &SensitiveValue::new(b"unavailable".to_vec()))
            .await
            .is_err());
    }
}
