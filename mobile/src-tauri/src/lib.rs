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
    use tauri::{Manager, Url};
    use tauri_plugin_deep_link::DeepLinkExt;

    const DEFAULT_SHELL_URL: &str = "https://txcl.io";
    const MAIN_WINDOW_LABEL: &str = "main";

    pub fn run() -> tauri::Result<()> {
        tauri::Builder::default()
            .plugin(tauri_plugin_deep_link::init())
            .setup(|app| {
                let app_handle = app.handle().clone();

                match app.deep_link().get_current() {
                    Ok(Some(urls)) => route_deep_links(&app_handle, urls),
                    Ok(None) => {
                        if let Some(url) = shell_url_override() {
                            navigate_main_window(&app_handle, url);
                        }
                    }
                    Err(err) => eprintln!("failed to read startup deep link: {err}"),
                }

                let app_handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    route_deep_links(&app_handle, event.urls());
                });

                Ok(())
            })
            .run(tauri::generate_context!())
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
