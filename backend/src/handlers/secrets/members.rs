//! Sharing selectors identify registered provider subjects; grants retain the
//! stable portal UUID even when handles change or another provider is linked.
use super::*;
use crate::models::{User, UserIdentity};
use serde::Deserialize;

#[derive(Debug, PartialEq, Eq)]
enum Selector {
    User(Uuid),
    GoogleEmail(String),
    Email(String),
    Github(String),
}
fn parse_selector(value: &str) -> Result<Selector, AppError> {
    let value = value.trim();
    if value.len() > 320 {
        return Err(AppError::BadRequest(
            "Identity selector exceeds length limit",
        ));
    }
    if let Ok(id) = value.parse::<Uuid>() {
        return Ok(Selector::User(id));
    }
    if let Some(handle) = value
        .strip_prefix("github:")
        .or_else(|| value.strip_suffix("@github"))
    {
        if handle.is_empty()
            || handle.len() > 39
            || handle.starts_with('-')
            || handle.ends_with('-')
            || handle.contains("--")
            || !handle
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(AppError::BadRequest("Invalid GitHub handle"));
        }
        return Ok(Selector::Github(handle.to_ascii_lowercase()));
    }
    let (email, google) = value
        .strip_prefix("google:")
        .map_or((value, false), |email| (email, true));
    let parts: Vec<_> = email.split('@').collect();
    if parts.len() != 2
        || parts.iter().any(|part| part.is_empty())
        || email
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == ':')
    {
        return Err(AppError::BadRequest("Use google:EMAIL, HANDLE@github, github:HANDLE, a verified email, or a portal user UUID"));
    }
    Ok(if google {
        Selector::GoogleEmail(email.to_lowercase())
    } else {
        Selector::Email(email.to_lowercase())
    })
}

fn members(conn: &mut crate::db::DbConnection) -> Result<Vec<SecretMember>, AppError> {
    use crate::schema::{user_identities, users};
    let users = users::table
        .filter(users::disabled.eq(false))
        .select(User::as_select())
        .load(conn)?;
    let identities = user_identities::table
        .inner_join(users::table)
        .filter(users::disabled.eq(false))
        .select(UserIdentity::as_select())
        .load::<UserIdentity>(conn)?;
    let mut by_user = std::collections::HashMap::<Uuid, Vec<SecretIdentityMetadata>>::new();
    for identity in identities {
        by_user
            .entry(identity.user_id)
            .or_default()
            .push(SecretIdentityMetadata {
                provider: identity.provider,
                subject: identity.subject,
                handle: identity.handle,
                email: identity.email,
                email_verified: identity.email_verified,
            });
    }
    Ok(users
        .into_iter()
        .map(|user| SecretMember {
            user_id: user.id,
            display_name: user.name.unwrap_or_else(|| user.email.clone()),
            email: user.email,
            identities: by_user.remove(&user.id).unwrap_or_default(),
        })
        .collect())
}

