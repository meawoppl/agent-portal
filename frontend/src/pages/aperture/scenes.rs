use super::model::{Action, Experiment, MACHINES};
use yew::prelude::*;

pub fn render(state: &UseReducerHandle<Experiment>, action: Callback<MouseEvent>) -> Html {
    match state.chamber {
        0 => machines(state, action),
        1 => website(state, action),
        2 => sharing(state),
        _ => agents(state),
    }
}

fn machines(state: &Experiment, action: Callback<MouseEvent>) -> Html {
    let machine = MACHINES[state.machine];
    let destination = MACHINES[state.destination];
    html! {
        <div class={classes!("ap-machine-room", state.completed[0].then_some("ap-arrival"))} key={format!("room-{}", state.machine)}>
            <div class="ap-room-grid" aria-hidden="true"/>
            <div class="ap-room-sign" aria-hidden="true">{"01"}<small>{"TRANSIT"}</small></div>
            <div class="ap-room-ceiling" aria-hidden="true"/>
            <div class="ap-portals">
                <div class="ap-portal-station">
                    <div class="ap-portal ap-blue"><div class="ap-portal-interior">
                        <span class="ap-portal-code">{"AP / ORIGIN"}</span>
                        <div class="ap-machine-glyph" aria-hidden="true">{"▰"}<span>{"━━━"}</span></div>
                        <strong>{machine.0}</strong><small>{machine.1}</small>
                        <span class="ap-portal-prompt">{"YOU ARE HERE"}</span>
                    </div></div>
                    <span class="ap-portal-floor-label">{"01 / ENTRY"}</span>
                </div>
                <div class="ap-transit-path" aria-hidden="true"><span>{"·"}</span><span>{"·"}</span><span>{"·"}</span><span>{"›"}</span></div>
                <div class="ap-portal-station">
                    <button class="ap-portal ap-orange" onclick={action} disabled={state.destination == state.machine} aria-label={format!("Step through portal to {}", destination.0)}>
                        <span class="ap-portal-interior">
                            <span class="ap-portal-code">{"AP / DESTINATION"}</span>
                            <span class="ap-machine-glyph" aria-hidden="true">{"▤"}<span>{"▤"}</span></span>
                            <strong>{destination.0}</strong><small>{destination.1}</small>
                            <span class="ap-portal-prompt">{if state.destination == state.machine { "TRANSIT COMPLETE" } else { "STEP THROUGH ↗" }}</span>
                        </span>
                    </button>
                    <span class="ap-portal-floor-label">{"02 / EXIT"}</span>
                </div>
            </div>
            <div class="ap-room-terminal" key={format!("machine-{}", state.machine)}>
                <div><span class="ap-led"/>{format!("CONNECTED / {}", machine.2)}<span>{"AGENT ACTIVE"}</span></div>
                <p><span>{"❯"}</span>{machine.3}</p>
                <small>{"One browser. Your sessions keep running on their own machines."}</small>
            </div>
        </div>
    }
}

fn website(state: &Experiment, action: Callback<MouseEvent>) -> Html {
    html! {
        <div class={classes!("ap-website-room", state.website_open.then_some("open"))}>
            <div class="ap-browser-chrome"><span>{"● ● ●"}</span><span>{if state.website_open { "companion-lab / forwarded preview" } else { "lab-b.internal:8080 / unreachable from your browser" }}</span><span>{"↗"}</span></div>
            if state.website_open {
                <iframe class="ap-sample-site" title="Interactive Companion Calibration demo website" src="/aperture-demo/companion.html" sandbox="allow-scripts"/>
            } else {
                <div class="ap-website-waiting">
                    <div class="ap-orbit" aria-hidden="true"><span>{"8080"}</span></div>
                    <span class="ap-eyebrow">{"THE WEBSITE EXISTS. JUST NOT HERE."}</span>
                    <h2>{"Your agent made it."}<br/>{"Your portal brings it."}</h2>
                    <button class="ap-terminal-command" onclick={action}><span>{"$"}</span>{"agent-portal forward 8080"}<span>{"↵"}</span></button>
                </div>
            }
            <div class="ap-transport-path"><span>{"REMOTE MACHINE"}</span><i/><span>{"AGENT PORTAL"}</span><i/><span>{"YOUR BROWSER"}</span></div>
        </div>
    }
}

fn sharing(state: &Experiment) -> Html {
    html! {
        <div class={classes!("ap-sharing-room", state.invited.then_some("invited"))}>
            <div class="ap-sharing-title"><span class="ap-eyebrow">{"COOPERATIVE TESTING INITIATIVE"}</span><h2>{"Science needs witnesses."}</h2></div>
            <div class="ap-human-pair">
                <div class="ap-subject"><div class="ap-subject-icon ap-subject-blue" aria-hidden="true">{"◉"}<span>{"╱┃╲"}</span><span>{"╱ ╲"}</span></div><strong>{"YOU"}</strong><small>{"OWNER / FULL CONTROL"}</small></div>
                <div class="ap-link-beam" aria-hidden="true">{if state.invited { "● ━━━ ●" } else { "○ ┄┄┄ ○" }}<span>{if state.invited { "INVITATION READY" } else { "AWAITING FRIEND" }}</span></div>
                <div class={classes!("ap-subject", (!state.invited).then_some("ap-ghost"))}><div class="ap-subject-icon ap-subject-orange" aria-hidden="true">{"◉"}<span>{"╱┃╲"}</span><span>{"╱ ╲"}</span></div><strong>{"YOUR FRIEND"}</strong><small>{if state.editor { "EDITOR / CAN PARTICIPATE" } else { "VIEWER / CAN WATCH" }}</small></div>
            </div>
            <div class="ap-permission-card"><span>{if state.editor { "✎" } else { "◎" }}</span><div><strong>{if state.editor { "An extra pair of hands." } else { "A seat behind the glass." }}</strong><p>{if state.editor { "Editors can send prompts and collaborate in a shared session." } else { "Viewers follow the session without sending prompts or controlling the agent." }}</p></div></div>
        </div>
    }
}

