// Self-contained browser smoke test for the public exhibit; no backend or credentials.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const root = process.env.DEMO_ROOT || "/tmp/readme-demo";
const base = process.env.DEMO_URL || "http://localhost:8792";
const shots = path.join(root, "aperture-qa");
fs.mkdirSync(shots, { recursive: true });
const puppeteer = require(path.join(root, "node_modules/puppeteer-core"));
async function screenshot(page, name) {
  await page.evaluate(() =>
    Promise.all(
      document
        .getAnimations()
        .filter((a) => Number.isFinite(a.effect.getComputedTiming().endTime))
        .map((a) => a.finished.catch(() => {})),
    ),
  );
  await page.screenshot({ path: path.join(shots, name), fullPage: true });
}
(async () => {
  const browser = await puppeteer.launch({
    executablePath: "/usr/bin/google-chrome",
    headless: true,
    args: ["--no-sandbox"],
  });
  try {
    const page = await browser.newPage();
    await page.evaluateOnNewDocument(() => {
      window.__apertureAudio = [];
      const OriginalAudio = window.Audio;
      window.Audio = class extends OriginalAudio {
        constructor(...args) {
          super(...args);
          window.__apertureAudio.push(this);
        }
      };
    });
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await page.setViewport({ width: 1440, height: 1000 });
    await page.goto(base + "/aperture", { waitUntil: "domcontentloaded" });
    await page.waitForSelector(".ap-prologue");
    assert.equal(
      await page.$$eval(".ap-chamber-tab", (tabs) => tabs.length),
      8,
    );
    assert.match(
      await page.$eval(".ap-prologue", (e) => e.textContent),
      /First, we took/,
    );
    await screenshot(page, "prologue.png");
    for (const width of [320, 390]) {
      await page.setViewport({ width, height: 844 });
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth > innerWidth,
        ),
        false,
      );
      await screenshot(page, `prologue-${width}.png`);
    }
    await page.setViewport({ width: 1440, height: 1000 });
    await page.click(".ap-sound");
    await page.waitForFunction(() =>
      window.__apertureAudio.some((a) => a.currentTime > 0.15),
    );
    assert.equal(
      await page.evaluate(
        () => window.__apertureAudio.filter((a) => !a.paused).length,
      ),
      1,
    );
    await page.click(".ap-begin");
    await page.waitForFunction(() =>
      window.__apertureAudio.some(
        (a) => a.src.includes("s1-intro") && a.currentTime > 0.1,
      ),
    );
    await page.click(".ap-deck-actions .ap-primary");
    await page.waitForSelector(".ap-agent-result");
    await screenshot(page, "code-review.png");
    await page.click(".ap-next");
    await page.$eval(".ap-tool-evidence video", (e) => e.play());
    await page.waitForFunction(
      () => document.querySelector(".ap-tool-evidence video").currentTime > 0.2,
    );
    await page.click(".ap-orange");
    await page.waitForFunction(() =>
      document
        .querySelector(".ap-room-terminal")
        .textContent.includes("lab-b.internal"),
    );
    assert.match(
      await page.$eval(".ap-test-state", (e) => e.textContent),
      /COMPLETE/,
    );
    await page.click(".ap-next");
    await page.waitForFunction(() =>
      window.__apertureAudio.some(
        (a) => a.src.includes("s3-intro") && a.currentTime > 0.1,
      ),
    );
    assert.equal(
      await page.evaluate(
        () => window.__apertureAudio.filter((a) => !a.paused).length,
      ),
      1,
    );
    await page.click(".ap-sound");
    assert.equal(
      await page.evaluate(
        () => window.__apertureAudio.filter((a) => !a.paused).length,
      ),
      0,
    );
    await page.click(".ap-primary");
    await page.waitForSelector("iframe.ap-sample-site");
    const iframe = await (await page.$("iframe.ap-sample-site")).contentFrame();
    await iframe.waitForSelector("#rotate");
    await iframe.click("#rotate");
    assert.match(await iframe.$eval("#status", (e) => e.textContent), /90/);
    await iframe.click("#launch");
    await iframe.waitForFunction(() =>
      document.querySelector("#status").textContent.includes("LANDED"),
    );
    await screenshot(page, "websites.png");
    await page.click(".ap-deck-actions .ap-primary");
    await browser
      .defaultBrowserContext()
      .overridePermissions(new URL(base).origin, [
        "clipboard-read",
        "clipboard-write",
        "clipboard-sanitized-write",
      ]);
    await page.click(".ap-invitation button");
    await page.waitForFunction(() =>
      document
        .querySelector(".ap-invitation [role=status]")
        .textContent.includes("copied"),
    );
    assert.match(
      await page.evaluate(() => navigator.clipboard.readText()),
      /#observer-viewer$/,
    );
    await page.evaluate(() =>
      Object.defineProperty(navigator.clipboard, "writeText", {
        value: () =>
          Promise.reject(new Error("Permission denied in regression test")),
        configurable: true,
      }),
    );
    await page.click(".ap-invitation button");
    await page.waitForFunction(() =>
      document
        .querySelector(".ap-invitation [role=status]")
        .textContent.includes("Select and copy"),
    );
    const invitation = await page.$eval("#ap-share-url", (e) => e.value);
    assert.match(invitation, /#observer-viewer$/);
    const guest = await browser.newPage();
    await guest.goto(invitation, { waitUntil: "domcontentloaded" });
    assert.match(
      await guest.$eval(".ap-observer-banner", (e) => e.textContent),
      /viewer demo invitation/,
    );
    await guest.goto(invitation.replace("observer-viewer", "observer-editor"), {
      waitUntil: "domcontentloaded",
    });
    await guest.reload({ waitUntil: "domcontentloaded" });
    await guest.waitForSelector(".ap-observer-banner");
    assert.match(
      await guest.$eval(".ap-observer-banner", (e) => e.textContent),
      /editor demo invitation/,
    );
    assert.match(
      await guest.$eval(
        ".ap-role-option[aria-pressed=true]",
        (e) => e.textContent,
      ),
      /Editor/,
    );
    await guest.close();
    await page.click(".ap-role-option:nth-child(2)");
    assert.equal(await page.$(".ap-invitation"), null);
    await page.click(".ap-primary");
    assert.match(
      await page.$eval("#ap-share-url", (e) => e.value),
      /#observer-editor$/,
    );
    await screenshot(page, "sharing.png");
    await page.click(".ap-next");
    const recordings = ["cake", "boards", "visilog", "controls", "briefing"];
    for (let sector = 3; sector < 8; sector++) {
      await page.waitForSelector(".ap-plugins-room");
      const recording = recordings[sector - 3];
      assert.ok(
        (await page.$eval(".ap-announcer p", (e) => e.textContent)).trim(),
        `Missing sector ${sector + 1} narration`,
      );
      assert.match(
        await page.$eval(".ap-tool-evidence source", (e) => e.src),
        new RegExp(recording + "\\.mp4$"),
      );
      assert.equal(await page.$(".ap-next"), null);
      await page.$eval(".ap-tool-evidence video", (e) => e.play());
      await page.waitForFunction(
        () =>
          document.querySelector(".ap-tool-evidence video").currentTime > 0.2,
      );
      if (sector === 3) {
        assert.match(
          await page.$eval(".ap-cake-download", (e) => e.href),
          /cake\.stl$/,
        );
      }
      await page.click(".ap-deck-actions .ap-primary");
      assert.equal(
        await page.$eval(".ap-deck-actions .ap-primary", (e) => e.disabled),
        true,
      );
      // Preserve each observation across sector navigation.
      await page.click(".ap-chamber-tab:nth-child(1)");
      await page.click(`.ap-chamber-tab:nth-child(${sector + 1})`);
      assert.match(
        await page.$eval(".ap-test-state", (e) => e.textContent),
        /COMPLETE/,
      );
      await screenshot(page, `sector-${sector + 1}.png`);
      await page.click(".ap-next");
    }
    await page.waitForSelector(".ap-making-of");
    assert.match(
      await page.$eval(".ap-making-of", (e) => e.textContent),
      /human meat proxy/,
    );
    await screenshot(page, "making-of.png");
    await page.click(".ap-making-of .ap-primary");
    await page.waitForSelector(".ap-refinement:not(.ap-making-of)");
    assert.match(
      await page.$eval(".ap-announcer p", (e) => e.textContent),
      /universe|explor|Cosmic|frontier/i,
    );
    await screenshot(page, "refinement.png");
    for (const width of [320, 390]) {
      await page.setViewport({ width, height: 844 });
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth > innerWidth,
        ),
        false,
      );
      await screenshot(page, `refinement-${width}.png`);
    }
    await page.setViewport({ width: 1440, height: 1000 });
    await page.click(".ap-refinement .ap-primary");
    await page.waitForSelector(".ap-graduation");
    assert.match(
      await page.$eval(".ap-footer", (e) => e.textContent),
      /08 \/ 08/,
    );
    await screenshot(page, "finale.png");
    await page.click(".ap-final-actions button");
    assert.match(
      await page.$eval(".ap-prologue", (e) => e.textContent),
      /First, we took/,
    );
    assert.match(
      await page.$eval(".ap-footer", (e) => e.textContent),
      /00 \/ 08/,
    );
    for (const width of [320, 390, 768, 1024, 1440]) {
      await page.setViewport({ width, height: 844 });
      for (let chamber = 0; chamber < 8; chamber++) {
        await page.click(`.ap-chamber-tab:nth-child(${chamber + 1})`);
        const overflow = await page.evaluate(
          () => document.documentElement.scrollWidth > window.innerWidth,
        );
        assert.equal(overflow, false, `Overflow ${width} chamber ${chamber}`);
        if (width === 390) await screenshot(page, `mobile-${chamber + 1}.png`);
      }
    }
    await page.click(".ap-chamber-tab:nth-child(1)");
    await page.emulateMediaFeatures([
      { name: "prefers-reduced-motion", value: "reduce" },
    ]);
    assert.equal(
      await page.$eval(
        ".ap-agent-wire",
        (e) => getComputedStyle(e).animationName,
      ),
      "none",
    );
    assert.deepEqual(errors, []);
    console.log(
      "PASS: software prologue, eight sectors, dedicated engineering recordings, real tool videos, refinement, sample app, viewer/editor invitations, finale/reset, five viewport widths, reduced motion, audio cancellation, clipboard and denied-clipboard fallback; zero page errors",
    );
  } finally {
    await browser.close();
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
