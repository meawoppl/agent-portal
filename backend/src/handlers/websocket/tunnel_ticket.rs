//! Credential binding a port-forward data-plane socket to one session *and one
//! control-connection generation* (#1506).
//!
//! The `/ws/session/data` upgrade cannot be authenticated the way `/ws/session`
//! is: the control socket authenticates via the proxy JWT inside its `Register`
//! message body, but the data plane speaks binary frames and has no such
//! message. It also needs something the control socket never did — proof of
//! *which connection generation* it belongs to, so a data socket opened by a
//! reconnecting proxy can never be wired to the session's previous connection
//! (or vice versa).
//!
//! One short-lived JWT solves both. The backend mints it during a successful
//! `Register` (where the generation is already known) and returns it in
//! `RegisterAck`; the proxy echoes it back as the data plane's first frame
//! (`TunnelFrame::Hello`). Because the generation is a *claim*, the binding is
//! established by verification rather than by a racy after-the-fact lookup.
//!
//! It rides in the frame body rather than a query parameter so it never reaches
//! access logs or `Referer` headers, and the TTL is short because the proxy
//! dials immediately after registering — a leaked ticket is useless within
//! seconds, and it authorizes only tunnel bytes for one already-forwarded
//! session, never session input.

use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Audience claim — distinct from every other portal token audience so a token
/// minted elsewhere with the same secret cannot authenticate a data socket.
const AUD_TUNNEL_DATA: &str = "portal-tunnel-data";

/// Ticket lifetime. The proxy dials the data plane immediately on receiving its
/// `RegisterAck`, so this only has to cover one connect.
const TICKET_TTL_SECS: i64 = 120;

#[derive(Debug, Serialize, Deserialize)]
struct TicketClaims {
    aud: String,
    /// Session this data socket may carry tunnel bytes for.
    session_id: Uuid,
    /// Session key (the proxy's session UUID string) used as the registry key.
    session_key: String,
    /// Control-connection generation this data socket is bound to.
    gen: u64,
    /// Negotiated frame size (#1511); defaulted so a ticket minted by an older
    /// backend still decodes, as V1.
    #[serde(default = "default_max_chunk")]
    max_chunk: u32,
    /// Negotiated flow-control window (#1511); defaulted as V1.
    #[serde(default = "default_initial_window")]
    initial_window: u32,
    exp: i64,
    iat: i64,
}

fn default_max_chunk() -> u32 {
    shared::TunnelSizing::V1.max_chunk
}

fn default_initial_window() -> u32 {
    shared::TunnelSizing::V1.initial_window
}

/// What a verified ticket authorizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelTicket {
    pub session_id: Uuid,
    pub session_key: String,
    pub gen: u64,
    /// Sizing the backend negotiated for this connection; the data socket's
    /// streams are configured to it.
    pub sizing: shared::TunnelSizing,
}

/// Mint a data-plane ticket for `(session_id, session_key, gen)`.
pub fn mint(
    jwt_secret: &str,
    session_id: Uuid,
    session_key: &str,
    gen: u64,
    sizing: shared::TunnelSizing,
    now: i64,
) -> Option<String> {
    let claims = TicketClaims {
        aud: AUD_TUNNEL_DATA.to_string(),
        session_id,
        session_key: session_key.to_string(),
        gen,
        max_chunk: sizing.max_chunk,
        initial_window: sizing.initial_window,
        exp: now + TICKET_TTL_SECS,
        iat: now,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(jwt_secret.as_bytes()),
    )
    .ok()
}

