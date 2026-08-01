//! Resolving a login to a portal user across multiple identity providers
//! (#1535).
//!
//! Identity is keyed by `(provider, subject)` in `user_identities`, never by
//! email — a provider's subject is immutable, whereas an email address can be
//! reassigned to a different human. Email is used for exactly one thing: the
//! account-*linking* rule below.
//!
//! ## The linking rule, and why it is gated on verification
//!
//! Signing in through a *new* provider with the email of an existing account is
//! the normal case for a real person adding a second login. Linking silently is
//! convenient — and, if the new provider does not verify the address, it is an
//! account-takeover primitive: set your unverified email to the victim's, sign
//! in, inherit their sessions. Providers differ here (Google verifies; GitHub
//! will happily report an unverified address), so this module refuses to link on
//! an unverified email and makes the caller state verification explicitly.
//!
//! Consequently [`resolve_user`] has four outcomes, in order:
//!
//! 1. the `(provider, subject)` is known → that user, always;
//! 2. else the email is **verified** and matches an existing user → link a new
//!    identity to them;
//! 3. else the email is **verified** and is new → create the user;
//! 4. else (unverified or absent email, and no known identity) → refuse.
//!
//! Case 4 is a deliberate lockout rather than a silent second account: the
//! alternative — creating a duplicate user holding the same address — was the
//! pre-#1535 behavior and is worse, because the person appears logged in while
//! seeing none of their own sessions.

use diesel::prelude::*;
use tracing::{info, warn};

use crate::db::DbConnection;
use crate::models::{NewUser, NewUserIdentity, User, UserIdentity};

/// Provider key for Google logins, as stored in `user_identities.provider`.
pub const PROVIDER_GOOGLE: &str = "google";
/// Provider key for GitHub logins.
pub const PROVIDER_GITHUB: &str = "github";

/// A verified-or-not identity assertion from a provider's userinfo response.
#[derive(Debug, Clone)]
pub struct ProviderIdentity {
    /// Provider key — one of the `PROVIDER_*` constants.
    pub provider: &'static str,
    /// The provider's immutable id for this person.
    pub subject: String,
    /// Email as asserted by the provider, if any.
    pub email: Option<String>,
    /// Whether the provider states it has *verified* that address. Only a
    /// verified email may link to, or create, an account — see the module docs.
    pub email_verified: bool,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// Why a login could not be resolved to a user.
#[derive(Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// The provider supplied no email, or one it has not verified, and we have
    /// never seen this `(provider, subject)` before.
    UnverifiedEmail,
}

