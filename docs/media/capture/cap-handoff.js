const {puppeteer, record2, sleep, stage, clickAt, waitFor, BASE} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';
const PROMPT = 'Which module owns the TTL constant?';

// Each pane gets its OWN browser: two tabs in one browser cannot both paint,
// so the screencast of the non-front tab serves stale pixels no matter how the
// backgrounding flags are set.
const openPane = async (vp, name) => {
  const browser = await puppeteer.launch({
    executablePath: '/usr/bin/google-chrome', headless: 'new',
    args: ['--no-sandbox', '--disable-dev-shm-usage', '--hide-scrollbars',
           '--font-render-hinting=none', '--disable-lcd-text',
           '--disable-backgrounding-occluded-windows',
           '--disable-renderer-backgrounding',
           '--disable-background-timer-throttling',
           `--force-device-scale-factor=${vp.deviceScaleFactor}`],
    defaultViewport: vp,
  });
  const page = await browser.newPage();
  await page.goto(BASE + '/', {waitUntil: 'networkidle2'});
  await sleep(1200);
  const login = await page.$('.login-button');
  if (login) { await login.click(); await sleep(3000); }
  await waitFor(page, `[...document.querySelectorAll('.session-pill, [class*="session-pill"]')]
      .some(e => (e.innerText || '').includes('${name}'))`, {timeout: 30000});
  await sleep(1200);
  await page.evaluate(n => {
    const p = [...document.querySelectorAll('.session-pill, [class*="session-pill"]')]
      .find(e => (e.innerText || '').includes(n));
    if (p) p.click();
  }, name);
  await sleep(2500);
  return {browser, page};
};

(async () => {
  const d = await openPane({width: 1180, height: 760, deviceScaleFactor: 2}, 'claude-api');
  const f = await openPane({width: 390, height: 844, deviceScaleFactor: 2}, 'claude-api');
  const desktop = d.page, phone = f.page;
  await stage(desktop); await stage(phone);
  await sleep(900);

  await record2([desktop, phone], {
    dirs: [`${ROOT}/frames/hand-desk`, `${ROOT}/frames/hand-phone`], fps: 12,
    action: async (mark) => {
      const before = await desktop.evaluate(() =>
        document.querySelectorAll('.turn-metrics-footer').length);
      await sleep(900);
      // The ask comes from the phone.
      await clickAt(phone, '[placeholder^="Type your message"]', {settle: 380});
      await phone.keyboard.type(PROMPT, {delay: 44});
      await sleep(400);
      await phone.keyboard.press('Enter');
      mark('sent_from_phone');
      // ...and lands on the desktop, live.
      await waitFor(desktop, `document.body.innerText.includes(${JSON.stringify(PROMPT)})`, {timeout: 30000});
      mark('on_desktop');
      await waitFor(desktop, `document.querySelectorAll('.turn-metrics-footer').length > ${before}`,
                    {timeout: 120000}).catch(e => console.log('  answer wait:', e.message));
      mark('answered');
      await sleep(3200);
      mark('end');
    },
  });
  await desktop.screenshot({path: `${ROOT}/shots/hand-desk.png`});
  await phone.screenshot({path: `${ROOT}/shots/hand-phone.png`});
  await d.browser.close(); await f.browser.close();
})().catch(e => {console.error('ERR', e.message); process.exit(1);});
