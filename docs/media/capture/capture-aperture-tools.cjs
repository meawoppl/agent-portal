const { record, sleep, puppeteer: p } = require("./lib.js");
const fs = require("fs");
const cp = require("child_process");
const root = "builds/aperture-evidence";
(async () => {
  const b = await p.launch({
    executablePath: "/usr/bin/google-chrome",
    headless: true,
    args: [
      "--no-sandbox",
      "--window-size=2000,1200",
      "--use-gl=angle",
      "--use-angle=swiftshader",
      "--enable-unsafe-swiftshader",
    ],
    defaultViewport: { width: 1920, height: 1080 },
  });
  try {
    const page = await b.newPage();
    const name = process.argv[2];
    await page.goto(
      "http://127.0.0.1:" +
        (name === "yapcad" ? 8794 : name === "visilog" ? 8795 : 8796),
      { waitUntil: "domcontentloaded" },
    );
    await sleep(2500);
    let action;
    if (name === "yapcad") {
      await page.waitForFunction(
        () => document.querySelector("#stats")?.children.length > 0,
      );
      const initial = await page.$("#params input[type=number]");
      await initial.click({ clickCount: 3 });
      await initial.type("20");
      await initial.press("Tab");
      await sleep(1800);
      action = async () => {
        await sleep(1500);
        await page.mouse.move(1170, 580);
        await page.mouse.down();
        await page.mouse.move(1320, 625, { steps: 80 });
        await page.mouse.up();
        await sleep(1000);
        await page.select("#view", "iso");
        const n = await page.$("#params input[type=number]");
        await n.click({ clickCount: 3 });
        await n.type("34", { delay: 100 });
        await n.press("Tab");
        await sleep(2500);
        await page.mouse.move(1190, 560);
        await page.mouse.down();
        await page.mouse.move(1050, 610, { steps: 80 });
        await page.mouse.up();
        await sleep(2000);
      };
    } else if (name === "visilog") {
      await page.click("#load");
      await sleep(2000);
      const f = page.frames()[1];
      action = async () => {
        await sleep(1000);
        for (let i = 0; i < 10; i++) {
          await f.click("#run-edge");
          await sleep(350);
        }
        await f.click("#break-expr");
        await f.type("#break-expr", "counter_tb.count == 10", { delay: 55 });
        await f.click("#run-break");
        await sleep(2000);
        await f.click("#run-all");
        await sleep(2000);
        console.log(await f.$eval("body", (e) => e.innerText));
      };
    } else {
      action = async () => {
        await sleep(2000);
        await page.select("select", "routed");
        await sleep(3000);
        await page.$$eval("button", (els) =>
          els.find((e) => e.textContent === "3D").click(),
        );
        await page.waitForSelector(".modelv-canvas");
        await sleep(3000);
        await page.mouse.move(1050, 570);
        await page.mouse.down();
        await page.mouse.move(1340, 680, { steps: 100 });
        await page.mouse.up();
        await sleep(2000);
      };
    }
    const dir = `${root}/${name}/frames`;
    await record(page, { dir, fps: 24, action });
    await page.screenshot({ path: `${root}/${name}/poster.png` });
    cp.execFileSync("ffmpeg", [
      "-y",
      "-loglevel",
      "error",
      "-framerate",
      "24",
      "-i",
      `${dir}/f%04d.png`,
      "-c:v",
      "libx264",
      "-crf",
      "19",
      "-pix_fmt",
      "yuv420p",
      "-movflags",
      "+faststart",
      `${root}/${name}/tool.mp4`,
    ]);
    fs.rmSync(dir, { recursive: true });
  } finally {
    await b.close();
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
