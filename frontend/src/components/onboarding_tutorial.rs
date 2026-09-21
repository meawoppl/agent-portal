use crate::components::{CopyCommand, ProxyTokenSetup};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct OnboardingTutorialProps {
    pub on_open_launch: Callback<()>,
}

#[function_component(OnboardingTutorial)]
pub fn onboarding_tutorial(props: &OnboardingTutorialProps) -> Html {
    let open_launch = {
        let on_open_launch = props.on_open_launch.clone();
        Callback::from(move |_| on_open_launch.emit(()))
    };

    html! {
        <section class="onboarding-tutorial" aria-label="Agent Portal onboarding">
            <div class="onboarding-hero">
                <div>
                    <p class="onboarding-kicker">{ "Setup guide" }</p>
                    <h2>{ "Bring a device online, then launch your first agent session." }</h2>
                    <p class="onboarding-lede">
                        { "This walkthrough stays in the session workspace while you connect a machine, prepare GitHub, and learn the daily controls." }
                    </p>
                </div>
                <button type="button" class="onboarding-primary" onclick={open_launch}>
                    { "Open launcher setup" }
                </button>
            </div>

            <div class="onboarding-grid">
                <section class="onboarding-panel onboarding-install-panel">
                    <div class="onboarding-panel-header">
                        <span class="onboarding-step-badge">{ "1" }</span>
                        <div>
                            <h3>{ "Install Agent Portal on this device" }</h3>
                            <p>{ "Run these commands once on the machine that should host agent sessions." }</p>
                        </div>
                    </div>
                    <ProxyTokenSetup />
                </section>

                <section class="onboarding-panel">
                    <div class="onboarding-panel-header">
                        <span class="onboarding-step-badge">{ "2" }</span>
                        <div>
                            <h3>{ "Prepare GitHub" }</h3>
                            <p>{ "Use a GitHub account so agents can clone repositories and open pull requests." }</p>
                        </div>
                    </div>
                    <div class="onboarding-action-list">
                        <a
                            class="onboarding-link-action"
                            href="https://github.com/signup"
                            target="_blank"
                            rel="noopener noreferrer"
                        >
                            { "Create or sign in to GitHub" }
                        </a>
                        <CopyCommand command={"gh auth login".to_string()} />
                        <CopyCommand command={"git clone https://github.com/OWNER/REPO.git".to_string()} />
                    </div>
                </section>

                <section class="onboarding-panel">
                    <div class="onboarding-panel-header">
                        <span class="onboarding-step-badge">{ "3" }</span>
                        <div>
                            <h3>{ "Launch a session" }</h3>
                            <p>{ "Pick a connected host, choose a working directory, then start Claude, Codex, or Muse." }</p>
                        </div>
                    </div>
                    <button type="button" class="onboarding-secondary" onclick={
                        let on_open_launch = props.on_open_launch.clone();
                        Callback::from(move |_| on_open_launch.emit(()))
                    }>
                        { "Launch session" }
                    </button>
                </section>

                <section class="onboarding-panel onboarding-feature-panel">
                    <div class="onboarding-panel-header">
                        <span class="onboarding-step-badge">{ "4" }</span>
                        <div>
                            <h3>{ "Try the portal features" }</h3>
                            <p>{ "Once the first session is running, these are the controls people tend to use first." }</p>
                        </div>
                    </div>
                    <div class="onboarding-feature-grid">
                        <div class="onboarding-feature">
                            <strong>{ "Prompt bar" }</strong>
                            <span>{ "Send text, choose send mode, attach files, or set reasoning effort." }</span>
                        </div>
                        <div class="onboarding-feature">
                            <strong>{ "Voice" }</strong>
                            <span>{ "Dictate a prompt with the microphone button when speech-to-text is enabled." }</span>
                        </div>
                        <div class="onboarding-feature">
                            <strong>{ "Permissions" }</strong>
                            <span>{ "Answer tool approval prompts without leaving the transcript." }</span>
                        </div>
                        <div class="onboarding-feature">
                            <strong>{ "Share" }</strong>
                            <span>{ "Invite collaborators or send agents messages from another session." }</span>
                        </div>
                        <div class="onboarding-feature">
                            <strong>{ "Schedule" }</strong>
                            <span>{ "Create recurring prompts for a repository once a launcher is connected." }</span>
                        </div>
                        <div class="onboarding-feature">
                            <strong>{ "History" }</strong>
                            <span>{ "Browse archived sessions when the server has archival storage enabled." }</span>
                        </div>
                    </div>
                </section>
            </div>
        </section>
    }
}
