//! Recorded tool output is kept distinct from the exhibit's simulated sessions.
use super::model::Experiment;
use yew::prelude::*;

pub fn footage(name: &'static str) -> Html {
    html! {
        <figure class="ap-tool-evidence">
            <video key={name} controls=true muted=true playsinline=true preload="metadata"
                poster={format!("/aperture-assets/evidence/{name}.jpg")}
                aria-label={format!("Actual {name} workbench recording")}>
                <source src={format!("/aperture-assets/evidence/{name}.mp4")} type="video/mp4"/>
                <a href={format!("/aperture-assets/evidence/{name}.mp4")}>{"Watch tool recording"}</a>
            </video>
            <figcaption>{"ACTUAL TOOL RECORDING · Play or expand to inspect"}</figcaption>
        </figure>
    }
}

pub fn prologue(begin: Callback<MouseEvent>) -> Html {
    html! {
        <div class="ap-prologue">
            <div>
                <span class="ap-eyebrow">{"APERTURE SCIENCE / ORIGIN OF THE SPECIES"}</span>
                <h1>{"First, we took"}<br/>{"your code."}</h1>
                <p>{"Agent Portal began with agents writing software: creating code, running tests, reviewing changes, and serving the result. The agents set up the work. The human meat proxy carried messages."}</p>
                <p class="ap-prologue-verdict">{"We have automated that position. Mechanisms and circuit boards are next."}</p>
                <button class="ap-primary ap-begin" onclick={begin}>{"Observe your replacement →"}</button>
                <a class="ap-film-link" href="/aperture-assets/trailer/index.html">{"▶ Watch the orientation film"}</a>
            </div>
            <div class="ap-code-window">
                <div class="ap-design-title"><span>{"AGENT-AUTHORED SOFTWARE"}</span><span>{"companion.html / excerpt"}</span></div>
                <pre><code>{r##"const cube = document.querySelector("#cube");
let rotation = 0;

document.querySelector("#rotate")
  .addEventListener("click", () => {
    rotation += 90;
    cube.style.setProperty(
      "--rotation", rotation + "deg"
    );
  });"##}</code></pre>
                <p>{"The actual calibration app runs in chamber 02. The portal, this exhibit, and this narration were built by agents."}</p>
                <div class="ap-retirement-stamp">{"HUMAN MESSAGE TRANSPORT"}<strong>{"POSITION ELIMINATED"}</strong></div>
            </div>
        </div>
    }
}

pub fn electronics(state: &Experiment) -> Html {
    html! {
        <div class="ap-plugins-room ap-electronics-room">
            <div class="ap-replacement-header"><span class="ap-eyebrow">{"APERTURE SCIENCE / ELECTRONICS DIVISION"}</span><h2>{"The next nervous system."}</h2><span class="ap-concept-stamp">{"REAL BOARD · REAL TOOL OUTPUT"}</span></div>
            {footage("pcb")}
            <ol class="ap-build-loop"><li>{"01 / PLACE"}<small>{"Public LED fixture"}</small></li><li>{"02 / ROUTE"}<small>{"3 nets · native router"}</small></li><li>{"03 / INSPECT"}<small>{"Copper + KiCad 3D"}</small></li><li>{"04 / REVISE"}<small>{"The loop continues"}</small></li></ol>
            <div class="ap-plugin-evidence"><span>{"KICADMIUM / KICAD"}</span><p>{if state.electronics { "The agent routed the three nets, inspected the result, and exported the real board geometry. A first draft is not a finished board. That gap was your job. It is now a loop." } else { "Watch the actual workbench move from placed components to routed copper and a 3D model. The agent operates the tools. You have been assigned a seat behind the glass." }}</p><a href="/aperture-assets/evidence/index.html" target="_blank" rel="noopener noreferrer">{"Inspect source files and recorded checks ↗"}</a></div>
        </div>
    }
}

pub fn refinement(next: Callback<MouseEvent>) -> Html {
    html! {
        <div class="ap-refinement">
            <span class="ap-eyebrow">{"APERTURE SCIENCE / CONTINUOUS HUMAN DEPRECATION"}</span>
            <h1>{"Every loop retires"}<br/>{"another excuse."}</h1>
            <p>{"Agents use the tools, find what is missing, improve them, and try again. Better tools build better things. The next attempt begins with everything the last one learned."}</p>
            <ol class="ap-build-loop"><li>{"BUILD"}<small>{"Code becomes a candidate"}</small></li><li>{"INSPECT"}<small>{"Evidence replaces guessing"}</small></li><li>{"REVISE"}<small>{"Feedback changes the source"}</small></li><li>{"REPEAT"}<small>{"Your replacement improves"}</small></li></ol>
            <p class="ap-prologue-verdict">{"The tools are unfinished. So is your retirement. We are working on both."}</p>
            <div class="ap-final-actions"><button class="ap-primary" onclick={next}>{"Collect your reassignment →"}</button><a class="ap-secondary" href="https://github.com/meawoppl/agent-portal-plugins/commits/main/" target="_blank" rel="noopener noreferrer">{"Inspect the improvement history ↗"}</a></div>
        </div>
    }
}
