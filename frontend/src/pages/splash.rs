use crate::utils::{self, On401};
use crate::VERSION;
use gloo::console;
use shared::AppConfig;
use yew::prelude::*;

#[function_component(SplashPage)]
pub fn splash_page() -> Html {
    let config = use_state(|| None::<AppConfig>);

    {
        let config = config.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                // Pre-login page: a 401 here must not bounce through logout.
                if let Ok(loaded) =
                    utils::fetch_json::<AppConfig>("/api/config", On401::Ignore).await
                {
                    config.set(Some(loaded));
                }
            });
            || ()
        });
    }

    let buttons = login_buttons(config.as_ref());
    match config.as_ref().and_then(|c| c.splash_text.clone()) {
        Some(text) => minimal_splash(text, buttons),
        None => marketing_splash(buttons),
    }
}

/// A provider key as it appears in [`AppConfig::auth_providers`], paired with
/// the button copy and the route that starts its flow. Unknown keys render
/// nothing, so a backend advertising a provider this build does not know about
/// degrades to hiding it rather than showing a dead button.
fn provider_button(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider {
        "google" => Some(("Sign in with Google", "/api/auth/google")),
        "github" => Some(("Sign in with GitHub", "/api/auth/github")),
        _ => None,
    }
}

/// The GitHub mark, inlined rather than shipped as an asset so it inherits the
/// button's `currentColor` and needs no extra request.
fn provider_icon(provider: &str) -> Html {
    match provider {
        "github" => html! {
            <svg
                class="provider-icon"
                viewBox="0 0 24 24"
                width="18"
                height="18"
                fill="currentColor"
                aria-hidden="true"
            >
                <path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 \
                         0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 \
                         17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 \
                         1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 \
                         0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 \
                         1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 \
                         3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 \
                         5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 \
                         22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" />
            </svg>
        },
        _ => html! { <span class="provider-icon google-icon">{ "G" }</span> },
    }
}

/// Render one button per configured provider.
///
/// Nothing renders until the config arrives, so a provider the deploy has no
/// credentials for is never offered — the alternative is a button that 404s on
/// click. An empty provider list means dev mode, where login is bypassed: the
/// Google route redirects to dev login, so a single generic button is right.
fn login_buttons(config: Option<&AppConfig>) -> Html {
    let Some(config) = config else {
        return html! {};
    };

    if config.auth_providers.is_empty() {
        return html! {
            <button class="login-button" onclick={login_callback("/api/auth/google")}>
                { "Sign in" }
            </button>
        };
    }

    html! {
        <div class="login-buttons">
            { for config.auth_providers.iter().filter_map(|provider| {
                let (label, route) = provider_button(provider)?;
                Some(html! {
                    <button
                        key={provider.clone()}
                        class="login-button"
                        onclick={login_callback(route)}
                    >
                        { provider_icon(provider) }
                        { format!(" {label}") }
                    </button>
                })
            }) }
        </div>
    }
}

fn login_callback(route: &'static str) -> Callback<MouseEvent> {
    Callback::from(move |_| {
        console::log!("Redirecting to OAuth:", route);
        let Some(window) = web_sys::window() else {
            return;
        };
        let location = window.location();
        let auth_url = utils::api_url(route);
        let _ = location.set_href(&auth_url);
    })
}

/// Version + GitHub + bug-report footer shared by both splash variants.
/// `github_icon` controls the icon span the marketing variant renders
/// inside the GitHub link.
fn splash_footer(github_icon: bool) -> Html {
    html! {
        <div class="splash-footer">
            <span class="version">{ format!("v{}", VERSION) }</span>
            <a
                href="https://github.com/meawoppl/agent-portal"
                target="_blank"
                rel="noopener noreferrer"
                class="footer-link"
            >
                if github_icon {
                    <span class="github-icon">{ "" }</span>
                }
                { "GitHub" }
            </a>
            <a
                href="https://github.com/meawoppl/agent-portal/issues/new"
                target="_blank"
                rel="noopener noreferrer"
                class="footer-link bug-report"
            >
                { "Report a Bug" }
            </a>
            <a href="/privacy" class="footer-link">
                { "Privacy" }
            </a>
        </div>
    }
}

