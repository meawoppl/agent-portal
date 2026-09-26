# Split Forwarder and Electronics Session Plan

This document lays out the durable session work surface used by forwarded apps
today and the path toward PCB/electronics workflows inspired by the KiCad PCB
plugin and PasteBOM.

## Objective

Make forwarded HTTP apps first-class session surfaces.

The immediate user-visible change is that clicking a forward chip should open a
resizable split view, normally occupying about half of the session window,
instead of a floating overlay. The longer arc is to let sessions host rich
workflows where chat, local tools, and visual artifacts are visible at the same
time: PCB viewers, generated sites, notebooks, dashboards, test UIs, and setup
flows.

For electronics, this split view becomes the place where an agent and user
inspect a board, schematic, Gerber package, BOM, or 3D render while the
conversation remains live.

## Current State

Port forwarding already has strong transport and auth foundations:

- one active forward per session;
- subdomain-based origins rather than path prefixes;
- handoff auth from the portal origin to the forward origin;
- iframe preview support;
- WebSocket and SSE support;
- forward chips in the session header;
- owner-only revoke;
- live port health and process-name reporting.

The session UI separates:

- forward discovery and chip controls (`ForwardChips`);
- generic session surface state (`SessionSurface`);
- iframe host chrome (`ForwardSurface`);
- split/fullscreen layout policy in `SessionView`;
- future artifact surfaces that are not plain forwarded apps.

## Product Direction

Use three display modes:

| Mode | Purpose | Default use |
| --- | --- | --- |
| Inline card | A small durable transcript artifact | Thumbnails, DRC summaries, BOM excerpts |
| Split surface | A live co-working pane beside chat | Forwarded apps, PCB viewer, localhost site |
| Full-screen surface | Dense inspection or mobile use | PCB layout, 3D board, notebook, mobile |

The split surface is the normal mode for forwarded HTTP apps. The floating
overlay should become a legacy/transitional behavior or a compact fallback, not
the main interaction.

## Layout Contract

Desktop:

- The session remains a column with header, transcript, and prompt.
- Opening a surface changes the session body into a two-pane layout.
- Left pane: chat transcript and prompt.
- Right pane: active surface.
- Default split: 50% surface width for visual tools, 40-45% for simple web apps
  if the viewport is narrow.
- User can resize between 30% and 70%.
- Width is remembered per session.
- Closing the surface restores full-width chat.
- Collapsing hides the surface while keeping the iframe mounted, preserving app
  state.

Tablet / narrow desktop:

- Use a vertical split when horizontal width is too small.
- Default surface height should be 45-55% of the session view.
- The prompt remains reachable without scrolling through the forwarded app.

Mobile:

- Split view becomes full-screen surface mode when a surface opens on a
  phone-sized viewport.
- The surface toolbar returns to split/chat, and Escape exits fullscreen before
  closing the surface.
- Inline cards remain tappable entry points.

Multi-session dashboard:

- The focused session may open a split surface.
- Hidden/unfocused session panes should keep their state under the same rules
  currently used for session panes and permission dialogs.
- Keyboard navigation must treat the surface as part of the focused session, not
  as a global modal overlay.

## Surface Model

Introduce a frontend concept named `SessionSurface`.

Implemented shape:

```text
SessionSurface
  id: stable frontend id
  session_id
  kind: Forward
  title
  subtitle
  url
  port
  process
  open_mode: Split | Fullscreen
  collapsed
```

Future electronics shape:

```text
SessionSurface
  kind: BoardViewer | SchematicViewer | GerberViewer | BomTable | StepViewer
  artifact_id
  source_path
  generated_at
  tool_status
```

The important design choice is that the session view should not care whether
the right pane is a forwarded app, a PCB artifact, or a future browser surface.
It should care only that a surface exists and that it can render the surface
body.

## Forwarder Changes

`ForwardChips` is a small control strip:

- fetch forwards;
- render chips and owner revoke button;
- emit `OpenForwardSurface(ForwardInfo)`;
- no overlay geometry;
- no iframe chrome.

Component responsibilities:

- `SessionView`: owns active surface selection, mode, resize, and persistence.
- `ForwardSurface`: renders the forward iframe, title, visit link, close,
  collapse, fullscreen, and status.
- `SessionSurface`: carries the generic frontend state future board/BOM/3D
  surfaces will reuse.

