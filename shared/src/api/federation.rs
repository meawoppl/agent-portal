//! Linked-portal wire contracts. These types alone enable no routes or grants.
//!
//! A linked identity is always local to the issuing portal. Client caches must
//! include portal + account, never match accounts by email. Handlers must check
//! token scope, current federation policy, grant revocation and session role;
//! deserializing a request does not authorize it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CAP_LINKED_CLIENT_ENROLLMENT: &str = "linked_client_enrollment";
pub const CAP_LINKED_CLIENT_WRITE: &str = "linked_client_write";
pub const CAP_WS_TICKET: &str = "ws_ticket";
pub const CAP_SCOPED_MEDIA_URLS: &str = "scoped_media_urls";
pub const AUTH_MODE_LINKED_DEVICE_FLOW: &str = "linked_device_flow";
pub const AUTH_MODE_WS_TICKET: &str = "ws_ticket";

/// `GET` this path for a [`FederationDiscoveryResponse`]; unauthenticated.
pub const FEDERATION_DISCOVERY_PATH: &str = "/api/federation/discovery";

/// The federation protocol versions this build speaks. A client and portal
/// interoperate only where their ranges overlap.
pub const FEDERATION_PROTOCOL: FederationProtocolRange = FederationProtocolRange { min: 1, max: 1 };

/// Grant ceiling, not a session role. Write includes read but never elevates
/// viewer membership or permits minting a broader credential. Required on the
/// wire: a missing/unknown scope must not silently acquire write access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    Read,
    Write,
}

impl TokenScope {
    /// Whether this ceiling covers an operation's required scope. This is only
    /// one authorization check; it does not check policy, membership or expiry.
    pub fn permits(self, required: Self) -> bool {
        matches!(self, Self::Write) || required == Self::Read
    }

    /// Storage spelling, identical to the wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }

    /// Inverse of [`Self::as_str`]; unknown values are `None`, never a default.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            _ => None,
        }
    }
}

/// Admin policy for linked-client grants. Origins must be validated and
/// canonicalized by handlers (scheme + host + port, no path or wildcards).
/// Origin binding constrains browsers; it does not attest native clients.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum FederationPolicy {
    #[default]
    Off,
    Allowlist {
        origins: Vec<String>,
        max_scope: TokenScope,
    },
    Any {
        max_scope: TokenScope,
    },
}

/// Inclusive protocol compatibility range. Reject min > max and fail closed
/// when no supported version overlaps; this is not the portal release version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederationProtocolRange {
    pub min: u32,
    pub max: u32,
}

impl FederationProtocolRange {
    /// The highest version both ranges include, or `None` when they don't
    /// overlap or either range is inverted.
    pub fn highest_common(&self, other: &Self) -> Option<u32> {
        if self.min > self.max || other.min > other.max {
            return None;
        }
        let high = self.max.min(other.max);
        (high >= self.min.max(other.min)).then_some(high)
    }
}

/// Discovery data. Advertise only capabilities actually implemented/enabled.
/// Lists deliberately contain strings: unknown optional capabilities/auth modes
/// are ignored by clients, while unknown actions/scopes must never be guessed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederationDiscoveryResponse {
    /// Stable instance identity, not authority to redirect credentials.
    pub instance_id: Uuid,
    pub protocol: FederationProtocolRange,
    pub capabilities: Vec<String>,
    pub auth_modes: Vec<String>,
}

/// POST /api/ws-ticket, authenticated with a linked bearer. The server derives
/// identity/scope from the grant, compares this declared origin to the browser's
/// Origin header and approved grant origin, and applies current admin policy.
/// Never trust this field alone; native clients require an explicit policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WsTicketRequest {
    pub client_origin: String,
}

/// One-use opaque credential for /ws/client, consumed atomically with grant and
/// policy revalidation. Redact query strings in logs. No Debug to avoid accidental
/// credential logging; serialization is for delivery to the requesting client.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsTicketResponse {
    pub ticket: String,
    /// RFC 3339 UTC timestamp.
    pub expires_at: String,
}

/// Start a separate linked-client device flow; return DeviceCodeResponse.
/// Poll with DeviceFlowPollRequest and receive LinkedClientPollResponse.
/// Keeping this separate prevents legacy CLI/mobile issuance from accidentally
/// producing linked grants before their authorization enforcement is installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkedClientEnrollmentRequest {
    /// Requested home origin, not verified client identity. The approval page
    /// must present it as requested and show the actual local account/scopes.
    pub client_origin: String,
    pub requested_scope: TokenScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_label: Option<String>,
}

/// Non-secret metadata, but still privacy-sensitive account/link information.
/// Timestamps are RFC 3339 UTC. The grant expiry bounds all issued token TTLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedClientGrant {
    pub id: Uuid,
    pub user_id: Uuid,
    /// User-visible label for identifying the device when reviewing grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_label: Option<String>,
    /// Display only; never an identity-linking key.
    pub user_email: String,
    /// Approved home/client origin, distinct from the issuing portal origin.
    pub client_origin: String,
    pub scope: TokenScope,
    pub created_at: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedClientDenialReason {
    UserDenied,
    PolicyDisabled,
    OriginNotAllowed,
    ScopeExceedsCap,
    /// New denial reasons remain denials; clients render a generic explanation.
    #[serde(other)]
    Unknown,
}

