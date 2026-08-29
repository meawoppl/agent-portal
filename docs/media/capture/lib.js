const puppeteer = require(`${process.env.DEMO_ROOT || '/tmp/readme-demo'}/node_modules/puppeteer-core`);
const fs = require('fs');
const path = require('path');

const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';
const BASE = process.env.DEMO_URL || 'http://localhost:3100';

async function openPortal({width = 1280, height = 760, scale = 2} = {}) {
  const browser = await puppeteer.launch({
    executablePath: '/usr/bin/google-chrome',
    headless: 'new',
    args: ['--no-sandbox', '--disable-dev-shm-usage', '--hide-scrollbars',
           '--font-render-hinting=none', '--disable-lcd-text',
           // Keep a page painting while it is not the front tab: the
           // side-by-side capture records two pages at once.
           '--disable-backgrounding-occluded-windows',
           '--disable-renderer-backgrounding',
           '--disable-background-timer-throttling',
           `--force-device-scale-factor=${scale}`],
    defaultViewport: {width, height, deviceScaleFactor: scale},
  });
  const page = await browser.newPage();
  await page.goto(BASE + '/', {waitUntil: 'networkidle2'});
  await sleep(1500);
  const login = await page.$('.login-button');
  if (login) { await login.click(); await sleep(3000); }
  await sleep(1500);
  return {browser, page};
}

const sleep = ms => new Promise(r => setTimeout(r, ms));

// Record a screencast while `action` runs, resample to fixed fps, write PNG frames.
async function record(page, {dir, fps = 12, action}) {
  fs.rmSync(dir, {recursive: true, force: true});
  fs.mkdirSync(dir, {recursive: true});
  const client = await page.createCDPSession();
  const frames = [];
  client.on('Page.screencastFrame', async ({data, metadata, sessionId}) => {
    frames.push({t: metadata.timestamp, buf: Buffer.from(data, 'base64')});
    try { await client.send('Page.screencastFrameAck', {sessionId}); } catch (e) {}
  });
  const vp = page.viewport();
  await client.send('Page.startScreencast', {
    format: 'png', everyNthFrame: 1,
    maxWidth: Math.round(vp.width * (vp.deviceScaleFactor || 1)),
    maxHeight: Math.round(vp.height * (vp.deviceScaleFactor || 1)),
  });
  const t0 = Date.now() / 1000;
  const marks = [];
  const mark = label => { marks.push({label, t: Date.now() / 1000 - t0}); console.log(`  mark ${label} @ ${(Date.now()/1000-t0).toFixed(2)}s`); };
  await action(mark);
  const t1 = Date.now() / 1000;
  await sleep(250);
  await client.send('Page.stopScreencast');
  await client.detach();

  if (!frames.length) throw new Error('no frames captured');
  const step = 1 / fps;
  let n = 0;
  for (let t = frames[0].t; t <= Math.max(t1, frames[frames.length - 1].t); t += step) {
    let pick = frames[0];
    for (const f of frames) { if (f.t <= t) pick = f; else break; }
    fs.writeFileSync(path.join(dir, `f${String(n).padStart(4, '0')}.png`), pick.buf);
    n++;
  }
  console.log(`  captured ${frames.length} raw frames -> ${n} @ ${fps}fps (${(t1 - t0).toFixed(1)}s)`);
  const offset = (t0 - frames[0].t);
  for (const m of marks) console.log(`  frame for ${m.label}: ${Math.round((m.t + offset) * fps)}`);
  return n;
}

module.exports = {BASE, openPortal, record, sleep, puppeteer};

// --- staging helpers -------------------------------------------------------

const CURSOR_CSS = `
#__cur { position: fixed; left: 0; top: 0; width: 26px; height: 26px; z-index: 2147483647;
  pointer-events: none; transition: transform 380ms cubic-bezier(.4,0,.2,1);
  filter: drop-shadow(0 2px 3px rgba(0,0,0,.6)); }
#__cur.__click { animation: __curclick 320ms ease-out; }
@keyframes __curclick { 0% { scale: 1 } 40% { scale: .82 } 100% { scale: 1 } }
.dev-mode-banner, [class*="insecure"], [class*="dev-mode-warning"] { display: none !important; }
.focus-flow-header h1 { display: none !important; }
`;

