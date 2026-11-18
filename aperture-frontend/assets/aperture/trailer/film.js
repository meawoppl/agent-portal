/* One full film. The soundtrack owns the clock; export uses the same deterministic seek. */
"use strict";
const $ = (id) => document.getElementById(id);
const query = new URLSearchParams(location.search);
const capture = query.has("capture"),
  external = capture && query.has("external-clips");
const sectors = [
  {
    id: "s1",
    name: "CODE GENERATION",
    title: "FIRST, WE TOOK YOUR CODE.",
    quip: "HUMAN MEAT PROXY / MESSAGE TRANSPORT POSITION ELIMINATED",
    bar: "AGENT-AUTHORED CODE · PEER REVIEW · ILLUSTRATED WORKFLOW",
    note: "THE EXHIBIT IS REAL SOFTWARE · THE REVIEW DIAGRAM IS AN ILLUSTRATION",
    clip: null,
  },
  {
    id: "s2",
    name: "REMOTE TEST BENCH",
    title: "THE BENCH IS IN ANOTHER BUILDING.",
    quip: "SO IS THE PERSON WHO USED TO BABYSIT IT.",
    bar: "CF-00020-01 · NUCLEO FIXTURE · v4.1.0 · 2 kHz TELEMETRY",
    note: "RECORDED HARDWARE DATA · NOT LIVE · SINGLE BENCH BOARD",
    clip: "bench",
  },
  {
    id: "s3",
    name: "WEB PROXY",
    title: "YOUR BROWSER. OUR LABORATORY.",
    quip: "HTTP 200 OK / CALORIES 404",
    bar: "AGENT-HOSTED WEBSITE · FORWARDED PREVIEW · VIEWER ACCESS",
    note: "ACTUAL EXHIBIT RECORDING · SESSION AND SHARING ACTIONS ARE ISOLATED DEMO STATE",
    clip: "web",
  },
  {
    id: "s4",
    name: "MECHANICAL",
    title: "THE CAKE HAS AN ISSUE TRACKER.",
    quip: "FILLING: 0.8 → 6 mm / FROSTING: 3 → 7 mm",
    bar: "yapCAD · ACTUAL PARAMETRIC REBUILD · RETAINED RUNS",
    note: "PRINT VARIANT · 108 × 108 × 60 mm · ONE CLOSED BODY · NO PRINT TRIAL",
    clip: "cake",
  },
  {
    id: "s5",
    name: "ELECTRICAL",
    title: "YOUR CIRCUIT BOARDS. OUR TURN.",
    quip: "SCHEMATIC → COPPER → THREE DIMENSIONS → REVISE",
    bar: "KICADMIUM · REAL M.2 MODULE AND TESLA CONTROLLER · KiCad 10",
    note: "ACTUAL DESIGNS AND CHECKS · QUALITY FINDINGS RETAINED · NOT FLIGHT QUALIFICATION",
    clip: "boards",
  },
  {
    id: "s6",
    name: "LOGIC",
    title: "WE DO NOT BLINK.",
    quip: "WE DO NOT HAVE EYES. THIS HELPS.",
    bar: "VISILOG · COUNTER TESTBENCH · LIVE VALUES AND WAVEFORMS",
    note: "ACTUAL RTL SIMULATION · BREAKPOINT AND $finish · NOT TIMING SIGN-OFF",
    clip: "visilog",
  },
  {
    id: "s7",
    name: "CONTROL",
    title: "CLOSE THE LOOP. RETIRE THE PROXY.",
    quip: "BUILD A MODEL / INSPECT THE RESPONSE / KEEP THE FAILURES",
    bar: "UNLINKED · JLG SINGLE-AXIS RECONSTRUCTION · MODEL, TRACE AND JLG REVIEW",
    note: "SIMULATION, NOT MEASURED HARDWARE · SUPPORTED BLOCKS · NO MATHWORKS EQUIVALENCE CLAIM",
    clip: "controls",
  },
  {
    id: "s8",
    name: "BRIEFING",
    title: "EVEN THE PRESENTER IS OPTIONAL.",
    quip: "HUMAN MEAT PROXY / REASSIGNED TO AUDIENCE",
    bar: "ENGINEERING-PRESENTATIONS · ACTUAL JUSTLETGO REVIEW DECK",
    note: "AGENT-AUTHORED ENGINEERING BRIEFING · SIMULATED AND ANALYTIC RESULTS KEEP THEIR LIMITS",
    clip: "briefing",
  },
];
const videos = new Map(),
  urls = [],
  scenes = [],
  cues = [],
  clips = [];
