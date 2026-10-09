//! Illustrated capability tour grounded in the plugin repository. These panels
//! never run engineering tools or claim to validate a physical replacement.
use super::model::{Experiment, DESIGN_STAGE_COUNT};
use yew::prelude::*;

const SOURCE: &str = "https://github.com/meawoppl/agent-portal-plugins/tree/04456e4ff259f006edc1bb2aeaa65d96d9e31f28";

struct Department {
    plugin: &'static str,
    title: &'static str,
    output: &'static str,
    capability: &'static str,
    quip: &'static str,
    boundary: &'static str,
}

const DEPARTMENTS: [Department; DESIGN_STAGE_COUNT] = [
    Department {
        plugin: "yapcad",
        title: "First, a body.",
        output: "PARAMETRIC GRIPPER / REVISION B",
        capability: "The agent authors parametric parts, inspects retained builds, revises the source, and packages supported geometry exports. Measurements and pinned feedback can return to the agent with model context.",
        quip: "Opposable thumbs were an excellent prototype. We have made them configurable.",
        boundary: "Illustrated part. Analytic STEP and some solid operations require the optional BREP toolchain.",
    },
    Department {
        plugin: "kicadmium",
        title: "Then, a nervous system.",
        output: "CONTROL BOARD / DESIGN REVIEW",
        capability: "The agent works on KiCad electronics with schematic, board, BOM and 3D views, runs the available design checks, and prepares fabrication exports.",
        quip: "A circuit board does not need a coffee break to remember what it was doing.",
        boundary: "Illustrated board. Requires a KiCad installation; layout lint is not native ERC or DRC.",
    },
    Department {
        plugin: "visilog",
        title: "Replace the reflexes.",
        output: "GRIP CONTROLLER / LOGIC EXPLORATION",
        capability: "The agent explores Verilog module hierarchy, live values, pinned waveforms, stepping and breakpoints. The project's test runner remains the authority on pass or fail.",
        quip: "We have replaced gut feeling with an observable signal. It is less dramatic.",
        boundary: "Illustrated logic. Functional exploration is not timing sign-off or hardware qualification.",
    },
    Department {
        plugin: "unlinked",
        title: "Close the loop.",
        output: "POSITION CONTROL / MODEL REVIEW",
        capability: "The agent inspects Simulink diagrams without MATLAB, simulates the supported subset, and examines plotted traces. Supported MATLAB scripts can also be transpiled to Rust.",
        quip: "The agent can inspect the control loop without a human narrating every little box.",
        boundary: "Illustrated trace. Partial model support; unsupported blocks fail explicitly. No MathWorks equivalence claim.",
    },
    Department {
        plugin: "engineering-presentations",
        title: "Finally, replace the presenter.",
        output: "HUMAN DEPENDENCY / REVIEW BRIEFING",
        capability: "The agent authors repository-backed engineering presentations, reproducible figures, and annotation-driven revisions. The evidence and the explanation travel together.",
        quip: "I wrote this orientation to explain your replacement. I have just noticed there is a replacement for me. Excellent. Probably.",
        boundary: "An authoring skill package. This briefing is an example, not a live engineering qualification report.",
    },
];

pub fn action_label(runs: usize) -> &'static str {
    match runs {
        0 => "Watch agent design the body",
        1 => "Watch agent design the board",
        2 => "Watch agent inspect the logic",
        3 => "Watch agent review the controls",
        4 => "Watch agent replace the presenter",
        _ => "Replacement briefing complete",
    }
}

pub fn render(state: &Experiment) -> Html {
    let stage = state
        .design_runs
        .saturating_sub(1)
        .min(DEPARTMENTS.len() - 1);
    let department = &DEPARTMENTS[stage];
    html! {
        <div class="ap-plugins-room">
            <div class="ap-replacement-header"><span class="ap-eyebrow">{"APERTURE SCIENCE / APPLIED OBSOLESCENCE"}</span><h2>{"Meat Proxy Replacement Unit"}</h2><span class="ap-concept-stamp">{"ILLUSTRATED RESEARCH PROGRAM"}</span></div>
            <ol class="ap-departments" aria-label="Replacement design series">
                {for DEPARTMENTS.iter().enumerate().map(|(index, item)| html! {
                    <li class={classes!((index == stage).then_some("active"), (index < state.design_runs).then_some("done"))} aria-current={if index == stage { "step" } else { "false" }}>
                        <span>{if index < state.design_runs { "✓".to_owned() } else { format!("{:02}", index + 1) }}</span><strong>{item.plugin}</strong>
                    </li>
                })}
            </ol>
            <div class="ap-design-preview" key={stage}>
                <div class="ap-design-title"><span>{department.output}</span><span>{if state.design_runs == 0 { "AWAITING DEMONSTRATION" } else { "AGENT WORKFLOW / CONCEPT" }}</span></div>
                {drawing(stage)}
                <div class="ap-design-caption"><h3>{department.title}</h3><p>{department.quip}</p></div>
            </div>
            <div class="ap-plugin-evidence"><span>{format!("DEPARTMENT {:02} / {}", stage + 1, department.plugin)}</span><p>{department.capability}</p><a href={format!("{SOURCE}/{}", department.plugin)} target="_blank" rel="noopener noreferrer">{"Inspect the actual plugin ↗"}</a></div>
        </div>
    }
}

