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
        (a) => a.src.includes("websites-intro") && a.currentTime > 0.1,
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
    await page.click(".ap-next");
    await page.click(".ap-primary");
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
    await page.click(".ap-primary");
    await page.waitForSelector(".ap-agent-result");
    await screenshot(page, "agents.png");
    await page.click(".ap-next");
    await page.waitForSelector(".ap-plugins-room");
    assert.match(
      await page.$eval(".ap-announcer p", (e) => e.textContent),
      /Chamber five/,
    );
    await page.click(".ap-sound");
    await page.waitForFunction(() =>
      window.__apertureAudio.some(
        (a) => a.src.includes("plugins-intro") && a.currentTime > 0.15,
      ),
    );
    await page.click(".ap-sound");
    for (let stage = 0; stage < 4; stage++) {
      assert.equal(await page.$(".ap-next"), null);
      await page.click(".ap-deck-actions .ap-primary");
      assert.equal(
        await page.$eval(
          ".ap-departments",
          (e) => e.querySelectorAll(".done").length,
        ),
        stage + 1,
      );
      assert.match(
        await page.$eval(".ap-plugin-evidence a", (e) => e.href),
        /agent-portal-plugins\/tree\//,
      );
      if (stage < 2) {
        await page.$eval(".ap-tool-evidence video", (e) => e.play());
        await page.waitForFunction(
          () =>
            document.querySelector(".ap-tool-evidence video").currentTime > 0.2,
        );
      }
      await screenshot(page, `plugins-${stage + 1}.png`);
      if (stage === 2) {
        // Leaving and returning preserves the design series without graduating.
        await page.click(".ap-chamber-tab:nth-child(1)");
        await page.click(".ap-chamber-tab:nth-child(5)");
      }
    }
    assert.equal(
      await page.$eval(".ap-deck-actions .ap-primary", (e) => e.disabled),
      true,
    );
    assert.match(
      await page.$eval(".ap-announcer p", (e) => e.textContent),
      /performance review/,
    );
    await page.click(".ap-next");
    await page.waitForSelector(".ap-electronics-room");
    await page.$eval(".ap-tool-evidence video", (e) => e.play());
    await page.waitForFunction(
      () => document.querySelector(".ap-tool-evidence video").currentTime > 0.2,
    );
    await page.click(".ap-deck-actions .ap-primary");
    await screenshot(page, "electronics.png");
    await page.click(".ap-next");
    await page.waitForSelector(".ap-refinement");
    assert.match(
      await page.$eval(".ap-announcer p", (e) => e.textContent),
      /Every improvement/,
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
      /06 \/ 06/,
    );
    await screenshot(page, "finale.png");
    await page.click(".ap-final-actions button");
    assert.match(
      await page.$eval(".ap-prologue", (e) => e.textContent),
      /First, we took/,
    );
    assert.match(
      await page.$eval(".ap-footer", (e) => e.textContent),
      /00 \/ 06/,
    );
    for (const width of [320, 390, 768, 1024, 1440]) {
      await page.setViewport({ width, height: 844 });
      for (let chamber = 0; chamber < 6; chamber++) {
        await page.click(`.ap-chamber-tab:nth-child(${chamber + 1})`);
        const overflow = await page.evaluate(
          () => document.documentElement.scrollWidth > window.innerWidth,
        );
        assert.equal(overflow, false, `Overflow ${width} chamber ${chamber}`);
        if (width === 390) await screenshot(page, `mobile-${chamber + 1}.png`);
      }
    }
    await page.click(".ap-chamber-tab:nth-child(4)");
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
      "PASS: software prologue, six chambers, four plugin departments, real tool videos, refinement, sample app, viewer/editor invitations, finale/reset, five viewport widths, reduced motion, audio cancellation, clipboard and denied-clipboard fallback; zero page errors",
    );
  } finally {
    await browser.close();
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
