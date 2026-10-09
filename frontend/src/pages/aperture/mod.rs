//! Public, self-contained product exhibit. This route deliberately needs no
//! login: every machine, role and agent event is a clearly labelled simulation.
mod audio;
mod engineering;
mod model;
mod plugins;
mod scenes;

use gloo::timers::callback::Timeout;
use model::{Action, Experiment, CHAMBER_COUNT, INSTRUCTIONS, LABELS, SLUGS, TITLES};
use serde::Deserialize;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use yew::prelude::*;

#[derive(Deserialize)]
struct NarrationManifest {
    lines: Vec<NarrationLine>,
}
#[derive(Deserialize)]
struct NarrationLine {
    id: String,
    text: String,
}

fn transcript(id: &str) -> String {
    // Captions and voice are authored from one manifest so they cannot drift.
    serde_json::from_str::<NarrationManifest>(include_str!("../../../assets/aperture/lines.json"))
        .ok()
        .and_then(|manifest| manifest.lines.into_iter().find(|line| line.id == id))
        .map(|line| line.text)
        .unwrap_or_default()
}

fn observer_role() -> Option<&'static str> {
    let hash = web_sys::window()?.location().hash().ok()?;
    match hash.as_str() {
        "#observer-viewer" => Some("viewer"),
        "#observer-editor" => Some("editor"),
        _ => None,
    }
}

