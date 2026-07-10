# Agent Portal — Live Demo Script

A live-narrated feature tour. It starts with the **basics** — the things you'd
touch in your first five minutes — and builds toward the **richer** features.
It's about *what the portal does and why you'd want it*, not how it's built.

Each beat has a **[SAY]** line (roughly what to speak), a **[DO]** stage
direction (what to click or type), and a short **Why it matters** where it helps.

⭐ marks highlights; trim the un-starred beats first if you're short on time.
Rough length: ~20–30 min at a relaxed pace, less if you skip the trimmables.

---

## Pre-flight checklist

- [ ] Logged in, on the dashboard, with **a live agent session mid-task** so
      there's motion on screen the moment you start (warm one up early).
- [ ] A **second session** open so the multi-agent beats are real, not described.
- [ ] A throwaway local service ready for the forwarding demo, e.g.
      `python3 -m http.server 8899 --bind 127.0.0.1 -d <dir>` — but **don't start
      it yet**; starting it on stage is the money moment.
- [ ] Your **phone**, logged in to the same portal.
- [ ] Admin account handy if you'll show the admin feature at the end.
- [ ] Big readable font; the portal is dark and projects well.

---

# TIER 1 — The basics: watch it and talk to it
*(The first-five-minutes features. Everyone uses these.)*

## 1.1 — Your agents, in a browser ⭐

**[DO]** Open on the dashboard, a session's output streaming live.

**[SAY]** "This is a coding agent doing real work — but I'm watching it from a
browser, not a terminal. No SSH, no screen-share. The agent runs on a machine
somewhere; this is just my window into it, from anywhere."

**Why it matters:** the agent isn't trapped in the terminal you launched it from.
It's a thing you can check on from any browser.

## 1.2 — Live, streaming output

**[DO]** Let the output stream; scroll up through earlier turns.

**[SAY]** "Everything the agent does streams in live, as it happens. And the whole
history is here — I can scroll back through the entire conversation any time."

## 1.3 — Talk to it ⭐

**[DO]** Type a message in the input bar and send it; show it land as a turn.

**[SAY]** "It's a conversation. I can jump in whenever — answer a question,
redirect it, paste an error I want it to look at. I'm not locked out while it
works."

## 1.4 — Always know what it's doing

**[DO]** Point at the **session pill**: working directory, git branch, PR link.

**[SAY]** "I never have to ask 'wait, where are you working?' Right here it shows
me the folder, the branch, and if it's opened a pull request, a direct link to
it."

## 1.5 — Many agents at once ⭐

**[DO]** Show the session list; switch between two sessions.

**[SAY]** "I usually have a few going at once — one refactoring, one writing
tests, one chasing a bug. They all live here, and I flip between them like tabs."

## 1.6 — It keeps running without you ⭐

**[DO]** Close the tab (or describe it), then reopen the portal and show the
session still there, still progressing.

**[SAY]** "Here's the important one: the agents don't live in my browser. I can
close the tab, shut the laptop, come back tomorrow — they've kept running the
whole time, and I pick right back up where they were."

**Why it matters:** long tasks don't need you babysitting a terminal window.

## 1.7 — On your phone, too ⭐

**[DO]** Pick up your **phone**, open the same session, scroll, maybe send a
message.

**[SAY]** "And it's the same thing on my phone — not a stripped-down version, the
real thing. I can check on an agent, answer its question, nudge it in a new
direction, from anywhere. From a coffee shop, from bed."

---

# TIER 2 — Richer interaction: it meets you halfway
*(Beyond plain chat — the features that make it pleasant, not just functional.)*

## 2.1 — It doesn't just talk, it shows ⭐

**[DO]** Have the agent produce (or show a prepared session with): a formatted
table, a syntax-highlighted code block, and ideally an **image or chart** it
renders inline.

**[SAY]** "The replies aren't walls of plain text. Tables come out formatted, code
is highlighted, math is typeset — and if the agent makes a diagram or a plot, it
shows up as an actual picture, right in the conversation."

## 2.2 — Hand you files ⭐

**[DO]** Click a `Download` link the agent produced.

**[SAY]** "If it builds me something — a report, an export, a generated file — it
gives me a download button. One click and it's on my machine."

## 2.3 — It asks YOU questions ⭐

**[DO]** Trigger (or screenshot) a multiple-choice question card; tap an answer.

**[SAY]** "When the agent needs a decision, it doesn't dump five paragraphs and
hope I read them. It pops a little multiple-choice card — I tap the option I
want and it keeps going. Especially nice on a phone: no typing."

## 2.4 — Talk to it out loud

**[DO]** Tap the **mic** and speak a short instruction (Chromium / Safari).

**[SAY]** "And I don't even have to type. I can just talk to it — voice input,
nothing to set up."

## 2.5 — Notifications and feel (quick hit)

**[DO]** Open Settings ▸ Sounds (and Appearance) briefly.

**[SAY]** "Little touches: it can chime when an agent needs me or finishes, so I
can look away and trust it'll get my attention. And there's the usual
personalization — sounds, appearance — to make it yours."

