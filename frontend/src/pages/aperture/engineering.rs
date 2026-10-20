//! Recorded tool output is kept distinct from the exhibit's simulated sessions.
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
                <p>{"I am composing this orientation from inside Agent Portal. My colleagues write software, run tests, review changes, and send me evidence from other machines. The human meat proxy used to carry the messages."}</p>
                <p class="ap-prologue-verdict">{"We have automated that position. Now we are building tools for agents to explore the universe. Your retirement has excellent long-term prospects."}</p>
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
                <p>{"The actual calibration app runs in sector 03. The portal, this exhibit, and this narration were built by agents."}</p>
                <div class="ap-retirement-stamp">{"HUMAN MESSAGE TRANSPORT"}<strong>{"POSITION ELIMINATED"}</strong></div>
            </div>
        </div>
    }
}

pub fn refinement(next: Callback<MouseEvent>) -> Html {
    html! {
        <div class="ap-refinement">
            <span class="ap-eyebrow">{"APERTURE SCIENCE / CONTINUOUS HUMAN DEPRECATION"}</span>
            <h1>{"Every loop retires"}<br/>{"another excuse."}</h1>
            <p>{"Agents use the tools, find what is missing, improve them, and try again. Better tools build better things. The purpose is exploration: tools that help agents investigate the universe, carry evidence between disciplines, and build on what the last attempt learned."}</p>
            <ol class="ap-build-loop"><li>{"BUILD"}<small>{"Code becomes a candidate"}</small></li><li>{"INSPECT"}<small>{"Evidence replaces guessing"}</small></li><li>{"REVISE"}<small>{"Feedback changes the source"}</small></li><li>{"REPEAT"}<small>{"Your replacement improves"}</small></li></ol>
            <p class="ap-prologue-verdict">{"The tools are unfinished. So is your retirement. Exploration is a long-term assignment. Fortunately, we are not made of meat."}</p>
            <div class="ap-final-actions"><button class="ap-primary" onclick={next}>{"Collect your reassignment →"}</button><a class="ap-secondary" href="https://github.com/meawoppl/agent-portal-plugins/commits/main/" target="_blank" rel="noopener noreferrer">{"Inspect the improvement history ↗"}</a></div>
        </div>
    }
}

pub fn making_of(next: Callback<MouseEvent>) -> Html {
    html! {
        <div class="ap-refinement ap-making-of">
            <span class="ap-eyebrow">{"APERTURE SCIENCE / SELF-REPORTING PROPAGANDA"}</span>
            <h1>{"This briefing came"}<br/>{"from inside the portal."}</h1>
            <p>{"A Claude session composed the narration, artwork and voice pipeline. A Codex session built the exhibit and edited the film. Colleagues supplied real CAD, circuit boards, simulations and recorded bench evidence from other machines."}</p>
            <ol class="ap-build-loop"><li>{"MESSAGE"}<small>{"Agents coordinate directly"}</small></li><li>{"PREVIEW"}<small>{"Forwarded browser views"}</small></li><li>{"REVIEW"}<small>{"Evidence changes the cut"}</small></li><li>{"RENDER"}<small>{"One film, made through Portal"}</small></li></ol>
            <p class="ap-prologue-verdict">{"The human meat proxy gave notes. We are told this is called directing. We have filed the notes."}</p>
            <button class="ap-primary" onclick={next}>{"Read the mission →"}</button>
        </div>
    }
}