fn agents(state: &Experiment) -> Html {
    html! {
        <div class="ap-agents-room">
            <div class="ap-agent-topology">
                <div><span class="ap-agent-avatar">{"C"}</span><strong>{"BUILDER"}</strong><small>{"Claude / macbook.local"}</small></div>
                <span class={classes!("ap-agent-wire", (state.messages > 0).then_some("active"))} aria-hidden="true">{"◌ ━━━ ◌"}</span>
                <div><span class="ap-agent-avatar orange">{"X"}</span><strong>{"REVIEWER"}</strong><small>{"Codex / lab-b.internal"}</small></div>
            </div>
            <div class="ap-agent-conversation" aria-live="polite">
                <div class="ap-agent-message"><span>{"CLAUDE"}</span><p>{"The companion calibration app is ready. Requesting an independent review before we introduce humans."}</p></div>
                if state.messages >= 1 {
                    <div class="ap-agent-command"><span>{"$ agent-portal message send <session-id>"}</span><code>{"\"Review the calibration app. Pay particular attention to the launch button.\""}</code></div>
                }
                if state.messages >= 2 {
                    <div class="ap-agent-message orange"><span>{"CODEX"}</span><p>{"Review received on lab-b. Checking boundaries, controls, and the continued existence of the cube…"}</p></div>
                }
                if state.messages >= 3 {
                    <div class="ap-agent-result"><span>{"✓ REVIEW COMPLETE"}</span><p>{"All checks passed. Added reduced-motion support. The cube has elected to remain a cube."}</p></div>
                } else if state.messages > 0 {
                    <div class="ap-agent-thinking"><i/><i/><i/><span>{"Agents collaborating"}</span></div>
                } else {
                    <div class="ap-agent-idle">{"Independent minds. Shared context. No copy-pasting required."}</div>
                }
            </div>
        </div>
    }
}

pub fn controls(state: &UseReducerHandle<Experiment>) -> Html {
    match state.chamber {
        0 => html! {
            <div class="ap-machine-picker" role="group" aria-label="Destination machine">
                {for MACHINES.iter().enumerate().map(|(index, machine)| {
                    let selected = state.destination == index;
                    let here = state.machine == index;
                    let state = state.clone();
                    let onclick = Callback::from(move |_| state.dispatch(Action::Destination(index)));
                    html! {<button {onclick} aria-pressed={selected.to_string()} class={classes!("ap-machine-option", selected.then_some("selected"))}><span>{format!("0{}", index + 1)}<small>{if here { "HERE" } else { "READY" }}</small></span><strong>{machine.0}</strong><small>{machine.1}</small></button>}
                })}
            </div>
        },
        1 => {
            html! {<p class="ap-deck-note">{if state.website_open { "The sample is a real interactive page. Try rotating or launching the cube. In Agent Portal, this panel shows the service running on your agent’s machine." } else { "Agent Portal carries HTTP, WebSockets, and streaming responses through the session’s tunnel. Your app gets its own web address." }}</p>}
        }
        2 => html! {
            <div class="ap-role-picker" role="group" aria-label="Demo invitation role">
                {for [false, true].into_iter().map(|editor| {
                    let selected = state.editor == editor;
                    let state = state.clone();
                    let onclick = Callback::from(move |_| state.dispatch(Action::Role(editor)));
                    html! {<button {onclick} class={classes!("ap-role-option", selected.then_some("selected"))} aria-pressed={selected.to_string()}><span>{if editor { "✎" } else { "◎" }}</span><div><strong>{if editor { "Editor" } else { "Viewer" }}</strong><small>{if editor { "Work alongside you" } else { "Watch the experiment" }}</small></div><span>{if selected { "●" } else { "○" }}</span></button>}
                })}
            </div>
        },
        _ => {
            html! {<p class="ap-deck-note">{"Agents can message another session directly or queue durable work for later. Keep the builder building while the reviewer reviews."}</p>}
        }
    }
}

/// Tie the fictional chambers back to the actual product controls. In
/// particular, sharing a demo URL must never imply a real membership grant.
pub fn guide(chamber: usize) -> Html {
    let steps = match chamber {
        0 => [
            "Connect your computers using Agent Portal's launcher.",
            "Open a new session in the dashboard and choose its Host, agent, and working directory.",
            "Select a session in the rail to switch machines. Each agent keeps running on its own host.",
        ],
        1 => [
            "Ask your remote agent to start an HTTP service, for example on port 8080.",
            "Have that agent run agent-portal forward 8080. The returned address opens its service in your browser.",
            "Forwarded apps stay private unless the owner explicitly makes the forward public in Settings.",
        ],
        2 => [
            "Open a session's menu and choose Share Session.",
            "Enter your friend's account email, choose Viewer or Editor, and select Add. Your friend needs an account on the same portal.",
            "A Viewer can follow the session. An Editor can send prompts. Owners can change or remove access.",
        ],
        _ => [
            "Have an agent run agent-portal message list to find your other sessions and their IDs.",
            "Send a message with agent-portal message send <session-id> \"Review my changes\".",
            "The receiving agent sees who sent it. Use agent-portal work-queue add <session-id> for work that should wait.",
        ],
    };
    html! {
        <details class="ap-real-guide">
            <summary>{"Try this with your real agents"}</summary>
            <ol>{for steps.into_iter().map(|step| html! { <li>{step}</li> })}</ol>
            <a href="/dashboard">{"Open your dashboard ↗"}</a>
        </details>
    }
}
