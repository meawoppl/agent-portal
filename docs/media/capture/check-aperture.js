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
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await page.setViewport({ width: 1440, height: 1000 });
    await page.goto(base + "/aperture", { waitUntil: "networkidle0" });
    await page.waitForSelector(".ap-orange");
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
    const invitation = await page.$eval("#ap-share-url", (e) => e.value);
    assert.match(invitation, /#observer-viewer$/);
    const guest = await browser.newPage();
    await guest.goto(invitation, { waitUntil: "networkidle0" });
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
    await page.waitForSelector(".ap-graduation");
    assert.match(
      await page.$eval(".ap-footer", (e) => e.textContent),
      /04 \/ 04/,
    );
    await screenshot(page, "finale.png");
    await page.click(".ap-final-actions button");
    assert.match(
      await page.$eval(".ap-room-terminal", (e) => e.textContent),
      /macbook.local/,
    );
    assert.match(
      await page.$eval(".ap-footer", (e) => e.textContent),
      /00 \/ 04/,
    );
    for (const width of [320, 390, 768, 1024, 1440]) {
      await page.setViewport({ width, height: 844 });
      for (let chamber = 0; chamber < 4; chamber++) {
        await page.click(`.ap-chamber-tab:nth-child(${chamber + 1})`);
        const overflow = await page.evaluate(
          () => document.documentElement.scrollWidth > window.innerWidth,
        );
        assert.equal(overflow, false, `Overflow ${width} chamber ${chamber}`);
        if (width === 390) await screenshot(page, `mobile-${chamber + 1}.png`);
      }
    }
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
      "PASS: all four chambers, sample app, viewer/editor invitations, finale/reset, five viewport widths, reduced motion; zero page errors",
    );
  } finally {
    await browser.close();
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