async function stage(page) {
  await page.addStyleTag({content: CURSOR_CSS});
  await page.evaluate(() => {
    // The dev-mode banner is an <h1> in the header; hide it and keep hiding it,
    // since a re-render brings it back.
    // Remove rather than hide: a background tab does not repaint a region that
    // only changed style, so a hidden banner can survive in the screencast.
    // Removing the node forces a layout change, and the header repaints.
    const hideBanner = () => document.querySelectorAll('h1').forEach(e => {
      if (/INSECURE DEV MODE/.test(e.textContent || '')) e.remove();
    });
    hideBanner();
    if (!window.__bannerObserver) {
      window.__bannerObserver = new MutationObserver(hideBanner);
      window.__bannerObserver.observe(document.body, {childList: true, subtree: true});
    }
    if (!document.getElementById('__cur')) {
      const d = document.createElement('div');
      d.id = '__cur';
      d.innerHTML = `<svg viewBox="0 0 24 24" width="26" height="26"><path d="M4 2 L4 20 L9 15.5 L12.2 22 L15.4 20.4 L12.2 14.2 L19 14 Z" fill="#fff" stroke="#1a1b26" stroke-width="1.4" stroke-linejoin="round"/></svg>`;
      document.body.appendChild(d);
      d.style.transform = 'translate(640px, 400px)';
    }
  });
}

async function cursorTo(page, sel, {dx = 0, dy = 0, settle = 460} = {}) {
  const box = await page.evaluate((s, dx, dy) => {
    const el = typeof s === 'string' ? document.querySelector(s) : null;
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return {x: r.left + r.width / 2 + dx, y: r.top + r.height / 2 + dy};
  }, sel, dx, dy);
  if (!box) throw new Error('no element for ' + sel);
  await page.evaluate(({x, y}) => {
    const c = document.getElementById('__cur');
    c.style.transform = `translate(${x - 3}px, ${y - 2}px)`;
  }, box);
  await sleep(settle);
  return box;
}

async function clickAt(page, sel, opts = {}) {
  const box = await cursorTo(page, sel, opts);
  await page.evaluate(() => {
    const c = document.getElementById('__cur');
    c.classList.remove('__click'); void c.offsetWidth; c.classList.add('__click');
  });
  await sleep(120);
  await page.mouse.click(box.x, box.y);
  return box;
}

async function clickNth(page, sel, n, opts = {}) {
  const handle = await page.evaluate((s, n) => {
    const el = document.querySelectorAll(s)[n];
    if (!el) return null;
    el.setAttribute('data-capture-target', '1');
    return true;
  }, sel, n);
  if (!handle) throw new Error(`no ${sel}[${n}]`);
  const r = await clickAt(page, '[data-capture-target="1"]', opts);
  await page.evaluate(() => document.querySelectorAll('[data-capture-target]')
    .forEach(e => e.removeAttribute('data-capture-target')));
  return r;
}

module.exports.stage = stage;
module.exports.cursorTo = cursorTo;
module.exports.clickAt = clickAt;
module.exports.clickNth = clickNth;

// Wait until `fn` returns truthy in the page, polling. Returns elapsed ms.
async function waitFor(page, fn, {timeout = 60000, poll = 250} = {}) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeout) {
    const ok = typeof fn === 'string' ? await page.evaluate(`(() => (${fn}))()`) : await page.evaluate(fn);
    if (ok) return Date.now() - t0;
    await sleep(poll);
  }
  throw new Error('waitFor timed out');
}
module.exports.waitFor = waitFor;