#[derive(Deserialize)]
struct GithubUser {
    id: u64,
    login: String,
    #[serde(rename = "type")]
    account_type: String,
}
/// Public identity lookup deliberately sends no portal or OAuth bearer token.
/// No redirects: an old alias must not silently redirect a sharing selector.
async fn github_user_at(
    client: &reqwest::Client,
    base: &str,
    handle: &str,
) -> Result<GithubUser, AppError> {
    let mut response = client
        .get(format!("{base}/users/{handle}"))
        .header(reqwest::header::USER_AGENT, "agent-portal-secret-sharing")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|_| AppError::ServiceUnavailable("GitHub identity verification unavailable"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AppError::NotFound("GitHub user not found"));
    }
    if !response.status().is_success() {
        return Err(AppError::ServiceUnavailable(
            "GitHub identity verification unavailable; no cached handle fallback",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AppError::ServiceUnavailable("GitHub identity response unavailable"))?
    {
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err(AppError::ServiceUnavailable(
                "GitHub identity response exceeds limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let user: GithubUser = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::ServiceUnavailable("Invalid GitHub identity response"))?;
    if user.account_type != "User" || !user.login.eq_ignore_ascii_case(handle) || user.id == 0 {
        return Err(AppError::Conflict("GitHub handle changed or is not a human account; use a current handle or portal user UUID"));
    }
    Ok(user)
}
fn select_member(
    members: &[SecretMember],
    selector: &Selector,
    github: Option<&GithubUser>,
) -> Result<SecretMember, AppError> {
    if let Selector::Github(handle) = selector {
        let github = github.ok_or(AppError::ServiceUnavailable(
            "GitHub identity verification unavailable",
        ))?;
        let subject = github.id.to_string();
        // A name reassigned since a registered user's last login is dangerous:
        // demand the immutable portal UUID rather than selecting a new person.
        if members
            .iter()
            .flat_map(|member| &member.identities)
            .any(|identity| {
                identity.provider == "github"
                    && identity
                        .handle
                        .as_ref()
                        .is_some_and(|cached| cached.eq_ignore_ascii_case(handle))
                    && identity.subject != subject
            })
        {
            return Err(AppError::Conflict(
                "GitHub handle was reassigned; select the intended member by portal user UUID",
            ));
        }
    }
    let matches: Vec<_> = members
        .iter()
        .filter(|member| match selector {
            Selector::User(id) => member.user_id == *id,
            Selector::GoogleEmail(email) | Selector::Email(email) => {
                member.identities.iter().any(|identity| {
                    identity.email_verified
                        && identity
                            .email
                            .as_ref()
                            .is_some_and(|asserted| asserted.trim().to_lowercase() == *email)
                        && (!matches!(selector, Selector::GoogleEmail(_))
                            || identity.provider == "google")
                })
            }
            Selector::Github(_) => github.is_some_and(|user| {
                member.identities.iter().any(|identity| {
                    identity.provider == "github" && identity.subject == user.id.to_string()
                })
            }),
        })
        .collect();
    match matches.as_slice() {
        [member] => Ok((*member).clone()),
        [] => Err(AppError::NotFound("No active registered member matches this verified identity; never matched by display name")),
        _ => Err(AppError::Conflict("Identity selector is ambiguous; specify its provider or the portal user UUID")),
    }
}

/// An open-signup portal is not a trusted organization directory. Exact
/// resolution is gated too, otherwise it becomes an account-enumeration oracle.
/// Login allowlist exceptions and legacy profile emails do not establish domain
/// membership: require a verified provider assertion for this caller.
fn require_directory(
    state: &AppState,
    conn: &mut crate::db::DbConnection,
    principal: Principal,
) -> Result<(), AppError> {
    if !principal.metadata {
        return Err(AppError::Forbidden);
    }
    let domain = state
        .allowed_email_domain
        .as_deref()
        .filter(|domain| {
            !domain.is_empty() && !domain.contains('@') && !domain.chars().any(char::is_whitespace)
        })
        .ok_or(AppError::Forbidden)?;
    store::active_user(conn, principal.user)?;
    use crate::schema::user_identities;
    let emails = user_identities::table
        .filter(user_identities::user_id.eq(principal.user))
        .filter(user_identities::email_verified.eq(true))
        .select(user_identities::email)
        .load::<Option<String>>(conn)?;
    if emails.into_iter().flatten().any(|email| {
        email
            .split_once('@')
            .is_some_and(|(local, asserted_domain)| {
                !local.is_empty() && asserted_domain.eq_ignore_ascii_case(domain)
            })
    }) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

pub(super) async fn list(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
) -> Result<Json<SecretMembersResponse>, AppError> {
    let mut conn = state.conn()?;
    require_directory(&state, &mut conn, principal)?;
    Ok(Json(SecretMembersResponse {
        members: members(&mut conn)?,
    }))
}
pub(super) async fn resolve(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    Json(request): Json<SecretMemberResolveRequest>,
) -> Result<Json<SecretMember>, AppError> {
    // Authorize before parsing a selector or making external requests.
    {
        let mut conn = state.conn()?;
        require_directory(&state, &mut conn, principal)?;
    }
    let selector = parse_selector(&request.selector)?;
    // Do not hold a DB connection during public-provider network I/O.
    let github = if let Selector::Github(handle) = &selector {
        static GITHUB_LOOKUPS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
        let _lookup = GITHUB_LOOKUPS.try_acquire().map_err(|_| {
            AppError::ServiceUnavailable(
                "GitHub identity verification busy; retry or select a portal user UUID",
            )
        })?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|_| {
                AppError::ServiceUnavailable("GitHub identity verification unavailable")
            })?;
        Some(github_user_at(&client, "https://api.github.com", handle).await?)
    } else {
        None
    };
    let mut conn = state.conn()?;
    require_directory(&state, &mut conn, principal)?;
    Ok(Json(select_member(
        &members(&mut conn)?,
        &selector,
        github.as_ref(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn member(
        provider: &str,
        subject: &str,
        handle: Option<&str>,
        email: &str,
        verified: bool,
    ) -> SecretMember {
        SecretMember {
            user_id: Uuid::new_v4(),
            display_name: "Same Display Name".into(),
            email: email.into(),
            identities: vec![SecretIdentityMetadata {
                provider: provider.into(),
                subject: subject.into(),
                handle: handle.map(Into::into),
                email: Some(email.into()),
                email_verified: verified,
            }],
        }
    }
    fn github(id: u64, login: &str) -> GithubUser {
        GithubUser {
            id,
            login: login.into(),
            account_type: "User".into(),
        }
    }
    #[test]
    fn selectors_require_a_verified_identity_not_a_display_name() {
        assert!(parse_selector("Same Display Name").is_err());
        assert!(parse_selector("github:a/b").is_err());
        assert!(parse_selector("github:../users").is_err());
        assert!(parse_selector("unknown:a@example.test").is_err());
        assert_eq!(
            parse_selector("meawoppl@github").unwrap(),
            Selector::Github("meawoppl".into())
        );
        assert_eq!(
            parse_selector("github:Meawoppl").unwrap(),
            Selector::Github("meawoppl".into())
        );
        assert_eq!(
            parse_selector("google:Matt@cosmicfrontier.org").unwrap(),
            Selector::GoogleEmail("matt@cosmicfrontier.org".into())
        );
    }
    #[test]
    fn provider_collisions_and_ambiguous_email_fail_closed() {
        let google = member("google", "42", None, "same@example.test", true);
        let github = member("github", "42", Some("same"), "same@example.test", true);
        let members = [google.clone(), github.clone()];
        assert!(
            select_member(&members, &Selector::Email("same@example.test".into()), None).is_err()
        );
        assert_eq!(
            select_member(
                &members,
                &Selector::GoogleEmail("same@example.test".into()),
                None
            )
            .unwrap()
            .user_id,
            google.user_id
        );
        assert_eq!(
            select_member(
                &members,
                &Selector::Github("same".into()),
                Some(&self::github(42, "same"))
            )
            .unwrap()
            .user_id,
            github.user_id
        );
    }
    #[test]
    fn unverified_assertions_and_legacy_profile_emails_are_not_selectors() {
        let user = member("google", "42", None, "unverified@example.test", false);
        assert!(select_member(
            std::slice::from_ref(&user),
            &Selector::Email(user.email.clone()),
            None
        )
        .is_err());
        assert!(select_member(
            std::slice::from_ref(&user),
            &Selector::GoogleEmail(user.email.clone()),
            None
        )
        .is_err());
        assert_eq!(
            select_member(
                std::slice::from_ref(&user),
                &Selector::User(user.user_id),
                None
            )
            .unwrap()
            .user_id,
            user.user_id
        );
    }
    #[test]
    fn renamed_handles_use_current_provider_id_and_reassigned_handles_are_rejected() {
        let old = member("github", "42", Some("old-name"), "owner@example.test", true);
        let users = [old.clone()];
        assert_eq!(
            select_member(
                &users,
                &Selector::Github("new-name".into()),
                Some(&github(42, "new-name"))
            )
            .unwrap()
            .user_id,
            old.user_id
        );
        assert!(select_member(
            &users,
            &Selector::Github("old-name".into()),
            Some(&github(99, "old-name"))
        )
        .is_err());
        let replacement = member(
            "github",
            "99",
            Some("old-name"),
            "replacement@example.test",
            true,
        );
        assert!(select_member(
            &[old, replacement],
            &Selector::Github("old-name".into()),
            Some(&github(99, "old-name"))
        )
        .is_err());
        assert!(select_member(&users, &Selector::Github("new-name".into()), None).is_err());
        assert!(select_member(
            &users,
            &Selector::Github("unregistered".into()),
            Some(&github(1000, "unregistered"))
        )
        .is_err());
    }
    #[test]
    fn linked_providers_with_same_verified_email_are_one_stable_member() {
        let mut user = member("google", "g42", None, "linked@example.test", true);
        user.identities.push(SecretIdentityMetadata {
            provider: "github".into(),
            subject: "42".into(),
            handle: Some("linked".into()),
            email: Some(user.email.clone()),
            email_verified: true,
        });
        assert_eq!(
            select_member(
                std::slice::from_ref(&user),
                &Selector::Email(user.email.clone()),
                None
            )
            .unwrap()
            .user_id,
            user.user_id
        );
        assert_eq!(
            select_member(
                std::slice::from_ref(&user),
                &Selector::Github("linked".into()),
                Some(&github(42, "linked"))
            )
            .unwrap()
            .user_id,
            user.user_id
        );
    }
    #[test]
    fn member_directory_uses_persisted_assertions_and_excludes_disabled_users() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let conn = &mut pool.get().unwrap();
        let user = crate::test_support::insert_user(conn, "provider-directory");
        use crate::schema::{user_identities, users};
        diesel::insert_into(user_identities::table)
            .values(&crate::models::NewUserIdentity {
                user_id: user.id,
                provider: "github".into(),
                subject: Uuid::new_v4().to_string(),
                email: Some(user.email.clone()),
                handle: Some("asserted-handle".into()),
                email_verified: true,
                last_seen_at: Some(chrono::Utc::now().naive_utc()),
            })
            .execute(conn)
            .unwrap();
        let directory = members(conn).unwrap();
        let saved = directory
            .iter()
            .find(|member| member.user_id == user.id)
            .unwrap();
        assert_eq!(saved.identities.len(), 1);
        assert_eq!(saved.identities[0].provider, "github");
        assert_eq!(
            saved.identities[0].handle.as_deref(),
            Some("asserted-handle")
        );
        assert!(saved.identities[0].email_verified);
        assert_eq!(
            select_member(&directory, &Selector::Email(user.email.clone()), None)
                .unwrap()
                .user_id,
            user.id
        );
        diesel::update(users::table.find(user.id))
            .set(users::disabled.eq(true))
            .execute(conn)
            .unwrap();
        assert!(!members(conn)
            .unwrap()
            .iter()
            .any(|member| member.user_id == user.id));
        diesel::delete(users::table.find(user.id))
            .execute(conn)
            .unwrap();
    }
    #[tokio::test]
    async fn directory_routes_require_a_verified_locked_domain_for_browser_and_cli() {
        use crate::handlers::secrets::vault_tests::{credential, Database};
        use crate::schema::{user_identities, users};
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let Some(db) = Database::new() else {
            return;
        };
        let user = crate::test_support::insert_user(&mut db.pool.get().unwrap(), "directory-gate");
        let identity = crate::models::NewUserIdentity {
            user_id: user.id,
            provider: "google".into(),
            subject: Uuid::new_v4().to_string(),
            email: Some("member@company.test".into()),
            handle: None,
            email_verified: true,
            last_seen_at: None,
        };
        diesel::insert_into(user_identities::table)
            .values(&identity)
            .execute(&mut db.pool.get().unwrap())
            .unwrap();
        // Profile email is deliberately trusted-looking in every case. Only
        // provider assertions, not this legacy field or login exceptions, count.
        diesel::update(users::table.find(user.id))
            .set(users::email.eq("profile@company.test"))
            .execute(&mut db.pool.get().unwrap())
            .unwrap();
        for (domain, email, verified, allowed) in [
            (None, "member@company.test", true, false),
            (Some(""), "member@company.test", true, false),
            (Some("company.test"), "member@company.test", false, false),
            (Some("company.test"), "rando@gmail.com", true, false),
            (Some("company.test"), "member@evilcompany.test", true, false),
            (
                Some("company.test"),
                "member@company.test.evil",
                true,
                false,
            ),
            (Some("company.test"), "member@company.test", true, true),
            (Some("COMPANY.TEST"), "member@Company.Test", true, true),
        ] {
            let mut state = crate::test_support::test_app_state(db.pool.clone());
            state.allowed_email_domain = domain.map(str::to_owned);
            state.allowed_emails = Some(vec!["rando@gmail.com".into()]);
            let state = Arc::new(state);
            diesel::update(user_identities::table.filter(user_identities::user_id.eq(user.id)))
                .set((
                    user_identities::email.eq(Some(email)),
                    user_identities::email_verified.eq(verified),
                ))
                .execute(&mut db.pool.get().unwrap())
                .unwrap();
            let token = credential(&state, user.id, "directory-gate");
            let cookies = tower_cookies::Cookies::default();
            cookies
                .signed(&state.cookie_key)
                .add(tower_cookies::Cookie::new(
                    shared::protocol::SESSION_COOKIE_NAME,
                    user.id.to_string(),
                ));
            let cookie = cookies
                .list()
                .iter()
                .map(|c| format!("{}={}", c.name(), c.value()))
                .collect::<Vec<_>>()
                .join("; ");
            let router = Router::new()
                .route("/members", get(list))
                .route("/resolve", post(resolve))
                .layer(tower_cookies::CookieManagerLayer::new())
                .with_state(state);
            for browser in [true, false] {
                for (method, path, body) in [
                    ("GET", "/members", String::new()),
                    (
                        "POST",
                        "/resolve",
                        serde_json::json!({"selector":user.id.to_string()}).to_string(),
                    ),
                ] {
                    let mut request = Request::builder()
                        .method(method)
                        .uri(path)
                        .header("content-type", "application/json");
                    request = if browser {
                        request.header("cookie", &cookie)
                    } else {
                        request.header("authorization", format!("Bearer {token}"))
                    };
                    let response = router
                        .clone()
                        .oneshot(request.body(axum::body::Body::from(body)).unwrap())
                        .await
                        .unwrap();
                    assert_eq!(response.status(), if allowed {StatusCode::OK} else {StatusCode::FORBIDDEN}, "domain={domain:?}, email={email}, verified={verified}, browser={browser}, path={path}");
                    if !allowed {
                        let body = axum::body::to_bytes(response.into_body(), 8192)
                            .await
                            .unwrap();
                        assert!(!String::from_utf8_lossy(&body).contains("profile@company.test"));
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn github_lookup_checks_live_handle_and_never_sends_portal_bearer() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0u8; 1024];
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0 && bytes.len() < 16384);
                bytes.extend_from_slice(&chunk[..n]);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let body = r#"{"id":42,"login":"new-name","type":"User"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            String::from_utf8(bytes).unwrap()
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        assert!(
            github_user_at(&client, &format!("http://{address}"), "old-name")
                .await
                .is_err()
        );
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /users/old-name "));
        assert!(!request.contains("authorization:"));
    }
}