The iframe should keep the existing handoff URL behavior. Auth, cookies, reverse
proxy rewriting, and tunnel transport should not change in the split-view PR.

## Electronics Synthesis

The KiCad PCB plugin's useful pattern is not merely a PCB viewer. It is an
environment runtime: the machine that owns the checkout owns tools, terminals,
Git, provider credentials, and local files. Remote clients view and control that
environment.

Agent Portal already has the right architecture for this through launchers,
sessions, forwards, file download, media display, and per-session agent context.
The electronics work should plug into that architecture.

PasteBOM contributes a concrete artifact pipeline:

1. read a board or manufacturing package;
2. parse it into normalized JSON;
3. store original input plus parsed output;
4. render a browser viewer with BOM, layers, nets, search, pan/zoom, and
   thumbnails.

The KiCad PCB plugin contributes workflow coverage:

- `.kicad-pcb.json` project assignment;
- KiCad/schematic/Gerber/3D/BOM/footprint/symbol/panelization views;
- KiCad CLI and Python tool probing;
- ERC/DRC/export loops;
- mobile/remote viewing of the server environment;
- source-control and PR flow after inspection.

Agent Portal should synthesize these into an `Electronics Workspace` capability
rather than making the core product electronics-specific.

## Electronics Session Flow

1. Session starts in a repo.
2. Launcher probes the checkout for hardware signals:
   - `.kicad-pcb.json`;
   - `.kicad_pro`, `.kicad_pcb`, `.kicad_sch`;
   - Gerber ZIPs;
   - ODB++ archives;
   - Altium, Eagle, EasyEDA files;
   - PasteBOM-compatible uploads.
3. If hardware is detected, frontend exposes an `Electronics` surface affordance.
4. The agent gets electronics-specific instructions in the session reminder.
5. User or agent opens the board surface.
6. A launcher-side service parses/renders artifacts and serves a viewer over a
   local HTTP port.
7. The session opens that viewer in the same split surface machinery as ordinary
   forwards.
8. Agent changes are verified with ERC/DRC/BOM/Gerber artifacts before commit.
9. Optional publish/share to PasteBOM requires explicit user approval and a
   privacy warning.

## Proposed PR DAG

```text
P0 planning doc
 |
 +-- P1 surface state extraction
 |    |
 |    +-- P2 split layout shell
 |    |    |
 |    |    +-- P3 forward iframe surface
 |    |    |    |
 |    |    |    +-- P4 responsive/fullscreen/mobile behavior
 |    |    |    |
 |    |    |    +-- P5 persistence and keyboard polish
 |    |    |
 |    |    +-- P6 artifact surface API sketch
 |    |         |
 |    |         +-- P7 PasteBOM local parser/viewer spike
 |    |              |
 |    |              +-- P8 electronics manifest + detection
 |    |                   |
 |    |                   +-- P9 KiCad checks/export operations
 |    |                        |
 |    |                        +-- P10 electronics session cards
 |    |                             |
 |    |                             +-- P11 publish/share workflow
 |    |
 |    +-- P12 visual regression/demo capture
 |
 +-- P13 docs and agent reminder updates
```

`P12` and `P13` can proceed after the split surface exists, but they should not
block the parser and electronics design work unless CI/demo stability requires
it.

## PR Plan

### P0: Planning Document

Scope:

- Add this document.
- No behavior changes.

Exit criteria:

- The team agrees on the PR boundaries and the split-surface direction.

### P1: Surface State Extraction (shipped)

Scope:

- Add frontend-only `SessionSurface` state.
- Move forward-preview open/close state out of `ForwardChips`.
- Make `ForwardChips` emit callbacks instead of rendering preview chrome.
- Preserve current overlay behavior through a compatibility host if necessary.

Non-goals:

- No layout redesign.
- No protocol or backend changes.

Exit criteria:

- Clicking a forward still opens the existing preview.
- The preview is no longer owned by `ForwardChips`.
- Existing forward chip health/revoke behavior is unchanged.

Risks:

- Session switch races from stale forward fetches.
- Hidden session panes retaining open surface state incorrectly.

### P2: Split Layout Shell (shipped)

Scope:

- Add `SessionSplitLayout` around transcript, prompt, and surface host.
- Support horizontal desktop split.
- Add resize handle and min/max constraints.
- Keep the prompt in the chat pane.
- Keep the surface mounted while resizing.

Non-goals:

- No mobile/fullscreen work yet.
- No electronics artifacts.

