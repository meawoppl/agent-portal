const {openPortal, record, sleep, stage, clickAt, clickNth} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';

(async () => {
  const {browser, page} = await openPortal({width: 1180, height: 940, scale: 2});
  await stage(page);
  await sleep(800);

  await record(page, {dir: `${ROOT}/frames/launch`, fps: 12, action: async () => {
    await sleep(600);
    await clickAt(page, '.new-session-button');          // open dialog
    await sleep(1000);
    await clickNth(page, '.dir-entry', 1, {settle: 520}); // pick ~/acme-api
    await sleep(700);

    // model: Haiku 4.5
    await cursorHover(page, 'select.launcher-select', 2);
    await page.evaluate(() => {
      const s = document.querySelectorAll('select.launcher-select')[2];
      s.value = 'claude-haiku-4-5';
      s.dispatchEvent(new Event('change', {bubbles: true}));
    });
    await sleep(700);

    // session name
    await clickAt(page, 'input[placeholder="defaults to the folder name"]', {settle: 420});
    await page.type('input[placeholder="defaults to the folder name"]', 'rate-cache', {delay: 55});
    await sleep(600);

    await clickNth(page, 'input[type=checkbox]', 1, {settle: 520}); // worktree
    await sleep(700);
    await clickAt(page, '.launch-button', {settle: 560});
    await sleep(5500);
  }});

  await page.screenshot({path: `${ROOT}/shots/launch-final.png`});
  console.log('final text:', (await page.evaluate(() => document.body.innerText)).slice(0, 300).replace(/\n+/g, ' | '));
  await browser.close();
})().catch(e => {console.error('ERR', e); process.exit(1);});

async function cursorHover(page, sel, n) {
  const {cursorTo} = require('./lib.js');
  await page.evaluate((s, n) => { document.querySelectorAll(s)[n].setAttribute('data-capture-target','1'); }, sel, n);
  await cursorTo(page, '[data-capture-target="1"]', {settle: 400});
  await page.evaluate(() => document.querySelectorAll('[data-capture-target]').forEach(e => e.removeAttribute('data-capture-target')));
}
