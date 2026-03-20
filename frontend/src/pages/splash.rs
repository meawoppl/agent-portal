use crate::{utils, VERSION};
use gloo::console;
use yew::prelude::*;

#[function_component(SplashPage)]
pub fn splash_page() -> Html {
    let handle_login = Callback::from(|_| {
        console::log!("Redirecting to Google OAuth...");
        // Redirect to backend OAuth endpoint
        let window = web_sys::window().expect("no global `window` exists");
        let location = window.location();
        let auth_url = utils::api_url("/api/auth/google");
        let _ = location.set_href(&auth_url);
    });

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
                                <span class="terminal-btn minimize">{ "−" }</span>
                                <span class="terminal-btn maximize">{ "□" }</span>
                                <span class="terminal-btn close">{ "×" }</span>
                            </div>
                        </div>
                        <div class="terminal-body">
                            <div class="terminal-line">
                                <span class="prompt">{ "$ " }</span>
                                <span class="command">{ "claude-portal" }</span>
                            </div>
                            <div class="terminal-line empty"></div>
                            <div class="terminal-line">
                                <span class="output blue">{ "╭──────────────────────────────────────╮" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output blue">{ "│        Agent Portal Starting         │" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output blue">{ "╰──────────────────────────────────────╯" }</span>
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
                                <span class="output blue">{ "  → " }</span>
                                <span class="output">{ "Connecting to backend... " }</span>
                                <span class="output green">{ "connected" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output blue">{ "  → " }</span>
                                <span class="output">{ "Registering session... " }</span>
                                <span class="output green">{ "registered" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output blue">{ "  → " }</span>
                                <span class="output">{ "Starting Claude Code... " }</span>
                                <span class="output green">{ "started" }</span>
                            </div>
                            <div class="terminal-line empty"></div>
                            <div class="terminal-line">
                                <span class="output green">{ "╭──────────────────────────────────────╮" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output green">{ "│         ✓ Proxy Ready                │" }</span>
                            </div>
                            <div class="terminal-line">
                                <span class="output green">{ "╰──────────────────────────────────────╯" }</span>
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
                        <h3>{ "🌐 Remote Access" }</h3>
                        <p>{ "Connect to your dedicated development machines from any browser" }</p>
                    </div>
                    <div class="feature">
                        <h3>{ "🔄 Multiple Sessions" }</h3>
                        <p>{ "Manage and switch between multiple agent sessions" }</p>
                    </div>
                    <div class="feature">
                        <h3>{ "🚀 Fire & Forget" }</h3>
                        <p>{ "Start agent tasks and walk away. Check results later from any device" }</p>
                    </div>
                    <div class="feature">
                        <h3>{ "🔒 Secure" }</h3>
                        <p>{ "Google OAuth authentication and encrypted connections" }</p>
                    </div>
                </div>

                <button class="login-button" onclick={handle_login}>
                    <span class="google-icon">{ "G" }</span>
                    { " Sign in with Google" }
                </button>

                <div class="splash-footer">
                    <span class="version">{ format!("v{}", VERSION) }</span>
                    <a
                        href="https://github.com/meawoppl/agent-portal"
                        target="_blank"
                        rel="noopener noreferrer"
                        class="footer-link"
                    >
                        <span class="github-icon">{ "" }</span>
                        { "GitHub" }
                    </a>
                    <a
                        href="https://github.com/meawoppl/agent-portal/issues/new"
                        target="_blank"
                        rel="noopener noreferrer"
                        class="footer-link bug-report"
                    >
                        <span class="bug-icon">{ "🐛" }</span>
                        { "Report a Bug" }
                    </a>
                </div>
            </div>
        </div>
    }
}