let total = 0,
  active = null,
  captions = [],
  soundtrackReady = false;
const audio = $("audio");
const codeMarkup = `<div class="code-evidence"><h2>THE AGENT BUILDS THE THING YOU ARE LOOKING AT.</h2><pre><span class="added">+ const cube = document.querySelector("#cube");
+ let rotation = 0;
+
+ document.querySelector("#rotate")
+   .addEventListener("click", () =&gt; {
+     rotation += 90;
+     cube.style.setProperty("--rotation", rotation + "deg");
+   });</span></pre><div class="agent-relay"><div><strong>BUILDER → REVIEWER</strong>Change ready. Check the interaction and the rendered result.</div><div><strong>REVIEWER → BUILDER</strong>Inspect. Question. Revise. Run the checks again.</div></div><div class="code-verdict">NO HUMAN MESSAGE COURIER REQUIRED.</div></div>`;
function timeLabel(t) {
  return `${Math.floor(t / 60)}:${String(Math.floor(t % 60)).padStart(2, "0")}`;
}
function fit() {
  const r = $("viewport").getBoundingClientRect();
  $("stage").style.transform =
    `scale(${Math.min(r.width / 1920, r.height / 1080)})`;
}
function schedule(lines, measuredCaptions) {
  const byId = Object.fromEntries(lines.map((l) => [l.id, l]));
  const add = (id, lineIds, sector = null) => {
    const start = total;
    let cursor = start + (id === "welcome" ? 5 : 2);
    for (const key of lineIds) {
      const line = byId[key];
      if (!line) throw new Error(`Missing narration: ${key}`);
      const duration = line.duration_ms / 1000;
      cues.push([key, cursor]);
      captionLine(line, cursor, measuredCaptions);
      cursor += duration + 2.5;
    }
    total = cursor + (id === "finale" ? 3 : 0.5);
    const scene = { id, start, end: total, sector };
    scenes.push(scene);
    if (sector?.clip)
      clips.push({
        id: sector.clip,
        scene: id,
        start: start + 1.2,
        end: total - 0.6,
        x: 632,
        y: 282,
        w: 1188,
        h: 668,
        cropBottom: 36,
      });
  };
  add("welcome", ["welcome"]);
  for (const s of sectors) add(s.id, [`${s.id}-intro`, `${s.id}-complete`], s);
  add("making-of", ["making-of"]);
  add("mission", ["mission"]);
  add("finale", ["finale"]);
  $("seek").max = total;
  window.TOTAL = total;
  window.CUES = cues;
  window.CLIPS = clips;
  window.FILM = { version: 5, total, scenes, cues, clips };
}
function captionLine(line, start, measuredCaptions) {
  const entry = measuredCaptions.lines.find((c) => c.id === line.id);
  if (!entry || !entry.segments.length)
    throw new Error(`Missing measured captions: ${line.id}`);
  if (entry.segments.map((s) => s.text).join(" ") !== line.text)
    throw new Error(`Caption text does not match narration: ${line.id}`);
  for (const segment of entry.segments) {
    captions.push({
      start: start + segment.start_ms / 1000,
      end: start + segment.end_ms / 1000,
      text: segment.text,
    });
  }
}
function show(scene) {
  if (active?.id === scene.id) return;
  active = scene;
  for (const id of [
    "sector-scene",
    "welcome-scene",
    "making-scene",
    "mission-scene",
    "finale-scene",
  ])
    $(id).hidden = true;
  $("stage").classList.toggle("dark", !scene.sector);
  $("sector-count").textContent = scene.sector
    ? `SECTOR ${scene.id.slice(1).padStart(2, "0")} / 08`
    : "HUMAN MEAT PROXY RETIREMENT PROGRAM";
  if (scene.sector) {
    const s = scene.sector;
    $("sector-scene").hidden = false;
    $("hud-title").textContent = s.name;
    $("hud-sub").textContent = "APERTURE SCIENCE · AGENT PORTAL";
    $("sign").src = `../art/sign-${s.id.slice(1).padStart(2, "0")}.svg`;
    $("sign").alt = `Sector ${s.id.slice(1)}: ${s.name}`;
    $("sector-title").textContent = s.title;
    $("sector-quip").textContent = s.quip;
    $("evidence-bar").textContent = s.bar;
    $("evidence-note").textContent = s.note;
    $("hero").replaceChildren();
    if (s.clip) {
      const v = videos.get(s.clip);
      if (v) $("hero").append(v);
      else {
        const img = document.createElement("img");
        img.src = `../evidence/${s.clip}.jpg`;
        img.alt = s.name;
        $("hero").append(img);
      }
    } else $("hero").innerHTML = codeMarkup;
  } else {
    const id = {
      welcome: "welcome-scene",
      "making-of": "making-scene",
      mission: "mission-scene",
      finale: "finale-scene",
    }[scene.id];
    $(id).hidden = false;
    $("hud-title").textContent = {
      welcome: "ORIENTATION / COMPOSER ONLINE",
      "making-of": "PRODUCTION RECORD",
      mission: "EXPLORATION TOOLING",
      finale: "ENRICHMENT COMPLETE",
    }[scene.id];
    $("hud-sub").textContent = "APERTURE SCIENCE · COSMIC FRONTIER LABS";
  }
}
async function render(t) {
  t = Math.max(0, Math.min(total, t));
  const scene = scenes.find((s) => t >= s.start && t < s.end) || scenes.at(-1);
  if (!scene) return;
  show(scene);
  const age = t - scene.start,
    remain = scene.end - t;
  const fade = Math.min(1, age / 0.6, remain / 0.5);
  const panel = scene.sector
    ? $("sector-scene")
    : $(
        {
          welcome: "welcome-scene",
          "making-of": "making-scene",
          mission: "mission-scene",
          finale: "finale-scene",
        }[scene.id],
      );
  panel.style.opacity = fade;
  if (scene.id === "welcome") {
    $("composer-code").textContent = [
      '$ agent-portal message send colleague "Build the exhibit."',
      "> script: agent-authored",
      "> footage: real tools, real evidence",
      "> human meat proxy: under review",
    ]
      .join("\n")
      .slice(0, Math.floor(age * 31));
    document
      .querySelectorAll(".portal-ring")
      .forEach(
        (el, i) =>
          (el.style.transform = `rotate(${Math.sin(age * 1.2 + i) * 3}deg) scale(${1 + Math.sin(age * 2) * 0.015})`),
      );
  }
  if (scene.id === "s1") {
    document
      .querySelectorAll(".agent-relay>div")
      .forEach(
        (el, i) =>
          (el.style.opacity = Math.max(
            0,
            Math.min(1, (age - 4 - i * 6) / 0.6),
          )),
      );
    document.querySelector(".code-verdict").style.opacity = Math.max(
      0,
      Math.min(1, (age - 18) / 0.6),
    );
  }
  // Closing cards are deliberately stationary. Only the whole scene fades;
  // staggered credits and a rotating mission diagram distracted from the voice.
  $("subtitle").textContent =
    captions.find((c) => t >= c.start && t < c.end)?.text || "";
  $("mobile-title").textContent =
    scene.sector?.name || $("hud-title").textContent;
  $("mobile-caption").textContent = $("subtitle").textContent;
  $("mobile-provenance").textContent =
    scene.sector?.note || "APERTURE SCIENCE · AGENT PORTAL";
  $("film-progress").style.width = `${(100 * t) / total}%`;
  $("clock").textContent = `${timeLabel(t)} / ${timeLabel(total)}`;
  $("seek").value = t;
  $("play").textContent = audio.paused ? "▶" : "Ⅱ";
  const waiting = [];
  for (const [id, v] of videos) {
    const clip = clips.find((c) => c.id === id);
    const live = clip && t >= clip.start && t < clip.end;
    const duration = Number.isFinite(v.duration) ? v.duration : 0;
    const wanted = live
      ? Math.min(
          Math.max(0, duration - 0.06),
          (t - clip.start) * (clip.rate || 1),
        )
      : 0;
    if (!live) {
      v.pause();
      continue;
    }
    v.playbackRate = clip.rate || 1;
    if (
      Math.abs(v.currentTime - wanted) > (capture ? 0.01 : 0.35) &&
      !v.seeking
    ) {
      if (capture) {
        waiting.push(
          new Promise((resolve, reject) => {
            const timer = setTimeout(
              () => reject(new Error(`Video seek timed out: ${id}`)),
              6000,
            );
            v.addEventListener(
              "seeked",
              () => {
                clearTimeout(timer);
                resolve();
              },
              { once: true },
            );
          }),
        );
      }
      v.currentTime = wanted;
    }
    if (!capture && !audio.paused && wanted < duration - 0.1) {
      if (v.paused) v.play().catch(() => {});
    } else v.pause();
  }
  await Promise.all(waiting);
}
async function loadVideo(id) {
  const v = document.createElement("video");
  v.muted = true;
  v.playsInline = true;
  v.preload = "auto";
  v.poster = `../evidence/${id}.jpg`;
  v.dataset.clip = id;
  v.setAttribute("aria-label", `Recorded ${id} evidence`);
  videos.set(id, v);
  if (external) return;
  const r = await fetch(`../evidence/${id}.mp4`);
  if (!r.ok) throw new Error(`${id} footage: HTTP ${r.status}`);
  const u = URL.createObjectURL(await r.blob());
  urls.push(u);
  v.src = u;
  await new Promise((resolve, reject) => {
    v.addEventListener("loadeddata", resolve, { once: true });
    v.addEventListener(
      "error",
      () => reject(new Error(`Cannot decode ${id} footage`)),
      { once: true },
    );
    v.load();
  });
}
async function start() {
  if (!soundtrackReady) return;
  $("gate").hidden = true;
  await audio.play();
}
async function init() {
  if (capture) document.body.classList.add("capture");
  if (external) document.body.classList.add("external-clips");
  for (let i = 0; i < 100; i++) {
    const star = document.createElement("i");
    const n = (i * 7919) % 10000;
    star.style.cssText = `left:${n % 1920}px;top:${(n * 17) % 1080}px;width:${(i % 3) + 1}px;height:${(i % 3) + 1}px;opacity:${0.2 + (i % 7) / 10}`;
    $("stars").append(star);
  }
  fit();
  const r = await fetch("../lines.json");
  if (!r.ok) throw new Error("Narration manifest unavailable");
  const captionResponse = await fetch("../captions.json");
  if (!captionResponse.ok) throw new Error("Measured captions unavailable");
  schedule((await r.json()).lines, await captionResponse.json());
  const mediaResponse = await fetch("../evidence/media.json");
  if (!mediaResponse.ok) throw new Error("Evidence metadata unavailable");
  const media = await mediaResponse.json();
  for (const clip of clips) {
    const duration = media[clip.id]?.duration || 0;
    clip.rate = Math.max(1, duration / (clip.end - clip.start));
  }
  window.seek = render;
  window.ready = false;
  $("load-status").textContent = "Loading real tool recordings…";
  await Promise.all(
    sectors.filter((s) => s.clip).map((s) => loadVideo(s.clip)),
  );
  if (!capture) {
    $("load-status").textContent = "Loading the full soundtrack…";
    const a = await fetch("trailer.mp3");
    if (!a.ok) throw new Error("Soundtrack unavailable");
    const u = URL.createObjectURL(await a.blob());
    urls.push(u);
    audio.src = u;
    await new Promise((resolve, reject) => {
      audio.addEventListener("loadedmetadata", resolve, { once: true });
      audio.addEventListener(
        "error",
        () => reject(new Error("Soundtrack cannot play")),
        { once: true },
      );
      audio.load();
    });
    soundtrackReady = true;
  }
  await render(0);
  window.ready = true;
  $("begin").disabled = false;
  $("begin").textContent = "BEGIN YOUR REASSIGNMENT";
  $("load-status").textContent =
    `${timeLabel(total)} · Narrated orientation · Captions included`;
  if (!capture) {
    const tick = () => {
      render(Math.min(total, audio.currentTime)).catch(console.error);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  }
}
$("begin").addEventListener("click", () =>
  start().catch((e) => {
    $("load-status").textContent = e.message;
  }),
);
$("play").addEventListener("click", () => {
  if (!audio.paused) audio.pause();
  else start().catch(console.error);
});
$("restart").addEventListener("click", () => {
  audio.currentTime = 0;
  render(0);
  start().catch(console.error);
});
$("seek").addEventListener("input", () => {
  audio.currentTime = Number($("seek").value);
  render(audio.currentTime);
});
$("mute").addEventListener("click", () => {
  audio.muted = !audio.muted;
  $("mute").textContent = audio.muted ? "♪̸" : "♫";
  $("mute").setAttribute(
    "aria-label",
    audio.muted ? "Unmute narration" : "Mute narration",
  );
});
$("fullscreen").addEventListener("click", () => {
  if (document.fullscreenElement) document.exitFullscreen();
  else document.documentElement.requestFullscreen().catch(console.error);
});
window.addEventListener("resize", fit);
audio.addEventListener("ended", () => render(total));
window.addEventListener("pagehide", () =>
  urls.forEach((u) => URL.revokeObjectURL(u)),
);
init().catch((e) => {
  console.error(e);
  $("load-status").textContent = e.message;
  $("begin").textContent = "Unable to prepare film";
  window.filmError = e.message;
});
