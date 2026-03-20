//! Proxy Token Setup Component
//!
//! Displays instructions for setting up the proxy CLI with device flow authentication.

use crate::components::CopyCommand;
use gloo::utils::window;
use yew::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    Linux,
    MacOS,
    Windows,
}

impl Platform {
    fn label(&self) -> &'static str {
        match self {
            Platform::Linux => "Linux",
            Platform::MacOS => "macOS",
            Platform::Windows => "Windows",
        }
    }
}

fn detect_platform() -> Platform {
    let user_agent = window()
        .navigator()
        .user_agent()
        .unwrap_or_default()
        .to_lowercase();

    if user_agent.contains("win") {
        Platform::Windows
    } else if user_agent.contains("mac") {
        Platform::MacOS
    } else {
        Platform::Linux
    }
}

#[function_component(ProxyTokenSetup)]
pub fn proxy_token_setup() -> Html {
    let detected = detect_platform();
    let selected_platform = use_state(|| detected);

    // Get the base URL for the install script
    let base_url = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_else(|| "http://localhost:3000".to_string());

    let platforms = [Platform::Linux, Platform::MacOS, Platform::Windows];

    // Derive WebSocket URL from current origin (http->ws, https->wss)
    let ws_backend_url = base_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");

    // URL-encode the backend URL for the query parameter
    let encoded_backend_url = js_sys::encode_uri_component(&ws_backend_url);

    let install_command = match *selected_platform {
        Platform::Linux | Platform::MacOS => {
            // Pass backend_url as query param so install script configures the correct WebSocket URL
            format!(
                "curl -fsSL \"{}/api/download/install.sh?backend_url={}\" | bash",
                base_url, encoded_backend_url
            )
        }
        Platform::Windows => format!(
            "# Download from GitHub releases, then run:\n.\\agent-portal.exe --backend-url \"{}\"",
            ws_backend_url
        ),
    };
    let login_command = match *selected_platform {
        Platform::Linux | Platform::MacOS => "agent-portal login".to_string(),
        Platform::Windows => ".\\agent-portal.exe login".to_string(),
    };
    let service_command = match *selected_platform {
        Platform::Linux | Platform::MacOS => "agent-portal service install".to_string(),
        Platform::Windows => ".\\agent-portal.exe service install".to_string(),
    };

    html! {
        <div class="proxy-setup has-token">
            <h3>{ "Quick Setup" }</h3>

            <div class="platform-selector">
                {for platforms.iter().map(|platform| {
                    let is_selected = *selected_platform == *platform;
                    let platform = *platform;
                    let selected_platform = selected_platform.clone();

                    let class = classes!(
                        "platform-option",
                        is_selected.then_some("selected")
                    );

                    let onclick = Callback::from(move |_| {
                        selected_platform.set(platform);
                    });

                    html! {
                        <button {class} {onclick}>
                            {platform.label()}
                        </button>
                    }
                })}
            </div>

            <div class="setup-notes">
                {if *selected_platform == Platform::Windows {
                    html! {
                        <>
                            <p class="note warning-note">
                                <span class="note-icon">{ "!" }</span>
                                { "Windows support is experimental and largely untested. " }
                                <a href="https://github.com/meawoppl/agent-portal/issues" target="_blank">
                                    { "Please report issues!" }
                                </a>
                            </p>
                            <p class="note windows-note">
                                <span class="note-icon">{ "!" }</span>
                                { "Download Windows binary from " }
                                <a href="https://github.com/meawoppl/agent-portal/releases/latest" target="_blank">
                                    { "GitHub releases" }
                                </a>
                            </p>
                        </>
                    }
                } else {
                    html! {}
                }}
            </div>

            <div class="setup-step">
                <span class="step-number">{ "1" }</span>
                <div class="step-content">
                    <p class="step-label">{ "Install:" }</p>
                    <CopyCommand command={install_command} />
                </div>
            </div>

            <div class="setup-step">
                <span class="step-number">{ "2" }</span>
                <div class="step-content">
                    <p class="step-label">{ "Log in:" }</p>
                    <CopyCommand command={login_command} />
                </div>
            </div>

            <div class="setup-step">
                <span class="step-number">{ "3" }</span>
                <div class="step-content">
                    <p class="step-label">{ "Run as a background service:" }</p>
                    <CopyCommand command={service_command} />
                    <p class="step-hint">{ "(Or run directly with: agent-portal)" }</p>
                </div>
            </div>
        </div>
    }
}
