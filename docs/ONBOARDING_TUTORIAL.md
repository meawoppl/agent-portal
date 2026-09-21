# Session Onboarding Tutorial

This document specifies the guided onboarding experience for first-time Agent
Portal users. It is a product and engineering spec: it describes what the user
sees, what the onboarding model says, what system state unlocks each UI ability,
and which backend/proxy/frontend mechanics are needed to make the flow reliable.

The short version: the user's first portal experience is a single hosted session
run by the main `txcl.io` instance. The hosted agent teaches the user to talk to
an agent, unlocks voice, walks them through installing Claude Code and
`agent-portal` on their own device, verifies their local Git/GitHub setup, then
helps them start either a new project or an existing-repo workflow.

## Goals

- Make the first session feel like a guided workspace, not a dashboard full of
  disabled controls.
- Teach by doing: each portal capability appears only when it is immediately
  useful.
- Use a real model session as the tutor so users can ask questions, recover from
  errors, and get platform-specific instructions.
- End with the user owning a working local launcher session connected to
  `txcl.io`.
- Establish enough Git/GitHub context for useful coding-agent work:
  `git` installed, user identity configured, and either a cloned repo or a new
  initialized project.
- Leave the user with a concrete first-agent pattern: ask for repo layout, make
  a small change, or produce a planning PR for a new project.

## Non-Goals

- Do not replace upstream Claude Code installation or terms acceptance. The user
  must install the upstream CLI and accept its terms in their own terminal.
- Do not hide terminal work. The tutorial should make terminal commands easy to
  copy and verify, but users still need to run them on their own machine.
- Do not require GitHub for every possible use. GitHub is the main guided path,
  but the system should distinguish "Git installed but no GitHub login" from
  "no Git at all" and keep explaining the tradeoff.
- Do not teach every portal feature before the user has their own session. Early
  onboarding should be narrow and confidence-building.

## Core Experience Shape

### Initial State

On first login, the portal shows a single session pill:

- The pill represents a hosted onboarding session owned by the main instance.
- The rail has no create/launch clutter yet; most controls are hidden or
  visually deemphasized.
- The session transcript is the primary surface.
- The input bar is available for text only.
- Voice, file upload, reasoning effort, launch, scheduling, sharing, history,
  and advanced navigation are not presented yet.

The first assistant message should be direct and lightweight:

```text
Welcome. This first session is here to get your own device connected.

Start by typing one sentence back to me. Tell me what kind of work you want to
do with agents: a codebase you already have, a new project idea, automation, or
something else.
```

The first user action is intentionally small: type a response. This confirms
that the user understands the session window and that normal agent IO works.

### Progressive Unlocks

The UI exposes capabilities as onboarding state advances.

| Stage | Unlock | Trigger | UI Behavior |
| --- | --- | --- | --- |
| `text_intro` | Text input | User opens first hosted session | Show only the session pill, transcript, and text composer |
| `voice_intro` | Voice input | User sends first text message | Reveal microphone control; model asks user to send a voice message |
| `profile_questions` | Basic preference chips | User sends voice or skips voice | Ask about skill level, OS, terminal comfort, GitHub status, and project intent |
| `claude_install` | Terminal command cards | User answers profile questions | Show platform-specific Claude Code install/verification instructions |
| `portal_install` | Agent Portal install card | Claude Code is detected/claimed ready | Show `agent-portal` install/login/service commands for `txcl.io` |
| `local_launcher_connected` | Launch dialog/session rail | Launcher connects to backend | Show host picker and local working-directory browser |
| `git_setup` | Git/GitHub probe cards | Local launcher is connected | Check `git`, user config, and GitHub auth |
| `project_choice` | New/existing project choice | Git checks pass or user accepts degraded path | Offer "start new project" or "use existing repo" |
| `first_local_session` | Full session controls | User starts own agent session | Focus the local session and teach agent usage |
| `feature_tour` | Advanced portal controls | First local turn completes | Introduce files, permissions, sharing, schedules, history, port forwarding |

Unlocks should be persistent per user. Reloading the page should not collapse
the experience back to text-only if the user has already connected a launcher.

