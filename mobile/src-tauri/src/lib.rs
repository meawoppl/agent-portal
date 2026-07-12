#[cfg(any(target_os = "android", target_os = "ios"))]
#[tauri::mobile_entry_point]
pub fn run() {
    if let Err(err) = mobile::run() {
        eprintln!("failed to run Agent Portal mobile shell: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn run() {}

#[cfg(any(target_os = "android", target_os = "ios"))]
mod mobile {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Duration;

    #[cfg(target_os = "android")]
    use agent_portal_mobile_status_notification::{
        StatusNotificationExt, StatusNotificationLine, StatusNotificationPayload,
    };
    #[cfg(target_os = "android")]
    use shared::api::{AgentSessionInfo, AgentSessionsResponse};
    use shared::api::{
        DeviceClientType, DeviceCodeRequest, DeviceFlowPollRequest, TokenRefreshResponse,
    };
    use shared::DevicePollResponse;
    use tauri::{Manager, Url};
    use tauri_plugin_deep_link::DeepLinkExt;
    use tauri_plugin_opener::OpenerExt;
    use tauri_plugin_store::StoreExt;

    const DEFAULT_SHELL_URL: &str = "https://txcl.io";
    const MAIN_WINDOW_LABEL: &str = "main";
    const AUTH_STORE_PATH: &str = "mobile-auth.json";
    const AUTH_TOKEN_KEY: &str = "auth_token";
    #[cfg(target_os = "android")]
    const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(45);
    #[cfg(target_os = "android")]
    const MAX_STATUS_SESSIONS: usize = 5;

    pub fn run() -> tauri::Result<()> {
        let mut builder = tauri::Builder::default()
            .plugin(tauri_plugin_deep_link::init())
            .plugin(tauri_plugin_opener::init())
            .plugin(tauri_plugin_store::Builder::default().build());
        #[cfg(target_os = "android")]
        {
            builder = builder.plugin(agent_portal_mobile_status_notification::init());
        }

        builder
            .setup(|app| {
                let app_handle = app.handle().clone();
                let auth_in_progress = Arc::new(AtomicBool::new(false));

                let mut startup_destination = None;
                match app.deep_link().get_current() {
                    Ok(Some(urls)) => {
                        startup_destination = first_allowed_shell_url(&urls);
                        route_deep_links(&app_handle, urls);
                    }
                    Ok(None) => {
                        if let Some(url) = shell_url_override() {
                            navigate_main_window(&app_handle, url);
                        }
                    }
                    Err(err) => eprintln!("failed to read startup deep link: {err}"),
                }

                run_auth_handoff(&app_handle, auth_in_progress.clone(), startup_destination);

                let app_handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    route_deep_links(&app_handle, event.urls());
                });

                if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                    let app_handle = app.handle().clone();
                    window.on_window_event(move |event| {
                        if matches!(event, tauri::WindowEvent::Focused(true)) {
                            run_auth_handoff(&app_handle, auth_in_progress.clone(), None);
                        }
                    });
                }

                start_status_notification_polling(app.handle().clone());

                Ok(())
            })
            .run(tauri::generate_context!())
    }

    fn run_auth_handoff<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        in_progress: Arc<AtomicBool>,
        destination_url: Option<Url>,
    ) {
        if in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(err) = ensure_mobile_auth(&app, destination_url).await {
                eprintln!("mobile auth handoff failed: {err}");
            }
            in_progress.store(false, Ordering::Release);
        });
    }

    async fn ensure_mobile_auth<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        destination_url: Option<Url>,
    ) -> Result<(), String> {
        let shell_url = shell_base_url();
        let store = app
            .store(AUTH_STORE_PATH)
            .map_err(|err| format!("failed to open auth store: {err}"))?;

        if let Some(token) = stored_auth_token(&store) {
            match refresh_mobile_token(&shell_url, &token).await? {
                RefreshDecision::UseExisting => {
                    login_webview_with_token(app, &token, destination_url).await?;
                    update_status_notification_once(app, &shell_url, &token).await;
                    return Ok(());
                }
                RefreshDecision::UseReplacement(token) => {
                    save_auth_token(&store, &token)?;
                    login_webview_with_token(app, &token, destination_url).await?;
                    update_status_notification_once(app, &shell_url, &token).await;
                    return Ok(());
                }
                RefreshDecision::TokenRejected => {
                    store.delete(AUTH_TOKEN_KEY);
                    store
                        .save()
                        .map_err(|err| format!("failed to clear auth token: {err}"))?;
                }
            }
        }

        let token = run_device_flow(app, &shell_url).await?;
        save_auth_token(&store, &token)?;
        login_webview_with_token(app, &token, destination_url).await?;
        update_status_notification_once(app, &shell_url, &token).await;
        Ok(())
    }

    enum RefreshDecision {
        UseExisting,
        UseReplacement(String),
        TokenRejected,
    }

    async fn refresh_mobile_token(shell_url: &Url, token: &str) -> Result<RefreshDecision, String> {
        let url = shell_url
            .join("/api/auth/refresh")
            .map_err(|err| format!("failed to build refresh URL: {err}"))?;
        let response = reqwest::Client::new()
            .post(url.as_str())
            .bearer_auth(token)
            .send()
            .await
            .map_err(|err| format!("refresh request failed: {err}"))?;

        match response.status() {
            reqwest::StatusCode::OK => {
                let body = response
                    .json::<TokenRefreshResponse>()
                    .await
                    .map_err(|err| format!("refresh response was invalid: {err}"))?;
                Ok(match body.auth_token {
                    Some(token) => RefreshDecision::UseReplacement(token),
                    None => RefreshDecision::UseExisting,
                })
            }
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                Ok(RefreshDecision::TokenRejected)
            }
            status => Err(format!("refresh request returned {status}")),
        }
    }

    async fn run_device_flow<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        shell_url: &Url,
    ) -> Result<String, String> {
        let code = create_mobile_device_code(shell_url).await?;
        let verification_url = device_verification_url(&code.verification_uri, &code.user_code)?;
        app.opener()
            .open_url(verification_url.as_str(), None::<&str>)
            .map_err(|err| format!("failed to open device verification URL: {err}"))?;

        let interval = code.interval.max(5);
        let expires_at = std::time::Instant::now() + Duration::from_secs(code.expires_in);
        let poll_url = shell_url
            .join("/api/auth/device/poll")
            .map_err(|err| format!("failed to build poll URL: {err}"))?;
        let client = reqwest::Client::new();

        loop {
            if std::time::Instant::now() >= expires_at {
                return Err("device authorization expired".to_string());
            }

            tokio::time::sleep(Duration::from_secs(interval)).await;
            let request = DeviceFlowPollRequest {
                device_code: code.device_code.clone(),
            };
            let response = client
                .post(poll_url.as_str())
                .json(&request)
                .send()
                .await
                .map_err(|err| format!("device poll failed: {err}"))?;

            if !response.status().is_success() {
                return Err(format!("device poll returned {}", response.status()));
            }

            match response
                .json::<DevicePollResponse>()
                .await
                .map_err(|err| format!("device poll response was invalid: {err}"))?
            {
                DevicePollResponse::Pending => {}
                DevicePollResponse::Complete { access_token, .. } => return Ok(access_token),
                DevicePollResponse::Expired => {
                    return Err("device authorization expired".to_string())
                }
                DevicePollResponse::Denied => return Err("device authorization denied".to_string()),
            }
        }
    }

    async fn create_mobile_device_code(
        shell_url: &Url,
    ) -> Result<shared::api::DeviceCodeResponse, String> {
        let url = shell_url
            .join("/api/auth/device/code")
            .map_err(|err| format!("failed to build device-code URL: {err}"))?;
        let request = DeviceCodeRequest {
            hostname: Some("Agent Portal mobile".to_string()),
            working_directory: None,
            client_type: DeviceClientType::Mobile,
        };

        let response = reqwest::Client::new()
            .post(url.as_str())
            .json(&request)
            .send()
            .await
            .map_err(|err| format!("device-code request failed: {err}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "device-code request returned {}",
                response.status()
            ));
        }
        response
            .json::<shared::api::DeviceCodeResponse>()
            .await
            .map_err(|err| format!("device-code response was invalid: {err}"))
    }

    fn device_verification_url(verification_uri: &str, user_code: &str) -> Result<Url, String> {
        let mut url = Url::parse(verification_uri)
            .map_err(|err| format!("device verification URI was invalid: {err}"))?;
        url.query_pairs_mut().append_pair("user_code", user_code);
        Ok(url)
    }

    async fn login_webview_with_token<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        token: &str,
        preferred_destination_url: Option<Url>,
    ) -> Result<(), String> {
        let window = app
            .get_webview_window(MAIN_WINDOW_LABEL)
            .ok_or_else(|| "Agent Portal shell window is not available".to_string())?;
        let shell_url = shell_base_url();
        let mut destination_url = shell_url
            .join("/dashboard")
            .map_err(|err| format!("failed to build dashboard URL: {err}"))?;
        let has_preferred_destination = preferred_destination_url.is_some();
        if let Some(preferred_destination_url) =
            preferred_destination_url.filter(|url| same_origin(url, shell_url.as_str()))
        {
            destination_url = preferred_destination_url;
        }
        match window.url() {
            Ok(current_url) if same_origin(&current_url, shell_url.as_str()) => {
                if !has_preferred_destination {
                    destination_url = current_url;
                }
            }
            Ok(_) | Err(_) => {
                window
                    .navigate(shell_url.clone())
                    .map_err(|err| format!("failed to navigate Agent Portal shell: {err}"))?;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        let token = serde_json::to_string(token)
            .map_err(|err| format!("failed to encode auth token for WebView: {err}"))?;
        let token_login_url = shell_url
            .join("/api/auth/token-login")
            .map_err(|err| format!("failed to build token-login URL: {err}"))?;
        let token_login_url = serde_json::to_string(token_login_url.as_str())
            .map_err(|err| format!("failed to encode token-login URL: {err}"))?;
        let destination_url = serde_json::to_string(destination_url.as_str())
            .map_err(|err| format!("failed to encode dashboard URL: {err}"))?;
        let script = format!(
            r#"(async () => {{
                try {{
                    const response = await fetch({token_login_url}, {{
                        method: "POST",
                        headers: {{ "Authorization": "Bearer " + {token} }},
                        credentials: "include"
                    }});
                    if (response.ok) {{
                        window.location.assign({destination_url});
                    }} else {{
                        console.error("mobile token-login failed", response.status);
                    }}
                }} catch (error) {{
                    console.error("mobile token-login failed", error);
                }}
            }})()"#
        );
        window
            .eval(&script)
            .map_err(|err| format!("failed to run token-login in WebView: {err}"))
    }

    async fn update_status_notification_once<R: tauri::Runtime>(
        #[allow(unused_variables)] app: &tauri::AppHandle<R>,
        #[allow(unused_variables)] shell_url: &Url,
        #[allow(unused_variables)] token: &str,
    ) {
        #[cfg(target_os = "android")]
        if let Err(err) = refresh_status_notification(app, shell_url, token).await {
            eprintln!("failed to update Android status notification: {err}");
        }
    }

    #[cfg(target_os = "android")]
    fn start_status_notification_polling<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
        tauri::async_runtime::spawn(async move {
            let client = reqwest::Client::new();
            let mut interval = tokio::time::interval(STATUS_POLL_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;
                let shell_url = shell_base_url();
                let token = match app
                    .store(AUTH_STORE_PATH)
                    .ok()
                    .and_then(|store| stored_auth_token(&store))
                {
                    Some(token) => token,
                    None => {
                        if let Err(err) = app.status_notification().clear() {
                            eprintln!("failed to clear Android status notification: {err}");
                        }
                        continue;
                    }
                };

                if let Err(err) =
                    refresh_status_notification_with_client(&app, &client, &shell_url, &token).await
                {
                    eprintln!("failed to refresh Android status notification: {err}");
                }
            }
        });
    }

    #[cfg(not(target_os = "android"))]
    fn start_status_notification_polling<R: tauri::Runtime>(_app: tauri::AppHandle<R>) {}

    #[cfg(target_os = "android")]
    async fn refresh_status_notification<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        shell_url: &Url,
        token: &str,
    ) -> Result<(), String> {
        let client = reqwest::Client::new();
        refresh_status_notification_with_client(app, &client, shell_url, token).await
    }

    #[cfg(target_os = "android")]
    async fn refresh_status_notification_with_client<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        client: &reqwest::Client,
        shell_url: &Url,
        token: &str,
    ) -> Result<(), String> {
        let url = shell_url
            .join("/api/agent/sessions")
            .map_err(|err| format!("failed to build sessions URL: {err}"))?;
        let response = client
            .get(url.as_str())
            .bearer_auth(token)
            .send()
            .await
            .map_err(|err| format!("status sessions request failed: {err}"))?;

        match response.status() {
            reqwest::StatusCode::OK => {
                let body = response
                    .json::<AgentSessionsResponse>()
                    .await
                    .map_err(|err| format!("status sessions response was invalid: {err}"))?;
                let lines = status_notification_lines(shell_url, body.sessions);
                if lines.is_empty() {
                    app.status_notification()
                        .clear()
                        .map_err(|err| format!("failed to clear notification: {err}"))?;
                    return Ok(());
                }
                let dashboard_url = shell_url
                    .join("/dashboard")
                    .map_err(|err| format!("failed to build dashboard URL: {err}"))?;
                let sessions_json = serde_json::to_string(&lines)
                    .map_err(|err| format!("failed to encode notification sessions: {err}"))?;
                let summary = status_notification_summary(&lines);
                app.status_notification()
                    .show(StatusNotificationPayload {
                        title: "Agent Portal sessions".to_string(),
                        summary,
                        dashboard_url: dashboard_url.to_string(),
                        sessions_json,
                    })
                    .map_err(|err| format!("failed to show notification: {err}"))?;
                Ok(())
            }
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                app.status_notification()
                    .clear()
                    .map_err(|err| format!("failed to clear notification: {err}"))?;
                Ok(())
            }
            status => Err(format!("status sessions request returned {status}")),
        }
    }

    #[cfg(target_os = "android")]
    fn status_notification_lines(
        shell_url: &Url,
        sessions: Vec<AgentSessionInfo>,
    ) -> Vec<StatusNotificationLine> {
        sessions
            .into_iter()
            .filter(should_show_status_session)
            .filter_map(|session| {
                let url = shell_url
                    .join(&format!("/dashboard?session={}", session.id))
                    .ok()?;
                let name = compact_session_name(&session.session_name);
                let state = status_state(&session);
                Some(StatusNotificationLine {
                    session_id: session.id.to_string(),
                    name,
                    state,
                    url: url.to_string(),
                })
            })
            .take(MAX_STATUS_SESSIONS)
            .collect()
    }

    #[cfg(target_os = "android")]
    fn should_show_status_session(session: &AgentSessionInfo) -> bool {
        let status = session.status.to_ascii_lowercase();
        session.awaiting_permission
            || status.contains("active")
            || status.contains("working")
            || status.contains("running")
            || status.contains("disconnect")
    }

    #[cfg(target_os = "android")]
    fn status_state(session: &AgentSessionInfo) -> String {
        if session.awaiting_permission {
            return "awaiting input".to_string();
        }
        let status = session.status.to_ascii_lowercase();
        if status.contains("disconnect") {
            "disconnected".to_string()
        } else {
            "working".to_string()
        }
    }

    #[cfg(target_os = "android")]
    fn compact_session_name(name: &str) -> String {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return "Session".to_string();
        }
        const MAX_CHARS: usize = 32;
        if trimmed.chars().count() <= MAX_CHARS {
            return trimmed.to_string();
        }
        let mut compact = trimmed.chars().take(MAX_CHARS - 1).collect::<String>();
        compact.push_str("...");
        compact
    }

    #[cfg(target_os = "android")]
    fn status_notification_summary(lines: &[StatusNotificationLine]) -> String {
        let awaiting = lines
            .iter()
            .filter(|line| line.state == "awaiting input")
            .count();
        let disconnected = lines
            .iter()
            .filter(|line| line.state == "disconnected")
            .count();
        if awaiting > 0 {
            format!("{awaiting} awaiting input")
        } else if disconnected > 0 {
            format!("{disconnected} disconnected")
        } else {
            format!("{} working", lines.len())
        }
    }

    fn first_allowed_shell_url(urls: &[Url]) -> Option<Url> {
        urls.iter().find(|url| is_allowed_shell_url(url)).cloned()
    }

    fn stored_auth_token<R: tauri::Runtime>(
        store: &tauri_plugin_store::Store<R>,
    ) -> Option<String> {
        store
            .get(AUTH_TOKEN_KEY)
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
    }

    fn save_auth_token<R: tauri::Runtime>(
        store: &tauri_plugin_store::Store<R>,
        token: &str,
    ) -> Result<(), String> {
        store.set(AUTH_TOKEN_KEY, token);
        store
            .save()
            .map_err(|err| format!("failed to save auth token: {err}"))
    }

    fn route_deep_links<R: tauri::Runtime>(app: &tauri::AppHandle<R>, urls: Vec<Url>) {
        for url in urls {
            if is_allowed_shell_url(&url) {
                navigate_main_window(app, url);
            } else {
                eprintln!("ignoring unexpected Agent Portal deep link: {url}");
            }
        }
    }

    fn shell_base_url() -> Url {
        shell_url_override().unwrap_or_else(|| match Url::parse(DEFAULT_SHELL_URL) {
            Ok(url) => url,
            Err(err) => panic!("invalid built-in shell URL {DEFAULT_SHELL_URL}: {err}"),
        })
    }

    fn navigate_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>, url: Url) {
        match app.get_webview_window(MAIN_WINDOW_LABEL) {
            Some(window) => {
                if let Err(err) = window.navigate(url) {
                    eprintln!("failed to navigate Agent Portal shell: {err}");
                }
            }
            None => eprintln!("Agent Portal shell window is not available"),
        }
    }

    fn shell_url_override() -> Option<Url> {
        let raw = option_env!("PORTAL_SHELL_URL")?;
        match Url::parse(raw) {
            Ok(url) if is_http_url(&url) => Some(url),
            Ok(url) => {
                eprintln!("ignoring PORTAL_SHELL_URL with unsupported scheme: {url}");
                None
            }
            Err(err) => {
                eprintln!("ignoring invalid PORTAL_SHELL_URL: {err}");
                None
            }
        }
    }

    fn is_allowed_shell_url(url: &Url) -> bool {
        is_http_url(url)
            && (same_origin(url, DEFAULT_SHELL_URL)
                || option_env!("PORTAL_SHELL_URL")
                    .and_then(|raw| Url::parse(raw).ok())
                    .is_some_and(|shell_url| same_origin(url, shell_url.as_str())))
    }

    fn same_origin(url: &Url, origin: &str) -> bool {
        Url::parse(origin).is_ok_and(|origin| {
            url.scheme() == origin.scheme()
                && url.host_str() == origin.host_str()
                && url.port_or_known_default() == origin.port_or_known_default()
        })
    }

    fn is_http_url(url: &Url) -> bool {
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn hosted_dashboard_links_are_allowed() {
            let url = Url::parse("https://txcl.io/dashboard?session=abc").unwrap();

            assert!(is_allowed_shell_url(&url));
        }

        #[test]
        fn unrelated_origins_are_rejected() {
            let url = Url::parse("https://example.com/dashboard?session=abc").unwrap();

            assert!(!is_allowed_shell_url(&url));
        }

        #[test]
        fn same_origin_checks_scheme_host_and_port() {
            let url = Url::parse("https://txcl.io/dashboard").unwrap();

            assert!(same_origin(&url, "https://txcl.io"));
            assert!(!same_origin(&url, "http://txcl.io"));
            assert!(!same_origin(&url, "https://txcl.io:444"));
        }
    }
}
