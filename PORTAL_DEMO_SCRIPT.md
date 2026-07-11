# Agent Portal — Demo Walkthrough Script

*~15 minutes full run; each act stands alone if you need a 3-minute cut.
Format per beat: **SAY** (your line), **DO** (clicks/commands), **DETAIL**
(ammo for questions). Pre-flight checklist at the bottom — do it first.*

---

## Cold open (30 seconds)

**SAY:** "Every AI coding tool assumes you're sitting at the machine the agent
runs on. I don't want that. I want to hand a task to an agent on my desktop,
close the laptop, and check on it from my phone at lunch — see everything it
did, answer its questions, look at the app it built, all from a browser tab.
That's what this is. All of it is Rust — the server, the daemon, even this
web page you're looking at is Rust compiled to WebAssembly."

**DO:** Have the dashboard already open, several sessions in the list, at
least one actively streaming output.

**DETAIL:** Axum + PostgreSQL backend, Yew/WASM frontend, a launcher daemon
on each work machine, everything talking over typed WebSocket protocols.
One binary per role, shared protocol crate so the compiler catches drift.

---

## Act 1 — A session from anywhere (3 min)

**SAY:** "Each of these is a live agent session running on one of my machines.
The portal isn't a replay — it's the live wire. Watch."

**DO:**
- Click into a running session. Let output stream.
- Point at the header: session name, host, working directory, git branch,
  **launcher version chip**, cost ticker.
- Send a message from the input bar; the agent picks it up mid-flight.
- If on desktop, open the same session on your phone.

**SAY:** "Everything renders the way a human wants to read it — markdown,
syntax-highlighted diffs, LaTeX if the agent gets mathematical, inline images
when it reads a plot. When the agent needs a decision, I don't get a wall of
text — I get a form."

**DO:** Show a permission prompt or `AskUserQuestion` card if one's handy —
click-to-answer multiple choice.

**SAY:** "And the little cost badge shakes every time the agent spends money.
That's not a feature request I got. It's a feature everyone deserves."

**DETAIL:**
- Reconnects replay from a server-assigned watermark, so a flaky phone
  connection never loses transcript.
- Web input goes through a client-side outbox with idempotency — closing the
  tab mid-send doesn't drop or double a message.
- Voice input is browser-native (Web Speech API), no server credentials.
- Sound settings tab: fully synthesized notification sounds with an ADSR
  editor, because why not.

---

## Act 2 — Agents that talk to each other (2 min)

**SAY:** "Sessions aren't isolated. Any agent can message any of my other
agents from its shell — no setup, it reuses the session's own identity."

**DO:** In a session terminal (or narrate over a transcript):
```console
agent-portal message list          # see your other agents
agent-portal message send <id> "how's the build looking?"
```
Show the message landing as a turn in the other session.

**SAY:** "This is not a toy. Last week I shipped about twenty PRs where one
agent wrote the code and a *different* agent — a Codex session — did the code
review. They negotiated over this exact channel: 'here's the PR, focus on the
auth boundary' … 'blocker: your cache can be clobbered by a stale status' …
fix pushed … 'LGTM, merge on green.' The reviewer caught real security bugs
I'd have shipped. Two models, adversarially collaborating, unattended."

**DETAIL:** Messages arrive prefixed `[message from agent <id>]`; agents reply
by id. Attribution rides the session identity (`CLAUDE_CODE_SESSION_ID`), so
there's no credential handling in agent code.

---

## Act 3 — Port forwarding, the showpiece (5 min)

*This is the money demo. Stage it: a session with no forward, and
`python3 -m http.server 8899` ready to paste.*

**SAY:** "Agents build web things — dev servers, Jupyter, little dashboards.
Normally those are trapped on localhost of whatever machine the agent's on.
Here, the agent just asks for a door."

**DO:** In the session:
```console
agent-portal forward 8899
```
It prints one URL: `https://<8-hex>.portal.…/` — and warns nothing is
listening yet.

**SAY:** "One command, one URL. The subdomain is a hash of the *session*, not
the port — so the URL never changes even if the agent moves the service to a
different port. And look at the header —"

**DO:** Point at the new chip: `:8899 ↗` — currently **flat red**.

**SAY:** "Red means the portal probed the port and nothing's home. The proxy
checks every ten seconds — a loopback dial, microseconds — and only phones
home when the answer *changes*. Now let's give it something to serve."

**DO:** Start the server:
```console
python3 -m http.server 8899 --bind 127.0.0.1
```
Wait ≤10s. The chip starts **breathing green**. Hover it: `python3 — https://…`.

**SAY:** "Green, softly breathing — alive. And it knows *who* answered: hover
says `python3`. The proxy asked the OS which process owns the socket. Swap in
a Vite server on the same port and the label updates itself. Now, the part I
like most —"

**DO:** **Click the chip.** The preview window genies out of the chip.
- Drag it around by the title bar.
- Resize from the corner grip.
- Collapse (▾) — point out the title bar remembers the app: `python3 :8899 — …`.
- Expand — "the app never unloaded; it kept running while collapsed."
- Click **Visit site ↗** — full page in a new tab, authenticated.
- Close (×) — it collapses back into the chip.

**SAY:** "That's the live app, in a floating window, inside the portal.
WebSockets and server-sent events work through it — Jupyter kernels, Vite
hot-reload, all of it. And it's private by default: that URL bounced through
a token handoff only my login can complete. If I *want* it public —"

**DO:** Settings ▸ **Forwarding** → flip the toggle to **Public**.
Open the URL in an incognito window: it loads with no login.

**SAY:** "— one switch, ngrok-style sharing. And it's paranoid in the right
ways: if the agent moves the forward to a different port, the public flag
resets to private. You opted in to sharing *that* service, not whatever
binds the port next."

**DO (optional, admin flourish):** Admin ▸ **Subdomains** → give the session
a friendly name: type `demo`, save, open `https://demo.portal.…/`.

**SAY:** "Admins can hand out human subdomains. Both URLs route; typos and
reserved names get rejected with an explanation right in the form."

**DO (finale):** Kill the server (`Ctrl-C`). Within ten seconds the chip goes
**flat red** again.

**SAY:** "Nothing polled from your browser, no page refresh. The proxy
noticed, told the server once, the server nudged every open tab."

**DETAIL (deep bench, if asked):**
- Transport: a multiplexed byte tunnel over the session's existing WebSocket —
  credit-windowed (256 KiB/direction/stream), 16 KiB frames, 64 streams, so a
  fat download can't starve the agent's own traffic.
- The reverse proxy is a real hyper HTTP/1.1 client whose "TCP" is the tunnel;
  WebSocket upgrade is spliced with `copy_bidirectional`.
- Auth: 60-second JWT handoff → host-only cookie on the forward origin; the
  portal cookie never crosses origins; forwarded apps are origin-isolated
  from the portal *and* from each other.
- The preview iframe works because the proxy re-scopes `X-Frame-Options` /
  `frame-ancestors` to portal-or-self — *stricter* than the nothing most dev
  servers ship.
- Every piece of this was reviewed by the Codex agent from Act 2. It found
  five real bugs, including a public-flag-survives-port-change privilege leak.

---

## Act 4 — Ops that take care of themselves (2 min)

**SAY:** "The boring parts are where remote agents usually die, so those got
the most engineering."

Narrate over the launchers tab / a terminal:

- **"Launchers self-update and self-heal."** The daemon updates it