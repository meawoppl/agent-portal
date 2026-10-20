use super::model::{Action, Experiment, MACHINES};
use yew::prelude::*;

pub fn render(state: &UseReducerHandle<Experiment>, action: Callback<MouseEvent>) -> Html {
    match state.chamber {
        0 => agents(state),
        1 => {
            html! { <div class="ap-sector-stack">{machines(state, action)}{super::engineering::footage("bench")}<p class="ap-deck-note">{"RECORDED PHYSICAL BENCH · CF-00020-01 / Nucleo fixture. Recorded telemetry, not a live hardware connection; no new hardware actuation."}</p></div> }
        }
        2 => html! { <div class="ap-sector-stack">{website(state, action)}{sharing(state)}</div> },
        _ => super::plugins::render(state),
    }
}

fn machines(state: &Experiment, action: Callback<MouseEvent>) -> Html {
    let machine = MACHINES[state.machine];
    let destination = MACHINES[state.destination];
    html! {
        <div class={classes!("ap-machine-room", state.completed[1].then_some("ap-arrival"))} key={format!("room-{}", state.machine)}>
            <div class="ap-room-grid" aria-hidden="true"/>
            <div class="ap-room-sign" aria-hidden="true">{"02"}<small>{"TRANSIT"}</small></div>
            <div class="ap-room-ceiling" aria-hidden="true"/>
            <div class="ap-portals">
                <div class="ap-portal-station">
                    <div class="ap-portal ap-blue"><div class="ap-portal-interior">
                        <span class="ap-portal-code">{"AP / ORIGIN"}</span>
                        <div class="ap-machine-glyph" aria-hidden="true">{"▰"}<span>{"━━━"}</span></div>
                        <strong>{machine.0}</strong><small>{machine.1}</small>
                        <span class="ap-portal-prompt">{"AGENT IS HERE"}</span>
                    </div></div>
                    <span class="ap-portal-floor-label">{"01 / ENTRY"}</span>
                </div>
                <div class="ap-transit-path" aria-hidden="true"><span>{"·"}</span><span>{"·"}</span><span>{"·"}</span><span>{"›"}</span></div>
                <div class="ap-portal-station">
                    <button class="ap-portal ap-orange" onclick={action} disabled={state.destination == state.machine} aria-label={format!("Observe agent transit to {}", destination.0)}>
                        <span class="ap-portal-interior">
                            <span class="ap-portal-code">{"AP / DESTINATION"}</span>
                            <span class="ap-machine-glyph" aria-hidden="true">{"▤"}<span>{"▤"}</span></span>
                            <strong>{destination.0}</strong><small>{destination.1}</small>
                            <span class="ap-portal-prompt">{if state.destination == state.machine { "TRANSIT COMPLETE" } else { "AGENT TRANSIT ↗" }}</span>
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
                    <h2>{"The agent built it."}<br/>{"The agent forwards it."}</h2>
                    <button class="ap-terminal-command" onclick={action}><span>{"AGENT $"}</span>{"agent-portal forward 8080"}<span>{"↵"}</span></button>
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
            <div class="ap-code-window"><div class="ap-design-title"><span>{"AGENT-AUTHORED CODE"}</span><span>{"companion.html"}</span></div><pre><code>{"// Generated by the builder; reviewed by a peer.\nrotation += 90;\ncube.style.setProperty(\n  \"--rotation\", rotation + \"deg\"\n);"}</code></pre></div>
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
        1 => html! {
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
        0 => {
            html! {<p class="ap-deck-note">{"The software and peer conversation shown here demonstrate an isolated workflow. Agents can send real review requests directly to another session or queue durable work. No human message courier required."}</p>}
        }
        _ => super::plugins::controls(state),
    }
}

/// Tie the fictional chambers back to the actual product controls. In
/// particular, sharing a demo URL must never imply a real membership grant.
pub fn guide(chamber: usize) -> Html {
    let steps = match chamber {
        1 => [
            "Agents handle launcher setup on the intended machines; account authorization stays with the owner.",
            "The dashboard shows each session's Host, agent, and working directory so the owner can inspect where work runs.",
            "The session rail switches the observation window between machines. Each agent keeps running on its own host.",
        ],
        2 => [
            "The remote agent starts its HTTP service, for example on port 8080.",
            "The agent runs agent-portal forward 8080 and presents the returned address. The browser opens the service.",
            "Forwarded apps stay private unless made public in Settings. Session sharing separately uses account email plus Viewer or Editor role; a demo invitation grants no real permissions.",
        ],
        0 => [
            "The agent runs agent-portal message list to find peer sessions and their IDs.",
            "The agent sends its review request with agent-portal message send <session-id> \"Review my changes\".",
            "The receiving agent sees who sent it. Agents use agent-portal work-queue add <session-id> for work that should wait.",
        ],
        _ => [
            "The agent installs the relevant plugin, runs its setup and doctor checks, and opens the workbench alongside the conversation.",
            "Agents author project sources, inspect results, and revise designs. The plugin provides domain tools, retained evidence, and supported exports.",
            "The recordings preserve real tool output. Inspect the source and run evidence; model simulation, recorded physical measurements, and manufacturing qualification are distinct claims.",
        ],
    };
    html! {
        <details class="ap-real-guide">
            <summary>{"How agents do this for real"}</summary>
            <ol>{for steps.into_iter().map(|step| html! { <li>{step}</li> })}</ol>
            <a href="/dashboard">{"Open your dashboard ↗"}</a>
        </details>
    }
}
