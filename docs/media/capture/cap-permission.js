const {openPortal, record, sleep, stage, clickAt, waitFor} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';
const PROMPT = 'Add a docstring to convert() in src/rates.py.';

(async () => {
  const {browser, page} = await openPortal({width: 1180, height: 820, scale: 2});
  await sleep(6000);                       // let the freshly launched session boot
  await page.evaluate(() => {
    const pill = document.querySelector('.session-pill, [class*="session-pill"], .session-rail button');
    if (pill) pill.click();
  });
  await sleep(3000);
  await stage(page);
  await sleep(600);

  await record(page, {dir: `${ROOT}/frames/perm`, fps: 12, action: async (mark) => {
    await sleep(500);
    await clickAt(page, '[placeholder^="Type your message"]', {settle: 400});
    await page.keyboard.type(PROMPT, {delay: 40});
    await sleep(450);
    await page.keyboard.press('Enter');
    mark('sent');
    await waitFor(page, () => !!document.querySelector('.permission-prompt'), {timeout: 60000});
    mark('card');
    await sleep(1700);
    await clickAt(page, '.permission-options > *:first-child', {settle: 600});
    mark('allow');
    await waitFor(page, () => /updated successfully|has been updated/i.test(document.body.innerText), {timeout: 45000})
      .catch(() => {});
    await sleep(2200);
    mark('end');
  }});
  await browser.close();
})().catch(e => {console.error('ERR', e); process.exit(1);});
