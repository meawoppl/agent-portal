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
`;

async function stage(page) {
  await page.addStyleTag({content: CURSOR_CSS});
  await page.evaluate(() => {
    document.querySelectorAll('*').forEach(e => {
      const t = (e.innerText || '').trim();
      if (t.startsWith('⚠️ INSECURE DEV MODE') && t.length < 40) e.style.display = 'none';
    });
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
    if (await page.evaluate(fn)) return Date.now() - t0;
    await sleep(poll);
  }
  throw new Error('waitFor timed out');
}
module.exports.waitFor = waitFor;