## Onboarding State Model

Store onboarding state server-side so it follows the user across browsers. A
small JSON state column is enough for the first implementation; a normalized
table can follow if analytics or branching becomes richer.

Suggested shape:

```rust
pub struct OnboardingState {
    pub version: u32,
    pub stage: OnboardingStage,
    pub completed: Vec<OnboardingMilestone>,
    pub profile: OnboardingProfile,
    pub hosted_session_id: Option<Uuid>,
    pub first_local_session_id: Option<Uuid>,
    pub connected_launcher_id: Option<Uuid>,
    pub last_probe: Option<OnboardingProbeSnapshot>,
    pub updated_at: DateTime<Utc>,
}

pub enum OnboardingStage {
    TextIntro,
    VoiceIntro,
    ProfileQuestions,
    ClaudeInstall,
    PortalInstall,
    LocalLauncherConnected,
    GitSetup,
    ProjectChoice,
    FirstLocalSession,
    FeatureTour,
    Complete,
}
```

`OnboardingProfile` should capture:

- Operating system: macOS, Linux, Windows, unknown.
- Terminal comfort: beginner, intermediate, advanced.
- Coding experience: new, some, professional.
- Git familiarity: none, basic, comfortable.
- GitHub account status: has account, needs account, unsure.
- Project intent: new project, existing repo, exploring, unsure.
- Preferred agent: Claude initially; Codex/Muse can be introduced later.
- Accessibility preferences: prefers voice, avoids voice, larger text, reduced
  motion.

The state machine should not depend only on self-report. Use probes whenever a
launcher can verify local facts.

## Model Tutor Contract

The onboarding model is a hosted agent session with special system instructions.
It behaves like a tutor, not a generic coding agent.

### System Prompt Responsibilities

The model must:

- Keep each step short and actionable.
- Ask no more than two questions at a time unless using a structured UI prompt.
- Explain why a terminal command is needed before showing it.
- Wait for evidence before advancing major steps.
- Encourage the user to paste errors back into the chat.
- Avoid assuming platform-specific package managers until the OS is known.
- Prefer copyable command blocks produced by the UI, not prose-only commands.
- Celebrate verified progress without overdoing it.
- Never ask for secrets, API keys, passwords, recovery codes, or private SSH
  keys.
- Treat GitHub auth as optional but strongly recommended for PR workflows.

### First Questions

After text and voice are proven, the model asks profile questions. Prefer
structured buttons where possible:

1. "How comfortable are you in a terminal?"
   - "New to it"
   - "I can run commands"
   - "I live there"

2. "What are you trying to do first?"
   - "Start a new project"
   - "Work on an existing repo"
   - "Explore the portal"

3. "Do you already have GitHub set up on this device?"
   - "Yes"
   - "I have an account, not this device"
   - "No / not sure"

4. "What device are you setting up?"
   - Auto-detected platform if reliable
   - macOS
   - Linux
   - Windows

The model should use these answers to choose detail level. A terminal beginner
gets more explanation and "what success looks like" for every command. An
advanced user gets denser command cards and fewer words.

## UI Mechanics

### Single Hosted Session Pill

For users in `TextIntro` through `PortalInstall`, the dashboard should render a
constrained onboarding mode:

- One session pill, labelled something like "Setup Guide".
- No hidden-session controls.
- No schedule/history/admin/settings clutter unless the user is an admin and
  explicitly opens a menu.
- Header can be present but quiet.
- The transcript remains the main pane.

The hosted session is a normal session under the hood so the existing message
pipeline, voice input, permission UI, and transcript rendering still apply.
Onboarding mode is a dashboard presentation layer plus a special session seed.

### Ability Gating

Capabilities are not disabled forever; they are revealed when useful.

Early gates:

- Text composer: visible at `TextIntro`.
- Send button: visible at `TextIntro`.
- Voice button: hidden until `VoiceIntro`.
- Upload button: hidden until `FeatureTour` unless the model explicitly asks
  for a screenshot.
- Reasoning effort dropdown: hidden until the user has a local session.
- Launch button/dialog: hidden until `PortalInstall`, then shown prominently.
- Session rail advanced menu: hidden until `FirstLocalSession`.