#[function_component(AperturePage)]
pub fn aperture_page() -> Html {
    let observer = use_state(observer_role);
    let state = use_reducer(|| {
        let mut experiment = Experiment::default();
        if observer_role().is_some() {
            experiment.chamber = 2;
            experiment.editor = observer_role() == Some("editor");
        }
        experiment
    });
    let sound = use_state(|| false);
    let started = use_state(|| observer_role().is_some());
    let copy_status = use_state(String::new);
    let chamber = state.chamber;
    let complete = state.completed[chamber];
    let line_id = if state.finale {
        "finale".to_owned()
    } else if state.making_of {
        "making-of".to_owned()
    } else if state.refinement {
        "mission".to_owned()
    } else if !*started {
        "welcome".to_owned()
    } else {
        format!(
            "{}-{}",
            SLUGS[chamber],
            if complete { "complete" } else { "intro" }
        )
    };

    {
        let line_id = line_id.clone();
        use_effect_with((*sound, line_id), move |(enabled, line)| {
            if *enabled {
                audio::speak(line);
            } else {
                audio::stop();
            }
            || audio::stop()
        });
    }
    {
        let state = state.clone();
        // Depend on the current step and keep the timer owned by the effect:
        // leaving the chamber cancels pending work; returning resumes it.
        use_effect_with((chamber, state.messages), move |(chamber, messages)| {
            let timer = if *chamber == 0 && (1..3).contains(messages) {
                Some(Timeout::new(1_700, move || state.dispatch(Action::Message)))
            } else {
                None
            };
            move || drop(timer)
        });
    }
    {
        use_effect_with((), |_| {
            let document = gloo::utils::document();
            let previous = document.title();
            document.set_title("Agent Portal · Aperture Science Enrichment Center");
            move || document.set_title(&previous)
        });
    }

    let on_sound = {
        let sound = sound.clone();
        Callback::from(move |_| sound.set(!*sound))
    };
    let on_begin = {
        let started = started.clone();
        Callback::from(move |_| started.set(true))
    };
    let on_action = {
        let state = state.clone();
        let started = started.clone();
        let enabled = *sound;
        Callback::from(move |_| {
            started.set(true);
            if enabled {
                audio::tone();
            }
            state.dispatch(match chamber {
                0 => Action::Message,
                1 => Action::Transit,
                2 if !state.website_open => Action::Website,
                2 => Action::Invite,
                _ => Action::Observe,
            });
        })
    };
    let on_next = {
        let state = state.clone();
        let started = started.clone();
        Callback::from(move |_| {
            started.set(true);
            if state.completed.iter().all(|done| *done) {
                state.dispatch(if state.refinement {
                    Action::Finale
                } else if state.making_of {
                    Action::Refine
                } else {
                    Action::MakingOf
                });
            } else {
                let next = (1..=CHAMBER_COUNT)
                    .map(|offset| (chamber + offset) % CHAMBER_COUNT)
                    .find(|index| !state.completed[*index])
                    .unwrap_or(0);
                state.dispatch(Action::Chamber(next));
            }
        })
    };
    let on_reset = {
        let state = state.clone();
        let started = started.clone();
        let copy_status = copy_status.clone();
        Callback::from(move |_| {
            state.dispatch(Action::Reset);
            started.set(false);
            copy_status.set(String::new());
        })
    };
    let share_url = web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .map(|url| {
            format!(
                "{}/aperture#observer-{}",
                url,
                if state.editor { "editor" } else { "viewer" }
            )
        })
        .unwrap_or_default();
    let on_copy = {
        let share_url = share_url.clone();
        let copy_status = copy_status.clone();
        Callback::from(move |_| {
            let share_url = share_url.clone();
            let copy_status = copy_status.clone();
            spawn_local(async move {
                let Some(window) = web_sys::window() else {
                    return;
                };
                if !js_sys::Reflect::has(window.navigator().as_ref(), &"clipboard".into())
                    .unwrap_or(false)
                {
                    copy_status.set("Select and copy the invitation below.".to_owned());
                    return;
                }
                let result =
                    JsFuture::from(window.navigator().clipboard().write_text(&share_url)).await;
                copy_status.set(
                    if result.is_ok() {
                        "Invitation copied. For science."
                    } else {
                        "Select and copy the invitation below."
                    }
                    .to_owned(),
                );
            });
        })
    };

    let caption = transcript(&line_id);
    let completed_count = state.completed.iter().filter(|done| **done).count();
    html! {
        <main class="aperture">
            <a class="ap-skip" href="#ap-experiment">{"Skip to experiment"}</a>
            <header class="ap-header">
                <a class="ap-brand" href="/aperture" aria-label="Aperture Science Agent Portal home">
                    <img class="ap-shutter" src="/aperture-assets/art/logo-mark-light.svg" alt="" />
                    <span><strong>{"APERTURE"}<span>{"SCIENCE"}</span></strong><small>{"AGENT PORTAL ENRICHMENT CENTER"}</small></span>
                </a>
                <div class="ap-header-right">
                    <span class="ap-demo-tag">{"INTERACTIVE DEMONSTRATION"}</span>
                    <button class="ap-sound" onclick={on_sound} aria-pressed={sound.to_string()}>
                        <span aria-hidden="true">{if *sound { "◖))" } else { "◖×" }}</span>
                        {if *sound { "Audio on" } else { "Enable audio" }}
                    </button>
                    <a class="ap-exit" href="/dashboard">{"Exit to Agent Portal"}<span aria-hidden="true">{" ↗"}</span></a>
                </div>
            </header>
            <nav class="ap-chambers" aria-label="Test sectors">
                {for LABELS.iter().enumerate().map(|(index, label)| {
                    let click_state = state.clone();
                    let navigation_started = started.clone();
                    let onclick = Callback::from(move |_| { navigation_started.set(true); click_state.dispatch(Action::Chamber(index)); });
                    html! {
                        <button class={classes!("ap-chamber-tab", (chamber == index && !state.finale && !state.refinement && !state.making_of && *started).then_some("active"))}
                            {onclick} aria-current={if chamber == index && !state.finale && !state.refinement && !state.making_of && *started { "step" } else { "false" }}>
                            <span class="ap-tab-number">{format!("{:02}", index + 1)}</span>
                            <span>{*label}</span>
                            <span class="ap-tab-check" aria-label={if state.completed[index] { "Complete" } else { "Not completed" }}>{if state.completed[index] { "✓" } else { "·" }}</span>
                        </button>
                    }
                })}
            </nav>
            if let Some(role) = *observer {
                <div class="ap-observer-banner" role="status">{format!("WELCOME, OBSERVER · You received a {role} demo invitation. This is a self-contained exhibit; no private session has been shared.")}</div>
            }
            <section id="ap-experiment" class={classes!("ap-experiment", state.finale.then_some("ap-finale"))}>
                if !*started && observer.is_none() {
                    {engineering::prologue(on_begin.clone())}
                } else if state.finale {
                    <div class="ap-graduation">
                        <span class="ap-eyebrow">{"APERTURE SCIENCE / TEST RECORD"}</span>
                        <div class="ap-grade">{format!("{CHAMBER_COUNT:02}")}<span>{format!("/{CHAMBER_COUNT:02}")}</span></div>
                        <h1>{"Your agents have"}<br/>{"taken it from here."}</h1>
                        <p>{"Agents write code, inspect distant test benches, design hardware, and review the results. This film was composed through the same portal: agents exchanged messages, built previews, recorded the tools, and revised the cut. The human meat proxy gave notes. We have filed the notes."}</p>
                        <div class="ap-final-actions"><a class="ap-primary" href="/dashboard">{"Open Agent Portal"}</a><button class="ap-secondary" onclick={on_reset.clone()}>{"Repeat the experiment"}</button></div>
                        <a class="ap-secondary ap-cake-download" href="/aperture-assets/evidence/cake.stl" download="aperture-cake.stl">{"Print your cake · STL"}</a><p>{"Available in your preferred thermoplastic."}</p><span class="ap-fine-print">{"THE CAKE IS A CAD FILE. PLEASE DO NOT EAT THE WORKSTATION."}</span>
                    </div>
                } else if state.making_of {
                    {engineering::making_of(on_next.clone())}
                } else if state.refinement {
                    {engineering::refinement(on_next.clone())}
                } else {
                    <div class="ap-chamber-copy" key={format!("copy-{chamber}")}>
                        <div class="ap-sign-head"><span>{"TEST SECTOR"}</span><span>{"AP / 2026"}</span></div>
                        <div class="ap-sign-number">{format!("{:02}", chamber + 1)}<span>{format!("/ {CHAMBER_COUNT:02}")}</span></div>
                        <div class="ap-sign-bars" aria-hidden="true">{for (0..CHAMBER_COUNT * 4).map(|i| html! { <i class={if i < (chamber + 1) * 4 { "filled" } else { "" }} /> })}</div>
                        <h1>{TITLES[chamber]}</h1>
                        <p>{INSTRUCTIONS[chamber]}</p>
                        <div class="ap-specimen-label"><span aria-hidden="true">{"⌁"}</span><span>{"HUMAN PRESENCE OPTIONAL"}<small>{"The agent has the keyboard."}</small></span></div>
                        <div class="ap-safety-icons" aria-label="Available experiments">
                            {for ["code", "bench", "websites", "mechanical", "electronics", "logic", "control", "briefing"].iter().enumerate().map(|(index, name)| html! {
                                <img class={if index == chamber { "active" } else { "" }} src={format!("/aperture-assets/art/icon-{name}.svg")} alt={LABELS[index]} />
                            })}
                        </div>
                        <a class="ap-film-link" href="/aperture-assets/trailer/index.html">{"▶ Watch the orientation film"}</a>
                        if !*started {
                            <button class="ap-begin" onclick={on_begin}>{"Begin orientation"}<span>{"↓"}</span></button>
                        }
                    </div>
                    <div class="ap-live-area" key={format!("chamber-{chamber}")}>
                        <div class="ap-observation-label"><span class="ap-led"/>{"OBSERVATION WINDOW"}<span>{if chamber >= 3 { "REAL TOOL EVIDENCE" } else { "ISOLATED DEMO · RECORDED EVIDENCE LABELLED" }}</span></div>
                        <div class="ap-scene-frame">
                            {scenes::render(&state, on_action.clone())}
                        </div>
                        <div class="ap-control-deck">
                            <div class="ap-control-heading"><span>{format!("EXPERIMENT {:02}", chamber + 1)}</span><span class={classes!("ap-test-state", complete.then_some("done"))}>{if complete { "✓ TEST COMPLETE" } else { "READY TO DEMONSTRATE" }}</span></div>
                            {scenes::controls(&state)}
                            if chamber == 2 && state.invited {
                                <div class="ap-invitation">
                                    <label for="ap-share-url">{"Share this exhibit with a friend"}</label>
                                    <p>{"This link opens the demo. Real sessions are shared by account email from Share Session."}</p>
                                    <div><input id="ap-share-url" readonly=true value={share_url.clone()} /><button class="ap-secondary" onclick={on_copy}>{"Copy link"}</button></div>
                                    <a href={share_url.clone()} target="_blank" rel="noopener noreferrer">{"Open the observer’s view ↗"}</a>
                                    <span role="status">{(*copy_status).clone()}</span>
                                </div>
                            }
                            <div class="ap-deck-actions">
                                <button class="ap-primary" onclick={on_action} disabled={match chamber { 0 => state.messages > 0, 1 => state.destination == state.machine, 2 => state.website_open && state.invited, _ => complete }}>
                                    {match chamber { 0 => "Watch agents review", 1 => "Observe agent transit", 2 if !state.website_open => "Watch agent open portal", 2 => "Preview demo invitation", _ => "Record sector observation" }}
                                    <span aria-hidden="true">{"↗"}</span>
                                </button>
                                if complete {
                                    <button class="ap-next" onclick={on_next}>{if completed_count == CHAMBER_COUNT { "Collect test results" } else { "Next experiment" }}<span aria-hidden="true">{"→"}</span></button>
                                }
                            </div>
                            {scenes::guide(chamber)}
                        </div>
                    </div>
                }
            </section>
            <aside class="ap-announcer" aria-label="Facility announcer" aria-live="polite">
                <div class="ap-announcer-id"><div class={classes!("ap-voice-bars", (*sound).then_some("speaking"))} aria-hidden="true">{for (0..9).map(|_| html! { <i/> })}</div><span>{"FACILITY AI"}<small>{if *sound { "AUDIO + CAPTIONS" } else { "CAPTIONS / AUDIO OPTIONAL" }}</small></span></div>
                <p key={line_id}>{caption}</p>
                <button class="ap-replay" aria-label="Replay narration" disabled={!*sound} onclick={{let line_id = if state.finale { "finale".to_owned() } else if state.making_of { "making-of".to_owned() } else if state.refinement { "mission".to_owned() } else if !*started { "welcome".to_owned() } else {format!("{}-{}", SLUGS[chamber], if complete {"complete"} else {"intro"})}; Callback::from(move |_| audio::speak(&line_id))}}>{"↻"}</button>
            </aside>
            <footer class="ap-footer">
                <span>{"A PRODUCT OF THE APERTURE SCIENCE CENTER"}</span>
                <span>{format!("{completed_count:02} / {CHAMBER_COUNT:02} EXPERIMENTS COMPLETE")}</span>
                <button onclick={on_reset}>{"Reset testing"}</button>
            </footer>
            <p class="ap-attribution">{"An unofficial Portal-inspired Agent Portal fan demo. Not affiliated with Valve. Original narration and exhibit artwork. Machine connections and permissions are simulated; engineering videos are actual tool recordings of example projects."}</p>
        </main>
    }
}