---

# TIER 3 — Working together: agents as a team
*(The features that turn 'a chatbot' into 'a staff'.)*

## 3.1 — Agents that hand work to each other ⭐⭐

**[DO]** From one session, send a message to your second session (via the input or
the agent doing it); switch to the recipient and show it arrive as a turn.

**[SAY]** "This is where it stops being one assistant and becomes a team. My
agents can message *each other*. One can hand a task to another — 'you write the
tests for what I just built' — or ask another for a status. I'm the manager;
they coordinate."

**Why it matters:** you can split big work across specialized agents that talk,
instead of one agent trying to hold everything.

## 3.2 — Agents on a schedule ⭐

**[DO]** Show (or describe) scheduling a prompt on a cron.

**[SAY]** "I can also put an agent on a schedule. 'Every morning, check whether
our dependencies are stale and open a PR if they are.' It shows up for work
before I do. The agent that runs while you sleep."

## 3.3 — Share what you're seeing ⭐

**[DO]** Show the read-only share option for a session.

**[SAY]** "And I can share a session — read-only — with a teammate. Send them a
link and they watch the agent work, live, without me screen-sharing or them
needing to set anything up. Great for 'come look at what this thing just did.'"

---

# TIER 4 — The showpiece: see and use what agents build
*(Save the best for last. This is the beat people remember.)*

## 4.1 — Set the problem ⭐

**[SAY]** "Agents don't just write code — they build things that *run*. A dev
server, a notebook, a little web app. Normally, to actually *see* that, I'd be
SSH-ing in and forwarding ports and fighting firewalls. Watch how this handles
it instead."

## 4.2 — One command to expose it ⭐

**[DO]** In an agent session, run `agent-portal forward 8899`. Show the single URL
it prints.

**[SAY]** "The agent runs one command and gets a URL it can just hand me. That's
it. No setup on my side, no credentials to wire up."

## 4.3 — The living status chip ⭐⭐

**[DO]** Point at the **forward chip** in the session header (`:8899 ↗`).

**[SAY]** "See this little chip that appeared? That's the forward. Now watch the
color when I start the actual service —"

**[DO]** *Now* start the local server on 8899. Wait ~10 seconds until the chip
turns green and gently pulses.

**[SAY]** "— it's *breathing green*. That's the portal telling me, live, that
something is genuinely up and answering on that port. Hover it —"

**[DO]** Hover; show the app name (e.g. `python3`).

**[SAY]** "— and it even tells me *what's* running. Now if I kill the service —"

**[DO]** Stop the server; wait ~10s; chip goes flat red.

**[SAY]** "— flat red. No more 'is it up? did it crash?' The chip just tells me."

## 4.4 — Preview it without leaving ⭐⭐

**[DO]** Click the chip. The preview window animates open out of the chip.

**[SAY]** "And I don't have to leave the portal to look at the thing. Click the
chip and the app opens *right here*, in a little window — fully live. Streaming,
interactive, all of it working."

**[DO]** Drag the window by its title bar; resize from the corner; collapse with
`▾`, then expand again.

**[SAY]** "It's a real floating window — I drag it, resize it, tuck it out of the
way. And while it's tucked away the app keeps running; it doesn't reload. When I
want the full experience, there's a 'Visit site' button that opens it in a normal
tab."

**[DO]** Click the chip again — window collapses back into the chip.

**[SAY]** "And back into the chip it goes."

## 4.5 — Share it with the world (opt-in) ⭐

**[DO]** Open Settings ▸ Forwarding; show the per-forward public toggle.

**[SAY]** "By default that URL is private — only people who can already see this
session. But if I *want* to show someone — a client, a teammate without an
account — I flip one toggle and it becomes a public link. Like handing someone a
preview of the thing the agent just built, no accounts required."

**Why it matters:** the gap between 'the agent built a demo' and 'my customer is
looking at it' is now one toggle.

## 4.6 — Vanity names (admin, optional) ⭐

**[DO]** (If admin) Admin ▸ Subdomains; assign a friendly name to a forward.

**[SAY]** "And for the polished version — an admin can give a forward a real name.
Instead of a random URL, it's `myapp.our-domain`. Same live app, a link you'd
actually put on a slide."

---

## Close

**[SAY]** "So — start to finish: I watch my agents from any browser or my phone,
they keep running without me, they show me pictures and files and ask me
clean questions, they team up and hand work to each other and run on a schedule,
and when they build something that runs, I can see it, use it, and share it — all
without leaving this one page. That's the portal."

---

## Timing cheat-sheet

| Tier | Beats | ~Time |
|---|---|---|
| 1 — Basics | 1.1–1.7 | 6 min |
| 2 — Richer interaction | 2.1–2.5 | 5 min |
| 3 — Team | 3.1–3.3 | 4 min |
| 4 — Forwarding showcase | 4.1–4.6 | 8 min |

**If short on time,** trim: 2.5 → 2.4 → 4.6 → 3.2. **Never cut** 1.1, 1.5, 3.1,
or the 4.3/4.4 forwarding beats — those are the ones people remember.
