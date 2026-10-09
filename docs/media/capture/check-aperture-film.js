// Browser checks for the HTML orientation film, including static-host seeking.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const root = process.env.DEMO_ROOT || "/tmp/readme-demo";
const base = process.env.DEMO_URL || "http://localhost:8792";
const shots = path.join(root, "aperture-qa");
fs.mkdirSync(shots, { recursive: true });
const p = require(path.join(root, "node_modules/puppeteer-core"));
(async () => {
  const b = await p.launch({
    executablePath: "/usr/bin/google-chrome",
    headless: true,
    args: ["--no-sandbox"],
  });
  try {
    const page = await b.newPage();
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("response", (r) => {
      if (r.status() >= 400) errors.push(`${r.status()} ${r.url()}`);
    });
    await page.setViewport({ width: 1440, height: 900 });
    await page.goto(base + "/aperture-assets/trailer/index.html", {
      waitUntil: "domcontentloaded",
    });
    await page.waitForFunction(() => window.ready);
    assert.equal(await page.$eval("#film-start", (e) => e.hidden), false);
    await page.screenshot({ path: path.join(shots, "film-start.png") });
    await page.click("#start-screening");
    await page.waitForFunction(
      () => document.querySelector("audio").currentTime > 0.2,
    );
    assert.equal(await page.$eval("#film-start", (e) => e.hidden), true);
    const sharingTime = await page.evaluate(
      () => when(CUES.find(([id]) => id === "sharing-complete")[1]) + 1,
    );
    await page.evaluate((time) => {
      const a = document.querySelector("audio");
      a.pause();
      a.currentTime = time;
    }, sharingTime);
    await page.waitForFunction(() =>
      [...document.querySelectorAll(".sub")].some(
        (e) =>
          getComputedStyle(e).opacity > 0.9 &&
          e.textContent.includes("Your friend"),
      ),
    );
    await page.screenshot({ path: path.join(shots, "film-share.png") });
    await page.evaluate(
      () =>
        (document.querySelector("audio").currentTime =
          when(CUES.find(([id]) => id === "welcome")[1]) + 1),
    );
    await page.waitForFunction(() =>
      [...document.querySelectorAll(".sub")].some(
        (e) =>
          getComputedStyle(e).opacity > 0.9 && e.textContent.includes("Hello"),
      ),
    );
    await page.click("#restart-film");
    await page.waitForFunction(() => {
      const a = document.querySelector("audio");
      return !a.paused && a.currentTime < 2;
    });
    await page.setViewport({ width: 390, height: 844 });
    await page.evaluate((time) => {
      const a = document.querySelector("audio");
      a.pause();
      a.currentTime = time;
    }, sharingTime);
    await page.waitForFunction(() =>
      [...document.querySelectorAll(".sub")].some(
        (e) =>
          getComputedStyle(e).opacity > 0.9 &&
          e.textContent.includes("Your friend"),
      ),
    );
    assert.equal(
      await page.$eval("#stage", (e) => {
        const r = e.getBoundingClientRect();
        return (
          r.left >= -0.5 &&
          r.right <= innerWidth + 0.5 &&
          r.top >= 0 &&
          r.bottom <= innerHeight - 75
        );
      }),
      true,
    );
    await page.screenshot({ path: path.join(shots, "film-mobile.png") });
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth > innerWidth,
      ),
      false,
    );
    // Check the new engineering sequence at its authored cue, not a fixed
    // second that silently points to a different scene after narration edits.
    const pluginsTime = await page.evaluate(
      () => when(CUES.find(([id]) => id === "plugins-complete")[1]) + 1,
    );
    await page.evaluate(
      (time) => (document.querySelector("audio").currentTime = time),
      pluginsTime,
    );
    await page.waitForFunction(() =>
      [...document.querySelectorAll(".sub")].some(
        (e) =>
          getComputedStyle(e).opacity > 0.9 &&
          /Every design/.test(e.textContent),
      ),
    );
    await page.screenshot({ path: path.join(shots, "film-plugins.png") });
    for (const [id, phrase, clip] of [
      ["electronics-intro", "Chamber six", "clip-pcb"],
      ["refinement", "None of these tools", "refine-yapcad"],
    ]) {
      const t = await page.evaluate(
        (id) => when(CUES.find(([key]) => key === id)[1]),
        id,
      );
      await page.evaluate((t) => {
        const a = document.querySelector("audio");
        a.pause();
        a.currentTime = t + 1;
      }, t);
      await page.waitForFunction(
        (phrase) =>
          [...document.querySelectorAll(".sub")].some(
            (e) =>
              getComputedStyle(e).opacity > 0.9 &&
              e.textContent.includes(phrase),
          ),
        {},
        phrase,
      );
      await page.evaluate((t) => {
        document.querySelector("audio").currentTime = t + 8;
      }, t);
      await page.waitForFunction(
        (id) => document.getElementById(id).currentTime > 2,
        {},
        clip,
      );
      await page.screenshot({ path: path.join(shots, `film-${id}.png`) });
    }
    await page.waitForFunction(() =>
      [...document.querySelectorAll("video")].every(
        (v) => !v.seeking && v.readyState >= 2 && !v.error,
      ),
    );
    assert.deepEqual(errors, []);
    console.log(
      "PASS: film start, actual audio, pause/seek, backwards captions, restart, mobile fit, mechanism/electronics/refinement sequences, real video seeking, all assets load",
    );
  } finally {
    await b.close();
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