Exit criteria:

- A placeholder surface can occupy the right half of the focused session.
- Chat scrolling, prompt focus, and jump-to-live still work.
- Permission dialogs and task panels remain usable.

### P3: Forward Iframe Surface (shipped)

Scope:

- Move iframe preview into `ForwardSurface`.
- Use the existing `/api/sessions/{id}/forwards/open` handoff URL.
- Add title, process name, port, visit link, close, collapse.
- Preserve iframe state while collapsed when possible.
- Keep public/private forward behavior unchanged.

Exit criteria:

- `agent-portal forward 8080` opens as a split surface.
- Visit-site opens the full forward origin.
- Revoking the forward closes or invalidates the surface gracefully.
- WebSocket/SSE behavior remains unchanged.

### P4: Responsive and Fullscreen Behavior (shipped)

Scope:

- Vertical split for narrow desktop/tablet.
- Full-screen surface mode for mobile.
- Toolbar affordance to switch split/fullscreen on desktop.
- Back/return affordance on mobile.

Exit criteria:

- Forwarded app is usable on phone.
- Prompt remains reachable after returning from full-screen surface.
- No fixed-width controls overflow mobile.

### P5: Persistence and Keyboard Polish (shipped)

Scope:

- Remember split width per session in local storage or frontend session state.
- Define keyboard behavior:
  - Escape exits fullscreen before closing surface.
  - Existing dashboard navigation still focuses session panes.
  - Surface iframe does not steal global shortcuts when not focused.
- Add help-overlay entries only if shortcuts are added.

Exit criteria:

- Refreshing the dashboard restores a reasonable split size.
- Keyboard navigation does not treat the surface as a global modal.
- Reduced-motion behavior remains clean.

### P6: Artifact Surface API Sketch

Scope:

- Define shared/frontend types for non-forward surfaces.
- Decide persistence boundary:
  - transcript message;
  - backend DB artifact row;
  - media archive;
  - session-local ephemeral state.
- Document how an artifact opens a surface.

Non-goals:

- No parser implementation.
- No migrations unless the artifact persistence decision requires them.

Exit criteria:

- The split surface can host a fake board/BOM artifact from test data.
- The API shape can represent a forward surface and an artifact surface without
  special-casing layout.

### P7: PasteBOM Local Parser/Viewer Spike

Scope:

- Evaluate embedding or depending on PasteBOM parser/viewer code.
- Identify whether the first implementation should:
  - import `pcb-extract` as a crate;
  - shell out to a CLI;
  - host the PasteBOM viewer as an external sidecar;
  - or copy only the normalized JSON schema.
- Prototype outside the main product path or behind a dev-only feature.

Exit criteria:

- One KiCad board can render as a private session surface.
- One Gerber ZIP can render or produce a clear unsupported-path report.
- The output never publishes publicly by default.

### P8: Electronics Manifest and Detection

Scope:

- Support `.kicad-pcb.json` as a read-only project manifest.
- Detect common board/schematic/manufacturing files when no manifest exists.
- Expose detected electronics state in the UI.
- Add agent-facing instructions for detected electronics repos.

Exit criteria:

- A `.kicad-pcb.json` configured repo opens the expected board file.
- A plain KiCad repo is discovered without manual configuration.
- The user can override the selected board/schematic for the session.

### P9: KiCad Checks and Export Operations

Scope:

- Add launcher-owned typed operations for:
  - tool probe;
  - ERC;
  - DRC;
  - BOM export;
  - Gerber/drill export;
  - optional KiKit panelization preview.
- Keep all generated outputs in temp/session artifact storage unless the user
  asks to write release files into the repo.

Exit criteria:

- Agent can run checks and attach reports without ad hoc shell scraping.
- Failure modes say which tool is missing and how to install/configure it.
- Generated artifacts are reproducible enough for PR review.

### P10: Electronics Session Cards

Scope:

- Add durable transcript cards for:
  - board preview;
  - BOM;
  - ERC/DRC report;
  - Gerber package;
  - panel preview.
- Cards open the relevant split surface.
- Cards degrade gracefully in archived history.

Exit criteria:

- A manufacturing-review session is understandable from transcript history.
- Cards link back to source/generated files.
- Mobile can open the artifacts.

### P11: Publish and Share Workflow

Scope:

- Add explicit “publish to PasteBOM” or “create public board link” action.
- Show privacy copy: secret links are not private, only hidden from public feed.
- Record the external link as a transcript artifact.
- Keep private Portal artifacts as the default.

