//! Each engineering sector shows actual recorded evidence, with its limits beside it.
use super::model::Experiment;
use yew::prelude::*;

struct Department {
    plugin: &'static str,
    media: &'static str,
    title: &'static str,
    quip: &'static str,
    boundary: &'static str,
}
const DEPARTMENTS: [Department; 5] = [
    Department { plugin: "yapcad", media: "cake", title: "A fully parametric disappointment.", quip: "The cake is not a lie. It is a watertight mesh. We understand this is worse.", boundary: "Actual yapCAD slice-of-cake design and filling revision. Presentation geometry; not food. Available in your preferred thermoplastic." },
    Department { plugin: "kicadmium", media: "boards", title: "A nervous system with fewer meetings.", quip: "From schematic to copper to populated 3D. The agent has acquired more connections. You have acquired a chair.", boundary: "Real bp-test M.2 and Tesla-controller design evidence. Recorded checks and design limitations accompany the source; this is not a manufacturing qualification." },
    Department { plugin: "visilog", media: "visilog", title: "Your intuition now has a waveform.", quip: "Step. Inspect. Break. Fix. We did not need to schedule a feelings retrospective.", boundary: "Actual Visilog counter simulation: live values, breakpoint and waveforms. The native test run is the assertion authority; no physical hardware or timing sign-off." },
    Department { plugin: "unlinked", media: "controls", title: "Feedback without the performance meeting.", quip: "The model improves when the feedback is negative. Humans had a less convenient implementation.", boundary: "Actual justletgo SI control-model reconstruction and simulated traces. Agent-authored model, not vendor source; not a physical stabilization result or a MathWorks-equivalence claim." },
    Department { plugin: "engineering-presentations", media: "briefing", title: "The presenter is coming from inside the portal.", quip: "One agent composed the voice. Another built the exhibit and edited the film. Colleagues supplied evidence from other machines. The human gave notes. We have filed the notes.", boundary: "Actual justletgo engineering deck and review evidence. Simulation and design results remain labelled; presentation polish does not constitute a successful hardware test." },
];
pub fn render(state: &Experiment) -> Html {
    let department = &DEPARTMENTS[state.chamber.saturating_sub(3).min(4)];
    html! {
        <div class="ap-plugins-room">
            <div class="ap-replacement-header"><span class="ap-eyebrow">{"APERTURE SCIENCE / APPLIED OBSOLESCENCE"}</span><h2>{department.title}</h2><span class="ap-concept-stamp">{"RECORDED TOOL EVIDENCE"}</span></div>
            {super::engineering::footage(department.media)}
            <div class="ap-design-caption"><h3>{department.plugin}</h3><p>{department.quip}</p></div>
            <ol class="ap-build-loop"><li>{"BUILD"}<small>{"Author the source"}</small></li><li>{"INSPECT"}<small>{"Look at the evidence"}</small></li><li>{"REVISE"}<small>{"Change the design"}</small></li><li>{"REPEAT"}<small>{"Retire another excuse"}</small></li></ol>
            if state.chamber == 3 {
                <a class="ap-secondary ap-cake-download" href="/aperture-assets/evidence/cake.stl" download="aperture-cake.stl">{"Print your cake · STL"}</a>
            }
            <div class="ap-plugin-evidence"><span>{format!("SECTOR {:02} / {}", state.chamber + 1, department.plugin)}</span><p>{department.boundary}</p><a href="/aperture-assets/evidence/index.html" target="_blank" rel="noopener noreferrer">{"Inspect sources and recorded checks ↗"}</a><a href={format!("https://github.com/meawoppl/agent-portal-plugins/tree/main/{}", department.plugin)} target="_blank" rel="noopener noreferrer">{"Inspect the plugin ↗"}</a></div>
        </div>
    }
}
pub fn controls(state: &Experiment) -> Html {
    let department = &DEPARTMENTS[state.chamber.saturating_sub(3).min(4)];
    html! { <p class="ap-deck-note">{department.boundary}</p> }
}