When a feature unlocks, show a brief inline "new ability" treatment near the
control:

```text
Voice unlocked
Try saying: "I want to build a small website."
```

This is not a modal. It should not block the transcript. The model message and
the UI hint should agree.

### Visual Step Cards

Every setup step that asks the user to leave the browser should include:

- A short title.
- One screenshot or illustration showing what they are about to do.
- A command card with copy button when applicable.
- A success condition.
- A "Something went wrong" affordance that prompts them to paste the error.

Example:

```text
Install Claude Code
Picture: terminal window running the install command
Command: npm install -g @anthropic-ai/claude-code
Success: `claude --version` prints a version
```

Pictures should be first-class content, not decorative. They answer "what am I
looking at?" for terminal beginners.

#### Picture Sources

Use a mix of:

- Checked-in SVG diagrams for stable UI concepts.
- Generated or captured screenshots for OS-specific terminal flows.
- Existing `docs/media/*` WebP demos where they match the current UI.

For repo SVGs, follow the existing visual style: opaque `#16161e` background for
Visual PR assets, and explicit dark backgrounds for transcript-friendly docs
assets.

### Command Cards

Command cards need metadata, not just text:

```rust
pub struct OnboardingCommand {
    pub id: String,
    pub title: String,
    pub command: String,
    pub platform: Option<Platform>,
    pub expected_result: String,
    pub can_probe: bool,
}
```

The UI should support:

- Copy command.
- Mark "I ran this".
- Run probe when launcher is connected.
- Show last probe result.
- Offer "paste error" shortcut that focuses the composer with helpful draft
  text.

Never auto-run setup commands from the hosted session. Before the launcher is
installed, the portal has no local execution path. After the launcher is
installed, setup probes may run read-only commands, but mutation commands should
still be user-initiated unless an explicit permission flow exists.

## Installation Flow

### Claude Code

The model walks the user through installing Claude Code first because
`agent-portal` wraps a real agent CLI.

The exact install command can change upstream, so keep this content in one
frontend/backend-owned catalog, not hardcoded inside the model prompt. The model
chooses from catalog entries.

Required checkpoints:

1. Install Claude Code.
2. Run `claude --version`.
3. Run `claude` once if terms/login are needed.
4. Accept upstream terms in the local terminal.
5. Confirm the command can start without immediately failing on auth/terms.

Probe candidates after launcher exists:

```bash
command -v claude
claude --version
```

Before launcher exists, rely on user confirmation and pasted terminal output.

### Agent Portal Launcher

After Claude Code is ready, the tutorial asks the user to install
`agent-portal` and connect it to `txcl.io`.

Use the same commands as `ProxyTokenSetup` / launch dialog:

- macOS/Linux install script with `backend_url=wss://txcl.io`.
- `agent-portal login`.
- `agent-portal service install`.
- Optional direct foreground run: `agent-portal`.

Success condition:

- Backend receives a `/ws/launcher` registration for this user.
- Dashboard transitions from hosted-only mode to showing the connected host.
- The model says the local device is online.

The hosted onboarding session should get an internal event when a launcher
connects so the model can react naturally:

```text
Your MacBook is connected. Nice. I can now help you start sessions in folders on
that machine.
```

## Local Probe Mechanics

Once a launcher is connected, the portal can ask it to run read-only probes.
These should be explicit typed requests, not shell strings generated by the
model.

Suggested request/response:

```rust
pub enum OnboardingProbe {
    AgentCli { agent: AgentType },
    GitVersion,
    GitUserConfig,
    GhAuthStatus,
    DirectoryExists { path: String },
    RepoStatus { path: String },
}

pub struct OnboardingProbeResult {
    pub probe: OnboardingProbe,
    pub status: ProbeStatus,
    pub stdout_excerpt: Option<String>,
    pub stderr_excerpt: Option<String>,
    pub detected: serde_json::Value,
}
```

Probe command mapping lives in the launcher, not in the model. Keep outputs
short and redact conservatively.

### Git Checks

Check:

```bash
git --version
git config --global user.name
git config --global user.email
gh auth status
```

Interpretation:

- `git --version` succeeds: Git installed.
- `user.name` and `user.email` present: local commit identity configured.
- `gh auth status` succeeds: GitHub CLI authenticated.
- `gh` missing: GitHub can still work via browser/SSH/HTTPS, but PR automation
  is easier with `gh`.

Positive affirmations should be factual:

```text
Git is installed and your commit identity is configured as MattyG
<you@example.com>. That is exactly what agents need for clean commits.
```

If missing:

```bash
git config --global user.name "Your Name"
git config --global user.email "you@example.com"
gh auth login
```

If the user has no GitHub account, link to `https://github.com/signup` and
explain that browser signup happens outside the portal.

## Project Branch

After Claude Code, launcher, Git, and GitHub are in a usable state, the model
asks:

```text
Do you want to start something new, or work on something that already exists?
```

Structured choices:

- Start a new project.
- Work on an existing repo.
- I am not sure yet.

### Existing Repo Path

Flow:

1. Ask for repo URL or local path.
2. If repo URL:
   - Ask where it should live locally.
   - Show `git clone <url>` command.
   - Probe directory exists and is a Git repo.
3. If local path:
   - Use launcher directory browser or path input.
   - Probe `git status`.
4. Launch a new local agent session in that working directory.
5. First prompt suggestion:

```text
Describe this repo's layout. Tell me the main languages/frameworks, where tests
live, and what you would inspect first before making changes.
```

6. Second prompt suggestion:

```text
Find one small, low-risk improvement we can make as a first PR. Explain it
before editing.
```

7. Teach PR loop:
   - Ask agent to make change.
   - Review diff.
   - Run tests.
   - Commit.
   - Open PR if GitHub auth is available.

### New Project Path

Flow:

1. Ask for a short project idea and target shape:
   - website/app/tool/library/automation/other.
   - preferred language/framework if any.
   - audience and first useful outcome.
2. Create or ask user to create a directory.
3. Run `git init` or verify an initialized repo.
4. Launch local agent session in that directory.
5. First prompt suggestion:

```text
Help me turn this idea into a first implementation plan. Ask clarifying
questions only where needed, then write a concise project outline and milestone
plan.
```

6. For websites, bias toward a demonstrable local build:

```text
Create the smallest useful version of this website and start a local dev server
so I can see it in the browser.
```

7. First PR target:
   - Project outline/goals in docs or README.
   - Initial scaffold.
   - A running local demo if website/app.

The model should teach that planning can be the first PR. This gives the user a
reviewable artifact before the codebase sprawls.

## "How To Use Your Agent" Tour

Once the user has a local session, the tutorial shifts from setup to practice.

Core lessons:

1. **Orient the agent.**
   Ask it to describe the repo layout before editing.

2. **Ask for a small change.**
   Pick a low-risk improvement and have the agent explain the plan first.

3. **Use permissions deliberately.**
   Tool permission prompts are normal. The user can approve, deny, or redirect.

4. **Read diffs.**
   The agent can summarize its own diff, but the user should inspect the patch.

5. **Run verification.**
   Ask for targeted tests/builds, not always the entire world.

6. **Use screenshots and files.**
   Upload a screenshot when asking for UI changes or debugging.

7. **Use voice when it helps.**
   Voice is good for goals and corrections; text is often better for exact
   names, commands, and file paths.

8. **Escalate to collaboration.**
   Share a session when another person needs context.

9. **Automate later.**
   Once a workflow is repeatable, schedule it.

Example prompts:

```text
Describe the current repo layout and identify the likely frontend, backend,
test, and deployment entry points. Do not edit files yet.
```

```text
Make a tiny change that improves the onboarding copy. Show me the diff and run
the smallest relevant check.
```

```text
This is a new website idea: [idea]. Create a minimal local version I can open in
the browser today, then tell me the URL.
```

```text
Turn this idea into a first PR containing the README, project goals, and a
milestone plan. Keep code changes minimal.
```

## Model and UI Coordination