// Key-cap overlay: headless capture has no visible keyboard, so a keystroke is
// invisible unless we draw it.
const KEYCAP_CSS = `
#__keys { position: fixed; left: 50%; bottom: 26px; transform: translateX(-50%);
  display: flex; gap: 8px; z-index: 2147483646; pointer-events: none; }
#__keys .cap { font: 600 15px/1 ui-monospace, SFMono-Regular, Menlo, monospace;
  color: #e6ecff; background: #232838; border: 1px solid #3b425c;
  border-bottom-width: 3px; border-radius: 7px; padding: 9px 12px;
  box-shadow: 0 4px 12px rgba(0,0,0,.5); animation: __capin 140ms ease-out; }
@keyframes __capin { from { transform: translateY(4px); opacity: 0 } to { transform: none; opacity: 1 } }
`;

async function stageKeys(page) {
  await page.addStyleTag({content: KEYCAP_CSS});
  await page.evaluate(() => {
    if (!document.getElementById('__keys')) {
      const d = document.createElement('div');
      d.id = '__keys';
      document.body.appendChild(d);
    }
  });
}

async function pressKey(page, key, label, {hold = 520, modifiers = []} = {}) {
  await page.evaluate((text) => {
    const box = document.getElementById('__keys');
    box.innerHTML = `<span class="cap">${text}</span>`;
  }, label || key);
  await sleep(160);
  for (const m of modifiers) await page.keyboard.down(m);
  await page.keyboard.press(key);
  for (const m of modifiers.slice().reverse()) await page.keyboard.up(m);
  await sleep(hold);
  await page.evaluate(() => { document.getElementById('__keys').innerHTML = ''; });
  await sleep(120);
}

module.exports.stageKeys = stageKeys;
module.exports.pressKey = pressKey;

// Record two pages at once on a shared wall clock, then resample both onto the
// same fps grid so the frames can be composited side by side.
async function record2(pages, {dirs, fps = 12, action}) {
  const fs = require('fs'), path = require('path');
  const sessions = [];
  for (const p of pages) {
    const client = await p.createCDPSession();
    const frames = [];
    client.on('Page.screencastFrame', async ({data, metadata, sessionId}) => {
      frames.push({t: metadata.timestamp, buf: Buffer.from(data, 'base64')});
      try { await client.send('Page.screencastFrameAck', {sessionId}); } catch (e) {}
    });
    const vp = p.viewport();
    await client.send('Page.startScreencast', {
      format: 'png', everyNthFrame: 1,
      maxWidth: Math.round(vp.width * (vp.deviceScaleFactor || 1)),
      maxHeight: Math.round(vp.height * (vp.deviceScaleFactor || 1)),
    });
    sessions.push({client, frames});
  }
  const t0 = Date.now() / 1000;
  const marks = [];
  const mark = label => { marks.push({label, t: Date.now() / 1000 - t0});
    console.log(`  mark ${label} @ ${(Date.now() / 1000 - t0).toFixed(2)}s`); };
  await action(mark);
  const t1 = Date.now() / 1000;
  await sleep(250);
  for (const s of sessions) { await s.client.send('Page.stopScreencast'); await s.client.detach(); }

  // Shared grid: start when both have a frame, end at the last event.
  const start = Math.max(...sessions.map(s => s.frames[0].t));
  const step = 1 / fps;
  let n = 0;
  for (const d of dirs) { fs.rmSync(d, {recursive: true, force: true}); fs.mkdirSync(d, {recursive: true}); }
  for (let t = start; t <= t1 + (start - t0); t += step) {
    sessions.forEach((s, i) => {
      let pick = s.frames[0];
      for (const f of s.frames) { if (f.t <= t) pick = f; else break; }
      fs.writeFileSync(path.join(dirs[i], `f${String(n).padStart(4, '0')}.png`), pick.buf);
    });
    n++;
  }
  console.log(`  ${n} aligned frame pairs @ ${fps}fps`);
  for (const m of marks) console.log(`  frame for ${m.label}: ${Math.round((m.t + (t0 - start)) * fps)}`);
  return n;
}
module.exports.record2 = record2;