/// Linked enrollment polling result. No Debug: Complete contains a credential.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LinkedClientPollResponse {
    Pending,
    Complete {
        access_token: String,
        /// Token expiry, which may be earlier than grant expiry.
        expires_at: String,
        /// Canonical issuing portal origin. Compare against the enrolled origin;
        /// never follow it as a credential-bearing redirect based on this alone.
        portal_origin: String,
        instance_id: Uuid,
        grant: Box<LinkedClientGrant>,
    },
    Expired,
    Denied {
        reason: LinkedClientDenialReason,
    },
}

/// List grants belonging to the authenticated local account. Revoke an owned
/// grant with DELETE /api/linked-grants/{id} (no body, 204 success). A linked
/// bearer may revoke ONLY its own grant, including when its scope is read;
/// it cannot enumerate/revoke other grants or manage federation policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedClientGrantsResponse {
    pub grants: Vec<LinkedClientGrant>,
}

/// POST /api/linked-grants/refresh with the current linked bearer and no body.
/// Rotate within the SAME grant/account/client binding and scope, rechecking
/// current policy and expiry. Never turn a linked token into a legacy mobile
/// or proxy token. Expired/revoked grants require enrollment again; successful
/// rotation must not extend grant expiry. No Debug: contains a credential.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedClientRefreshResponse {
    pub access_token: String,
    pub expires_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json, to_value};

    #[test]
    fn explicit_scope_is_required_and_unknown_authority_fails_closed() {
        assert!(TokenScope::Write.permits(TokenScope::Read));
        assert!(TokenScope::Write.permits(TokenScope::Write));
        assert!(TokenScope::Read.permits(TokenScope::Read));
        assert!(!TokenScope::Read.permits(TokenScope::Write));
        for value in [
            json!({"client_origin": "https://home.example"}),
            json!({"client_origin": "https://home.example", "requested_scope": "admin"}),
        ] {
            assert!(from_value::<LinkedClientEnrollmentRequest>(value).is_err());
        }
        assert!(from_value::<FederationPolicy>(json!({"mode": "any"})).is_err());
        assert!(from_value::<FederationPolicy>(json!({"mode": "future"})).is_err());
        assert_eq!(
            to_value(FederationPolicy::default()).unwrap(),
            json!({"mode": "off"})
        );
    }

    #[test]
    fn write_request_and_admin_cap_have_stable_distinct_wire_fields() {
        let request: LinkedClientEnrollmentRequest = from_value(json!({
            "client_origin": "https://home.example", "requested_scope": "write"
        }))
        .unwrap();
        assert_eq!(request.requested_scope, TokenScope::Write);
        let policy: FederationPolicy = from_value(json!({
            "mode": "allowlist", "origins": ["https://home.example"], "max_scope": "read"
        }))
        .unwrap();
        assert_eq!(
            policy,
            FederationPolicy::Allowlist {
                origins: vec!["https://home.example".into()],
                max_scope: TokenScope::Read
            }
        );
        assert!(from_value::<WsTicketRequest>(json!({
            "client_origin": "https://home.example", "scope": "write"
        }))
        .is_err());
    }

    #[test]
    fn protocol_ranges_negotiate_the_highest_shared_version() {
        let r = |min, max| FederationProtocolRange { min, max };
        assert_eq!(r(1, 3).highest_common(&r(2, 5)), Some(3));
        assert_eq!(r(1, 1).highest_common(&FEDERATION_PROTOCOL), Some(1));
        assert_eq!(r(2, 3).highest_common(&r(4, 5)), None);
        assert_eq!(r(3, 1).highest_common(&r(1, 5)), None);
        assert_eq!(r(1, 5).highest_common(&r(4, 2)), None);
    }

    #[test]
    fn discovery_tolerates_unknown_features_but_denial_stays_denied() {
        let response: FederationDiscoveryResponse = from_value(json!({
            "instance_id": Uuid::nil(), "protocol": {"min": 1, "max": 2},
            "capabilities": [CAP_WS_TICKET, "future_feature"],
            "auth_modes": [AUTH_MODE_LINKED_DEVICE_FLOW, "future_auth"]
        }))
        .unwrap();
        assert_eq!(response.capabilities[1], "future_feature");
        let denied: LinkedClientPollResponse = from_value(json!({
            "status": "denied", "reason": "new_admin_policy"
        }))
        .unwrap();
        assert!(matches!(
            denied,
            LinkedClientPollResponse::Denied {
                reason: LinkedClientDenialReason::Unknown
            }
        ));
    }

    #[test]
    fn completed_enrollment_preserves_issuer_account_and_grant_binding() {
        let wire = json!({
            "status": "complete", "access_token": "test-secret",
            "expires_at": "2026-10-12T00:00:00Z",
            "portal_origin": "https://work.example",
            "instance_id": "00000000-0000-0000-0000-000000000001",
            "grant": {
                "id": "00000000-0000-0000-0000-000000000002",
                "user_id": "00000000-0000-0000-0000-000000000003",
                "user_email": "engineer@work.example",
                "device_label": "Personal tablet",
                "client_origin": "https://home.example", "scope": "write",
                "created_at": "2026-10-11T00:00:00Z",
                "expires_at": "2026-11-11T00:00:00Z"
            }
        });
        let parsed: LinkedClientPollResponse = from_value(wire.clone()).unwrap();
        assert_eq!(to_value(parsed).unwrap(), wire);
    }
}