/// Look up the user for an identity, linking or creating as the rule allows.
pub fn resolve_user(
    conn: &mut DbConnection,
    identity: &ProviderIdentity,
) -> Result<Result<User, IdentityError>, diesel::result::Error> {
    use crate::schema::{user_identities, users};

    // 1. Known identity — the only path that does not consult email at all, so
    //    an existing user keeps working even if their provider later stops
    //    returning (or changes) their address.
    let existing: Option<UserIdentity> = user_identities::table
        .filter(user_identities::provider.eq(identity.provider))
        .filter(user_identities::subject.eq(&identity.subject))
        .select(UserIdentity::as_select())
        .first(conn)
        .optional()?;

    if let Some(existing) = existing {
        let user = users::table
            .find(existing.user_id)
            .select(User::as_select())
            .first(conn)?;
        return Ok(Ok(user));
    }

    // Everything below needs a trustworthy address.
    let email = match (&identity.email, identity.email_verified) {
        (Some(email), true) if !email.trim().is_empty() => email.trim().to_string(),
        _ => {
            warn!(
                target: "auth_audit",
                event = "identity_refused",
                provider = identity.provider,
                reason = "unverified_email",
            );
            return Ok(Err(IdentityError::UnverifiedEmail));
        }
    };
    let email_lower = email.to_lowercase();

    // 2. Verified email matches an existing account → link this provider to it.
    let existing_user: Option<User> = users::table
        .filter(
            diesel::dsl::sql::<diesel::sql_types::Bool>("lower(email) = ")
                .bind::<diesel::sql_types::Text, _>(&email_lower),
        )
        .select(User::as_select())
        .first(conn)
        .optional()?;

    let user = match existing_user {
        Some(user) => {
            info!(
                target: "auth_audit",
                event = "identity_linked",
                provider = identity.provider,
                user_id = %user.id,
                user_email = %user.email,
            );
            user
        }
        // 3. New person.
        None => diesel::insert_into(users::table)
            .values(&NewUser {
                email,
                name: identity.name.clone(),
                avatar_url: identity.avatar_url.clone(),
            })
            .get_result::<User>(conn)?,
    };

    diesel::insert_into(user_identities::table)
        .values(&NewUserIdentity {
            user_id: user.id,
            provider: identity.provider.to_string(),
            subject: identity.subject.clone(),
            email: identity.email.clone(),
        })
        .execute(conn)?;

    Ok(Ok(user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Unique per run so tests never collide with dev data or each other; the
    /// email-uniqueness rule makes fixed addresses unusable across runs.
    fn nonce() -> String {
        Uuid::new_v4().to_string()
    }

    /// Delete the users (and their cascading identities) a test created.
    fn cleanup(conn: &mut DbConnection, ids: &[Uuid]) {
        use crate::schema::users;
        let _ = diesel::delete(users::table.filter(users::id.eq_any(ids))).execute(conn);
    }

    fn identity(
        provider: &'static str,
        subject: &str,
        email: &str,
        verified: bool,
    ) -> ProviderIdentity {
        ProviderIdentity {
            provider,
            subject: subject.to_string(),
            email: Some(email.to_string()),
            email_verified: verified,
            name: Some("Test".to_string()),
            avatar_url: None,
        }
    }

    #[test]
    fn creates_a_user_for_a_new_verified_identity() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let conn = &mut pool.get().expect("conn");
        let n = nonce();
        let id = identity(
            PROVIDER_GOOGLE,
            &format!("g-{n}"),
            &format!("new-{n}@example.invalid"),
            true,
        );

        let user = resolve_user(conn, &id).unwrap().unwrap();
        // Signing in again returns the SAME user, not a second one.
        let again = resolve_user(conn, &id).unwrap().unwrap();
        let (uid, same) = (user.id, again.id);
        cleanup(conn, &[uid]);

        assert_eq!(same, uid);
    }

    /// The whole point of keying on subject: a provider changing the address it
    /// reports must not fork the account.
    #[test]
    fn known_subject_wins_even_if_the_email_changed() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let conn = &mut pool.get().expect("conn");
        let n = nonce();
        let subject = format!("g-stable-{n}");

        let user = resolve_user(
            conn,
            &identity(
                PROVIDER_GOOGLE,
                &subject,
                &format!("before-{n}@example.invalid"),
                true,
            ),
        )
        .unwrap()
        .unwrap();
        let same = resolve_user(
            conn,
            &identity(
                PROVIDER_GOOGLE,
                &subject,
                &format!("after-{n}@example.invalid"),
                true,
            ),
        )
        .unwrap()
        .unwrap();
        let (uid, same_id, email) = (user.id, same.id, same.email.clone());
        cleanup(conn, &[uid]);

        assert_eq!(same_id, uid);
        // The user row keeps its original address; we do not chase renames.
        assert_eq!(email, format!("before-{n}@example.invalid"));
    }

    /// A second provider with the same *verified* email is the same human.
    #[test]
    fn links_a_second_provider_on_a_verified_email() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let conn = &mut pool.get().expect("conn");
        let n = nonce();
        let email = format!("shared-{n}@example.invalid");

        let user = resolve_user(
            conn,
            &identity(PROVIDER_GOOGLE, &format!("g-{n}"), &email, true),
        )
        .unwrap()
        .unwrap();
        let linked = resolve_user(
            conn,
            &identity(PROVIDER_GITHUB, &format!("gh-{n}"), &email, true),
        )
        .unwrap()
        .unwrap();
        let (uid, linked_id) = (user.id, linked.id);
        cleanup(conn, &[uid]);

        assert_eq!(linked_id, uid, "should link, not create a second user");
    }

    /// Case-insensitively, too — providers disagree on casing.
    #[test]
    fn linking_is_case_insensitive() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let conn = &mut pool.get().expect("conn");
        let n = nonce();

        let user = resolve_user(
            conn,
            &identity(
                PROVIDER_GOOGLE,
                &format!("g-{n}"),
                &format!("Mixed-{n}@Example.invalid"),
                true,
            ),
        )
        .unwrap()
        .unwrap();
        let linked = resolve_user(
            conn,
            &identity(
                PROVIDER_GITHUB,
                &format!("gh-{n}"),
                &format!("mixed-{n}@example.INVALID"),
                true,
            ),
        )
        .unwrap()
        .unwrap();
        let (uid, linked_id) = (user.id, linked.id);
        cleanup(conn, &[uid]);

        assert_eq!(linked_id, uid);
    }

    /// The takeover case: an unverified address must never reach an account.
    #[test]
    fn refuses_to_link_on_an_unverified_email() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let conn = &mut pool.get().expect("conn");
        let n = nonce();
        let email = format!("victim-{n}@example.invalid");

        let victim = resolve_user(
            conn,
            &identity(PROVIDER_GOOGLE, &format!("g-{n}"), &email, true),
        )
        .unwrap()
        .unwrap();
        let attacker = resolve_user(
            conn,
            &identity(PROVIDER_GITHUB, &format!("gh-{n}"), &email, false),
        )
        .unwrap();

        use crate::schema::user_identities;
        let attached: i64 = user_identities::table
            .filter(user_identities::user_id.eq(victim.id))
            .count()
            .get_result(conn)
            .unwrap();
        cleanup(conn, &[victim.id]);

        assert!(
            matches!(attacker, Err(IdentityError::UnverifiedEmail)),
            "unverified email must not reach an existing account"
        );
        assert_eq!(
            attached, 1,
            "victim must still have only their own identity"
        );
    }

    /// An unverified email must not create an account either — otherwise the
    /// refusal above is trivially bypassed by signing up first.
    #[test]
    fn refuses_to_create_on_an_unverified_email() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let conn = &mut pool.get().expect("conn");
        let n = nonce();
        assert!(matches!(
            resolve_user(
                conn,
                &identity(
                    PROVIDER_GITHUB,
                    &format!("gh-{n}"),
                    &format!("nobody-{n}@example.invalid"),
                    false
                )
            )
            .unwrap(),
            Err(IdentityError::UnverifiedEmail)
        ));
    }

    /// A provider that returns no email at all is treated as unverified.
    #[test]
    fn refuses_when_the_provider_supplies_no_email() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let conn = &mut pool.get().expect("conn");
        let n = nonce();
        let mut no_email = identity(
            PROVIDER_GITHUB,
            &format!("gh-{n}"),
            "x@example.invalid",
            true,
        );
        no_email.email = None;
        assert!(matches!(
            resolve_user(conn, &no_email).unwrap(),
            Err(IdentityError::UnverifiedEmail)
        ));
    }

    /// Distinct providers may legitimately use the same subject string; they
    /// must resolve to different accounts.
    #[test]
    fn same_subject_on_different_providers_is_two_accounts() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let conn = &mut pool.get().expect("conn");
        let n = nonce();
        let subject = format!("shared-subject-{n}");

        let ua = resolve_user(
            conn,
            &identity(
                PROVIDER_GOOGLE,
                &subject,
                &format!("a-{n}@example.invalid"),
                true,
            ),
        )
        .unwrap()
        .unwrap();
        let ub = resolve_user(
            conn,
            &identity(
                PROVIDER_GITHUB,
                &subject,
                &format!("b-{n}@example.invalid"),
                true,
            ),
        )
        .unwrap()
        .unwrap();
        let (a, b) = (ua.id, ub.id);
        cleanup(conn, &[a, b]);

        assert_ne!(a, b);
    }
}