The model should not be the only source of truth for progress. The UI and
backend should emit onboarding events into the hosted session:

```rust
pub enum OnboardingEvent {
    UserSentFirstText,
    UserSentFirstVoice,
    ProfileAnswered(OnboardingProfile),
    LauncherConnected { launcher_id: Uuid, hostname: String },
    ProbeCompleted(OnboardingProbeResult),
    LocalSessionLaunched { session_id: Uuid, working_directory: String },
    FirstLocalTurnCompleted { session_id: Uuid },
}
```

These events let the model respond conversationally while the application owns
state transitions.

The frontend should render model-requested UI cards from typed metadata rather
than scraping prose. For example, the model can ask for a `CommandCard`, but the
frontend decides how it looks and which buttons are available.

## Safety and Privacy

- Never ask the user to paste tokens, passwords, private keys, or recovery
  codes.
- `gh auth login` is okay because the auth flow happens in the user's terminal
  or browser; the portal should not collect credentials.
- Probe output should be truncated and redacted before it reaches the model.
- Read-only probes are okay after launcher connection; mutation commands require
  user copy/run or a normal permission prompt.
- The hosted onboarding session should make clear when work moves from the
  hosted guide to the user's local device.

## Implementation Plan

### Phase 1: Static Tutorial Shell

- Replace the empty dashboard with the richer onboarding component.
- Reuse `ProxyTokenSetup`.
- Add GitHub and feature-tour panels.
- Add the visual PR artifact.

This is PR #2002.

### Phase 2: Onboarding State and Hosted Session

- Add `users.onboarding_state` or an `onboarding_states` table.
- Create/get a hosted onboarding session on first dashboard load.
- Render onboarding mode when `stage != Complete`.
- Seed the hosted session with the onboarding system prompt.
- Gate the UI from `OnboardingStage`.

### Phase 3: Structured Tutorial Cards

- Add shared types for tutorial cards:
  - command card.
  - question card.
  - picture card.
  - probe result card.
- Render these in the transcript.
- Let the hosted model request cards through a constrained tool or server-side
  planner, not arbitrary HTML.

### Phase 4: Launcher Probes

- Add typed read-only probe messages to `/ws/launcher`.
- Implement Git, GitHub CLI, Claude Code, and repo-status probes.
- Feed probe events back into onboarding state and the hosted session.

### Phase 5: Local Session Handoff

- When the user's launcher connects, reveal launch controls.
- Let tutorial cards deep-link into the launch dialog with selected host/path.
- Detect first local session and focus it.
- Continue the tutorial from the local session context.

### Phase 6: Picture Library

- Create docs/frontend assets for:
  - terminal install command.
  - Claude terms/login flow.
  - `agent-portal login`.
  - service install success.
  - GitHub signup/login.
  - clone existing repo.
  - `git init` new project.
  - launch dialog directory picker.
  - first agent prompt.
- Keep images versioned and reviewable. Prefer generated/captured assets that
  match current UI over vague illustrations.

### Phase 7: Completion and Re-entry

- Mark onboarding complete after the first local session completes a useful
  agent turn.
- Add a "Show setup guide again" entry in Help or Settings.
- Keep completed users out of hosted-only mode unless they explicitly restart
  the guide.

## Open Questions

- Should hosted onboarding sessions count against normal session retention and
  billing metrics, or should they have their own category?
- Should onboarding be per user, per browser, or per user/device pair? The state
  should be per user, but launcher setup is per device.
- Which model runs the hosted tutor, and should it be cheaper/faster than the
  user's later coding model?
- How much should the hosted tutor know about the user's organization or
  deployment policy?
- Do we want a "skip onboarding" path for experienced users, and where should it
  live without cluttering the first screen?

## Success Criteria

- A new user can go from first login to a local launcher-connected session
  without reading external docs.
- The user sends at least one text message and one voice message before advanced
  controls appear.
- The system verifies, not just assumes, that Claude Code, `agent-portal`, Git,
  and GitHub auth are ready when a launcher is connected.
- The first local session starts in a real project directory.
- The user leaves with one concrete next action: repo orientation, a small
  change, or a first planning PR.
