// Exercise the actual audio clock, bidirectional seeking and every evidence scene.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const root = process.env.DEMO_ROOT || "/tmp/pp";
const base = process.env.DEMO_URL || "http://localhost:8792";
const shots = process.env.DEMO_SHOTS || path.join(root, "aperture-qa");
fs.mkdirSync(shots, { recursive: true });
const puppeteer = require(path.join(root, "node_modules/puppeteer-core"));
(async () => {
  const browser = await puppeteer.launch({
    executablePath: "/usr/bin/google-chrome",
    headless: true,
    args: ["--no-sandbox"],
  });
  try {
    const page = await browser.newPage(),
      errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("response", (r) => {
      if (r.status() >= 400) errors.push(`${r.status()} ${r.url()}`);
    });
    await page.setViewport({ width: 1440, height: 900 });
    await page.goto(base + "/aperture-assets/trailer/index.html", {
      waitUntil: "domcontentloaded",
    });
    await page.waitForFunction(() => window.ready, { timeout: 60000 });
    const film = await page.evaluate(() => window.FILM);
    const measured = await page.evaluate(async () =>
      (await fetch("../captions.json")).json(),
    );
    assert.equal(film.scenes.length, 12);
    assert.equal(film.cues.length, 20);
    assert.equal(film.clips.length, 7);
    assert.equal(await page.$eval("#gate", (e) => e.hidden), false);
    const length = await page.$eval("audio", (a) => a.duration);
    assert.ok(
      Math.abs(length - film.total) < 0.3,
      "Soundtrack and timeline must agree",
    );
    await page.click("#begin");
    await page.waitForFunction(
      () => document.querySelector("audio").currentTime > 0.2,
    );
    assert.equal(await page.$eval("#gate", (e) => e.hidden), true);
    await page.click("#play");
    assert.equal(await page.$eval("audio", (a) => a.paused), true);
    async function seek(t) {
      await page.evaluate((t) => {
        const a = document.querySelector("audio");
        a.pause();
        a.currentTime = t;
        return window.seek(t);
      }, t);
      await page.waitForFunction(() =>
        [...document.querySelectorAll("#hero video")].every(
          (v) => !v.seeking && v.readyState >= 2 && !v.error,
        ),
      );
    }
    // Reverse order catches state left behind by later scenes and stale captions.
    for (const scene of [...film.scenes].reverse()) {
      const cue = film.cues.find(
        ([id]) => id === scene.id || id === scene.id + "-intro",
      );
      const firstPhrase = measured.lines.find((l) => l.id === cue[0])
        .segments[0];
      await seek(cue[1] + (firstPhrase.start_ms + firstPhrase.end_ms) / 2000);
      assert.ok(
        await page.$eval("#subtitle", (e) => e.textContent.length > 0),
        scene.id + " captions",
      );
      if (scene.sector) {
        assert.ok(
          (await page.$eval("#sector-count", (e) => e.textContent)).includes(
            "/ 08",
          ),
        );
        if (scene.sector.clip) {
          const clip = film.clips.find((c) => c.scene === scene.id);
          await seek(clip.start + 8);
          assert.ok(await page.$eval("#hero video", (v) => v.currentTime > 1));
        }
      }
      await page.screenshot({ path: path.join(shots, `film-${scene.id}.png`) });
    }
    for (const [sceneId, selector] of [
      [
        "making-of",
        ".production-flow article, #making-scene h1, .message-command",
      ],
      ["mission", ".orbits, #mission-scene h1, .mission-copy"],
      ["finale", ".final-logo, #finale-scene h1, .final-actions"],
    ]) {
      const scene = film.scenes.find((s) => s.id === sceneId);
      const settled = async () =>
        page.$$eval(selector, (els) =>
          els.map((el) => ({
            rect: el.getBoundingClientRect().toJSON(),
            transform: getComputedStyle(el).transform,
            opacity: getComputedStyle(el).opacity,
          })),
        );
      await seek(scene.start + 3);
      const first = await settled();
      await seek(scene.start + 13);
      assert.deepEqual(
        await settled(),
        first,
        `${sceneId} closing titles remain stationary`,
      );
    }
    for (const line of measured.lines) {
      const cue = film.cues.find(([id]) => id === line.id)[1];
      for (const phrase of line.segments) {
        await seek(cue + (phrase.start_ms + phrase.end_ms) / 2000);
        assert.equal(
          await page.$eval("#subtitle", (e) => e.textContent),
          phrase.text,
        );
      }
    }
    await page.click("#restart");
    await page.waitForFunction(() => {
      const a = document.querySelector("audio");
      return !a.paused && a.currentTime < 2;
    });
    await page.setViewport({ width: 390, height: 844 });
    const mobilePhrase = measured.lines.find((l) => l.id === "s4-complete")
      .segments[1];
    await seek(
      film.cues.find(([id]) => id === "s4-complete")[1] +
        (mobilePhrase.start_ms + mobilePhrase.end_ms) / 2000,
    );
    assert.equal(
      await page.$eval("#stage", (e) => {
        const r = e.getBoundingClientRect();
        return (
          r.left >= -0.5 &&
          r.right <= innerWidth + 0.5 &&
          r.top >= 0 &&
          r.bottom <= innerHeight - 50
        );
      }),
      true,
    );
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth > innerWidth,
      ),
      false,
    );
    assert.equal(
      await page.$eval("#mobile-caption", (e) => getComputedStyle(e).fontSize),
      "24px",
    );
    assert.match(
      await page.$eval("#mobile-provenance", (e) => e.textContent),
      /PRINT VARIANT/,
    );
    assert.ok(
      await page.$eval("#mobile-caption", (e) => e.textContent.length > 0),
    );
    await page.screenshot({ path: path.join(shots, "film-mobile.png") });
    const stl = await page.evaluate(async () => {
      const r = await fetch("../evidence/cake.stl");
      return { status: r.status, bytes: (await r.arrayBuffer()).byteLength };
    });
    assert.equal(stl.status, 200);
    assert.ok(stl.bytes > 100000);
    assert.deepEqual(errors, []);
    console.log(
      "PASS: 8 sectors, 20 cues, matching audio length, actual playback, pause, reverse seeking/captions, all measured caption phrases, stationary closing titles, all seven real recordings, restart, mobile fit, STL download and no asset/page errors",
    );
  } finally {
    await browser.close();
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