/// Verify a data-plane ticket, returning what it authorizes.
///
/// Requires both `exp` and `aud`: `set_audience` alone only checks `aud` *when
/// present*, so without `set_required_spec_claims` a token minted for some other
/// purpose with the same secret but no audience would pass this boundary. Same
/// hardening as the forward-origin cookie check in `forward_proxy.rs`.
pub fn verify(jwt_secret: &str, token: &str) -> Option<TunnelTicket> {
    let mut validation = Validation::default();
    validation.set_audience(&[AUD_TUNNEL_DATA]);
    validation.set_required_spec_claims(&["exp", "aud"]);
    let data = decode::<TicketClaims>(
        token,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &validation,
    )
    .ok()?;
    Some(TunnelTicket {
        session_id: data.claims.session_id,
        session_key: data.claims.session_key,
        gen: data.claims.gen,
        sizing: shared::TunnelSizing {
            max_chunk: data.claims.max_chunk,
            initial_window: data.claims.initial_window,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-value-at-least-32-bytes-long";

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    #[test]
    fn roundtrips_session_key_and_generation() {
        let sid = Uuid::new_v4();
        let token = mint(
            SECRET,
            sid,
            "session-key",
            42,
            shared::TunnelSizing::V2,
            now(),
        )
        .expect("mint");
        assert_eq!(
            verify(SECRET, &token),
            Some(TunnelTicket {
                session_id: sid,
                session_key: "session-key".to_string(),
                gen: 42,
                sizing: shared::TunnelSizing::V2,
            })
        );
    }

    #[test]
    fn rejects_wrong_secret() {
        let token = mint(
            SECRET,
            Uuid::new_v4(),
            "k",
            1,
            shared::TunnelSizing::V1,
            now(),
        )
        .expect("mint");
        assert!(verify("a-different-secret-at-least-32-bytes!!", &token).is_none());
    }

    /// Well past expiry. `jsonwebtoken`'s default validation allows 60s of
    /// clock-skew leeway, which we keep (proxy and backend clocks drift), so the
    /// skew here is comfortably beyond it rather than borderline.
    #[test]
    fn rejects_expired_ticket() {
        let stale = now() - TICKET_TTL_SECS - 3600;
        let token = mint(
            SECRET,
            Uuid::new_v4(),
            "k",
            1,
            shared::TunnelSizing::V1,
            stale,
        )
        .expect("mint");
        assert!(verify(SECRET, &token).is_none());
    }

    #[test]
    fn rejects_garbage_and_empty() {
        assert!(verify(SECRET, "").is_none());
        assert!(verify(SECRET, "not.a.jwt").is_none());
    }

    /// A token signed with the same secret for a *different* audience (or none)
    /// must not open a data socket — this is the boundary `forward_proxy.rs`
    /// learned to require explicitly.
    #[test]
    fn rejects_other_audience_and_missing_audience() {
        #[derive(Serialize)]
        struct Other {
            aud: &'static str,
            session_id: Uuid,
            session_key: &'static str,
            gen: u64,
            exp: i64,
            iat: i64,
        }
        #[derive(Serialize)]
        struct NoAud {
            session_id: Uuid,
            session_key: &'static str,
            gen: u64,
            exp: i64,
            iat: i64,
        }
        let key = EncodingKey::from_secret(SECRET.as_bytes());
        let (exp, iat) = (now() + 60, now());

        let wrong_aud = encode(
            &Header::default(),
            &Other {
                aud: "portal-forward-session",
                session_id: Uuid::nil(),
                session_key: "k",
                gen: 1,
                exp,
                iat,
            },
            &key,
        )
        .unwrap();
        assert!(verify(SECRET, &wrong_aud).is_none());

        let no_aud = encode(
            &Header::default(),
            &NoAud {
                session_id: Uuid::nil(),
                session_key: "k",
                gen: 1,
                exp,
                iat,
            },
            &key,
        )
        .unwrap();
        assert!(verify(SECRET, &no_aud).is_none());
    }

    /// The generation is what stops a ticket from a *previous* connection being
    /// replayed onto a reconnect: two registrations mint distinguishable
    /// tickets, and the caller compares `gen` against the live connection.
    #[test]
    fn generation_distinguishes_successive_connections() {
        let sid = Uuid::new_v4();
        let first = mint(SECRET, sid, "k", 7, shared::TunnelSizing::V1, now()).expect("mint");
        let second = mint(SECRET, sid, "k", 8, shared::TunnelSizing::V1, now()).expect("mint");
        assert_eq!(verify(SECRET, &first).unwrap().gen, 7);
        assert_eq!(verify(SECRET, &second).unwrap().gen, 8);
    }
}