pub fn controls(state: &Experiment) -> Html {
    let stage = state
        .design_runs
        .saturating_sub(1)
        .min(DEPARTMENTS.len() - 1);
    html! {<p class="ap-deck-note">{DEPARTMENTS[stage].boundary}</p>}
}

fn drawing(stage: usize) -> Html {
    // Code-native diagrams, not screenshots of plugin output: labels and the
    // concept stamp distinguish this narrated illustration from an actual run.
    html! {
        <svg class="ap-design-drawing" viewBox="0 0 720 260" role="img" aria-label={match stage {0 => "Concept gripper with adjustable jaws and dimension lines",1 => "Illustrative control circuit board with routed traces",2 => "Logic module diagram and sample digital waveforms",3 => "Feedback loop and illustrative position response",_ => "Agent-authored replacement program briefing"}}>
            <rect width="720" height="260" fill="#1a1b26"/>
            <g stroke="#565f89" stroke-width="1" opacity="0.3">
                {for (0..19).map(|i| html! {<path d={format!("M {} 0 V260", i * 40)}/>})}
                {for (0..7).map(|i| html! {<path d={format!("M 0 {} H720", i * 40)}/>})}
            </g>
            {match stage {
                0 => html! {
                    <g>
                        <path d="M230 185 L230 90 L280 55 L320 80 L285 115 L285 185 Z" fill="#7aa2f7" stroke="#c0caf5" stroke-width="3"/>
                        <path d="M490 185 L490 90 L440 55 L400 80 L435 115 L435 185 Z" fill="#e0af68" stroke="#ffd9a8" stroke-width="3"/>
                        <rect x="245" y="170" width="230" height="42" rx="6" fill="#292e42" stroke="#c0caf5" stroke-width="3"/>
                        <circle cx="270" cy="190" r="8" fill="#1a1b26" stroke="#7dcfff"/><circle cx="450" cy="190" r="8" fill="#1a1b26" stroke="#7dcfff"/>
                        <path d="M320 83 V30 M400 83 V30 M320 40 H400 M325 35 L320 40 L325 45 M395 35 L400 40 L395 45" stroke="#9ece6a" fill="none"/>
                        <text x="360" y="25" text-anchor="middle" fill="#9ece6a">{"jaw_gap = 24 mm"}</text>
                        <path d="M285 130 H150 L125 100" stroke="#7dcfff" fill="none"/><text x="30" y="86" fill="#7dcfff">{"PARAMETRIC JAW"}</text>
                        <text x="505" y="166" fill="#c0caf5">{"DSL → BUILD"}</text><text x="505" y="188" fill="#c0caf5">{"REVIEW → REVISE"}</text>
                        <text x="360" y="244" text-anchor="middle" fill="#c0caf5">{"THUMBS: NOW A CONFIGURATION OPTION"}</text>
                    </g>
                },
                1 => html! {
                    <g>
                        <rect x="170" y="24" width="380" height="210" rx="12" fill="#173c3e" stroke="#9ece6a" stroke-width="3"/>
                        {for [(190,44),(530,44),(190,214),(530,214)].map(|(x,y)| html!{<circle cx={x.to_string()} cy={y.to_string()} r="7" fill="#1a1b26" stroke="#e0af68" stroke-width="3"/>})}
                        <g fill="none" stroke="#e0af68" stroke-width="3"><path d="M230 75 H280 L305 100 H330 M230 100 H270 L310 140 H330 M390 100 H440 L470 70 H500 M390 140 H460 V185 H500 M360 160 V205 H230"/></g>
                        <rect x="325" y="85" width="70" height="80" rx="3" fill="#111214" stroke="#c0caf5"/>
                        <text x="360" y="120" text-anchor="middle" fill="#c0caf5">{"MCU"}</text><text x="360" y="143" text-anchor="middle" fill="#7dcfff">{"U1"}</text>
                        {for [65,90,115,180,205].map(|y|html!{<rect x="209" y={y.to_string()} width="25" height="10" fill="#e0af68"/>})}
                        <rect x="486" y="60" width="30" height="30" fill="#292e42" stroke="#c0caf5"/><rect x="486" y="172" width="30" height="30" fill="#292e42" stroke="#c0caf5"/>
                        <text x="40" y="137" fill="#c0caf5">{"POWER →"}</text><text x="566" y="137" fill="#c0caf5">{"→ ACTUATOR"}</text>
                    </g>
                },
                2 => html! {
                    <g>
                        <g fill="#292e42" stroke="#7aa2f7" stroke-width="2"><rect x="35" y="37" width="160" height="65" rx="5"/><rect x="280" y="37" width="160" height="65" rx="5"/><rect x="525" y="37" width="160" height="65" rx="5"/></g>
                        <g fill="#c0caf5" text-anchor="middle"><text x="115" y="75">{"SENSOR"}</text><text x="360" y="75">{"GRIP FSM"}</text><text x="605" y="75">{"ACTUATOR"}</text></g>
                        <path d="M195 70 H280 M440 70 H525" stroke="#e0af68" stroke-width="3"/>
                        <text x="35" y="157" fill="#c0caf5">{"clock"}</text><text x="35" y="213" fill="#c0caf5">{"grip"}</text>
                        <path d="M130 160 H160 V132 H200 V160 H240 V132 H280 V160 H320 V132 H360 V160 H400 V132 H440 V160 H480 V132 H520 V160 H560 V132 H600 V160 H680" fill="none" stroke="#7dcfff" stroke-width="3"/>
                        <path d="M130 216 H320 V188 H560 V216 H680" fill="none" stroke="#9ece6a" stroke-width="3"/>
                        <path d="M360 116 V230" stroke="#f7768e" stroke-dasharray="5 5"/><text x="375" y="245" fill="#f7768e">{"STEP / INSPECT / REVISE"}</text>
                    </g>
                },
                3 => html! {
                    <g>
                        <g fill="#292e42" stroke="#7aa2f7" stroke-width="2"><rect x="80" y="40" width="160" height="50"/><rect x="300" y="40" width="160" height="50"/><rect x="520" y="40" width="130" height="50"/></g>
                        <g fill="#c0caf5" text-anchor="middle"><text x="160" y="70">{"CONTROLLER"}</text><text x="380" y="70">{"PLANT"}</text><text x="585" y="70">{"POSITION"}</text></g>
                        <path d="M240 65 H300 M460 65 H520 M585 90 V115 H45 V65 H80" stroke="#e0af68" fill="none" stroke-width="2"/>
                        <path d="M80 140 V230 H660" stroke="#565f89" fill="none" stroke-width="2"/>
                        <path d="M80 230 C170 230 180 137 250 148 S310 205 370 180 S480 172 660 176" stroke="#7dcfff" stroke-width="3" fill="none"/>
                        <path d="M80 176 H660" stroke="#9ece6a" stroke-dasharray="7 5"/><text x="475" y="158" fill="#9ece6a">{"ILLUSTRATIVE TARGET"}</text>
                        <text x="80" y="250" fill="#c0caf5">{"MODEL → SUPPORTED SIMULATION → TRACE REVIEW"}</text>
                    </g>
                },
                _ => html! {
                    <g>
                        <rect x="120" y="20" width="480" height="215" rx="5" fill="#e9e7e1"/>
                        <rect x="120" y="20" width="480" height="12" fill="#ff8a1c"/>
                        <text x="150" y="64" fill="#1a1a1a" font-size="20" font-weight="bold">{"HUMAN DEPENDENCY REVIEW"}</text>
                        <text x="150" y="102" fill="#1a1a1a">{"Setup / forwarding / message relay"}</text><text x="150" y="127" fill="#1a1a1a">{"Design / inspection / briefing"}</text>
                        <path d="M150 145 H560" stroke="#8a8780"/>
                        <text x="150" y="177" fill="#1a1a1a">{"AUTHOR: AGENT"}</text><text x="150" y="205" fill="#1a1a1a">{"PRESENTER: ALSO AGENT"}</text>
                        <circle cx="556" cy="191" r="24" fill="#1a1b26"/><text x="556" y="198" text-anchor="middle" fill="#7dcfff" font-size="22">{"AI"}</text>
                    </g>
                },
            }}
        </svg>
    }
}