Exit criteria:

- User consent is required before public upload.
- Published link survives in transcript/history.
- Revoking/deleting private session artifacts does not pretend to revoke the
  external PasteBOM link.

### P12: Visual Regression and Demo Capture

Scope:

- Add demo/capture coverage for:
  - no surface;
  - split forward surface;
  - collapsed surface;
  - fullscreen/mobile surface;
  - future fake board artifact.
- Update docs/media captures if they are used by the project homepage/docs.

Exit criteria:

- We can visually compare the new split behavior before shipping.
- Layout regressions around prompt, transcript, and surface are easy to catch.

### P13: Docs and Agent Reminder Updates

Scope:

- Update `docs/PORT_FORWARDING.md` frontend section from overlay to split
  surface.
- Update portal reminder text so agents know forwards open in the session UI.
- Add electronics workflow docs once P8-P10 exist.

Exit criteria:

- Docs describe the current behavior, not the historical overlay.
- Agent reminders guide agents toward showing useful surfaces, not just pasting
  raw URLs.

## Requirement DAG

| Requirement | Depends on | Needed by |
| --- | --- | --- |
| Surface state extracted from chips | P0 | P2, P3 |
| Split layout shell | P1 | P3, P4, P6 |
| Forward iframe surface | P1, P2 | P4, P5, P13 |
| Responsive/fullscreen behavior | P2, P3 | Electronics mobile use |
| Surface persistence/keyboard polish | P2, P3 | Daily usability |
| Artifact surface model | P2 | P7-P11 |
| PasteBOM parser/viewer decision | P6 | P8-P10 |
| Electronics detection/manifest | P6, P7 | P9-P10 |
| KiCad operations | P8 | P10 |
| Electronics cards | P6, P8, P9 | P11 |
| Public publish/share | P10 | External review workflows |
| Visual regression coverage | P2, P3 | Confident UI rollout |
| Docs/reminders | shipped behavior | User and agent adoption |

## Testing Strategy

Frontend unit tests:

- `ForwardChips` emits open/revoke events without owning iframe state.
- Surface reducer opens, closes, collapses, switches mode, and ignores stale
  session updates.
- Split constraints clamp to valid widths/heights.

Browser/manual tests:

- `python3 -m http.server` via `agent-portal forward`.
- Vite dev server with HMR.
- Jupyter or another WebSocket-heavy app.
- Private forward iframe auth in production-like same-site environment.
- Dev-domain behavior where private iframe auth may require full-page visit.

Responsive tests:

- desktop wide;
- laptop width;
- tablet/narrow split;
- phone fullscreen.

Electronics tests, once implemented:

- KiCad board parse and preview.
- Gerber ZIP parse and preview.
- Missing KiCad CLI produces actionable setup state.
- ERC/DRC report card opens from transcript.
- Public PasteBOM publish requires confirmation.

## Rollout Strategy

1. Land extraction with no visible behavior change.
2. Land split surface behind a frontend feature flag or local setting if the UI
   risk feels high.
3. Switch forward chip default from overlay to split.
4. Keep a direct “Visit site” path at all times.
5. Remove overlay-only code after the split surface has been used in production.
6. Build electronics artifacts on the same surface host.

## Open Decisions

- Should split width be stored in local storage, backend user preferences, or
  session-local frontend state?
- Should a collapsed split keep the iframe mounted on mobile, or should mobile
  always unmount when returning to chat to save memory?
- Should electronics artifacts use the existing media archive path or a distinct
  artifact table with metadata and lifecycle rules?
- Should `.kicad-pcb.json` be supported as the canonical electronics manifest,
  or should Portal create its own manifest while importing compatible fields?
- Should the first PCB viewer be a forwarded sidecar app, a native Portal WASM
  component, or both?
- What is the retention policy for generated board JSON, thumbnails, and
  manufacturing packages?

## Non-Goals

- Replacing the forward transport.
- Supporting multiple simultaneous forwarded ports in one session.
- Publishing private board files to PasteBOM automatically.
- Building a full EDA editor inside Portal.
- Letting the browser run local electronics tools directly.

## Guiding Principle

The session remains the source of collaboration. The split surface is a durable
workbench attached to that conversation, not a separate app and not a modal
detour. If an agent can point at a live artifact while explaining the work, the
workflow belongs in the split surface.
