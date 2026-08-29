const {openPortal, record, sleep, stage, clickAt, cursorTo, waitFor} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';
const PROMPT = 'Read each module in src and give me a one-line summary of each.';

(async () => {
  const {browser, page} = await openPortal({width: 1180, height: 820, scale: 2});
  await sleep(7000);
  await page.evaluate(() => {
    const pill = document.querySelector('.session-pill, [class*="session-pill"]');
    if (pill) pill.click();
  });
  await sleep(3000);
  // Warm-up turn, off camera: the agent's very first turn tends to recite the
  // environment it was handed (including the account email Claude Code injects),
  // so push it out of view before recording.
  await page.click('[placeholder^="Type your message"]');
  await page.keyboard.type('List the files in src.');
  await page.keyboard.press('Enter');
  await waitFor(page, () => document.querySelectorAll('.turn-metrics-footer').length >= 2,
                {timeout: 120000}).catch(e => console.log('  warm-up wait:', e.message));
  await sleep(2500);
  await page.evaluate(() => {
    const el = document.querySelector('[class*="transcript"], [class*="messages"]') || document.scrollingElement;
    el.scrollTop = el.scrollHeight;
  });
  await stage(page);
  await sleep(600);

  await record(page, {dir: `${ROOT}/frames/metrics`, fps: 12, action: async (mark) => {
    const before = await page.evaluate(() => document.querySelectorAll('.turn-metrics-footer').length);
    console.log('  footers before:', before);
    await sleep(400);
    await clickAt(page, '[placeholder^="Type your message"]', {settle: 340});
    await page.keyboard.type(PROMPT, {delay: 30});
    await page.keyboard.press('Enter');
    mark('sent');
    // let the sparkline accumulate turns
    await waitFor(page, `document.querySelectorAll('.turn-metrics-footer').length > ${before}`,
                  {timeout: 120000}).catch(e => console.log('  turn wait:', e.message));
    mark('turn_done');
    await sleep(1800);

    // open the metric picker and switch what the sparkline plots
    await clickAt(page, '.turn-metrics-pill', {settle: 520});
    await waitFor(page, () => !!document.querySelector('.turn-metrics-pill-menu'), {timeout: 8000});
    mark('menu');
    await sleep(1200);
    const items = await page.$$eval('.turn-metrics-pill-menu button', bs => bs.map(b => b.innerText));
    console.log('  metrics:', JSON.stringify(items));
    const idx = items.findIndex(t => /cache/i.test(t));
    if (idx >= 0) {
      await page.evaluate(i => document.querySelectorAll('.turn-metrics-pill-menu button')[i]
        .setAttribute('data-capture-target', '1'), idx);
      await clickAt(page, '[data-capture-target="1"]', {settle: 460});
    }
    mark('switched');
    await sleep(2600);
    mark('end');
  }});
  await page.screenshot({path: `${ROOT}/shots/metrics-final.png`});
  await browser.close();
})().catch(e => {console.error('ERR', e.message); process.exit(1);});