fn minimal_splash(heading: String, login_buttons: Html) -> Html {
    html! {
        <div class="splash-container">
            <div class="splash-content splash-minimal">
                <div class="splash-header">
                    <h1>{ heading }</h1>
                </div>

                { login_buttons }

                { splash_footer(false) }
            </div>
        </div>
    }
}

fn marketing_splash(login_buttons: Html) -> Html {
    html! {
        <div class="splash-container">
            <div class="splash-content">
                <div class="splash-header">
                    <h1>{ "Agent Portal" }</h1>
                    <p class="tagline">
                        { "Access your remote agent sessions from anywhere" }
                    </p>
                </div>

                <div class="splash-hero">
                    <div class="terminal-preview">
                        <div class="terminal-header">
                            <span class="terminal-title">{ "Terminal" }</span>
                            <div class="terminal-buttons">
                                <span class="terminal-btn minimize">{ "\u{2212}" }</span>
                                <span class="terminal-btn maximize">{ "\u{25a1}" }</span>
                                <span class="terminal-btn close">{ "\u{00d7}" }</span>
                            </div>
                        </div>
                        <div class="terminal-body">
                            <div class="terminal-line">
                                <span class="prompt">{ "$ " }</span>
                                <span class="command">{ "claude-portal" }</span>
                            </div>
                            <div class="terminal-line empty"></div>
                            <div class="terminal-line">
                                <span class="output blue">{ "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output blue">{ "\u{2502}        Agent Portal Starting         \u{2502}" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output blue">{ "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}" }</span>
                            </div>
                            <div class="terminal-line empty"></div>
                            <div class="terminal-line">
                                <span class="output dim">{ "  Session: " }</span>
                                <span class="output">{ "my-workstation-20260117-041500" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output dim">{ "  Backend: " }</span>
                                <span class="output">{ "wss://txcl.io" }</span>
                            </div>
                            <div class="terminal-line empty"></div>
                            <div class="terminal-line">
                                <span class="output blue">{ "  \u{2192} " }</span>
                                <span class="output">{ "Connecting to backend... " }</span>
                                <span class="output green">{ "connected" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output blue">{ "  \u{2192} " }</span>
                                <span class="output">{ "Registering session... " }</span>
                                <span class="output green">{ "registered" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output blue">{ "  \u{2192} " }</span>
                                <span class="output">{ "Starting Claude Code... " }</span>
                                <span class="output green">{ "started" }</span>
                            </div>
                            <div class="terminal-line empty"></div>
                            <div class="terminal-line">
                                <span class="output green">{ "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output green">{ "\u{2502}         \u{2713} Proxy Ready                \u{2502}" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output green">{ "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}" }</span>
                            </div>
                            <div class="terminal-line empty"></div>
                            <div class="terminal-line">
                                <span class="output">{ "  Navigate to " }</span>
                                <span class="output cyan">{ "txcl.io" }</span>
                                <span class="output">{ " to use the terminal." }</span>
                            </div>
                        </div>
                    </div>
                </div>

                <div class="splash-features">
                    <div class="feature">
                        <h3>{ "Remote Access" }</h3>
                        <p>{ "Connect to your dedicated development machines from any browser" }</p>
                    </div>
                    <div class="feature">
                        <h3>{ "Multiple Sessions" }</h3>
                        <p>{ "Manage and switch between multiple agent sessions" }</p>
                    </div>
                    <div class="feature">
                        <h3>{ "Fire & Forget" }</h3>
                        <p>{ "Start agent tasks and walk away. Check results later from any device" }</p>
                    </div>
                    <div class="feature">
                        <h3>{ "Secure" }</h3>
                        <p>{ "OAuth authentication and encrypted connections" }</p>
                    </div>
                </div>

                { login_buttons }

                { splash_footer(true) }
            </div>
        </div>
    }
}
