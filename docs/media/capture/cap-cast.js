const {puppeteer, record, sleep, waitFor} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';

(async () => {
  const browser = await puppeteer.launch({
    executablePath: '/usr/bin/google-chrome', headless: 'new',
    args: ['--no-sandbox', '--hide-scrollbars', '--font-render-hinting=none',
           '--disable-lcd-text', '--force-device-scale-factor=2'],
    defaultViewport: {width: 980, height: 600, deviceScaleFactor: 2},
  });
  const page = await browser.newPage();
  await page.goto(`file://${__dirname}/cast.html`, {waitUntil: 'networkidle2'});
  await sleep(400);

  await record(page, {dir: `${ROOT}/frames/cast`, fps: 12, action: async (mark) => {
    await waitFor(page, () => window.__done === true, {timeout: 120000});
    mark('done');
  }});
  await page.screenshot({path: `${ROOT}/shots/cast-final.png`});
  await browser.close();
})().catch(e => {console.error('ERR', e.message); process.exit(1);});
