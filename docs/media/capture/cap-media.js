const {openPortal, record, sleep, stage, clickAt, cursorTo, waitFor} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';
const PROMPT = 'Show me signals.riz in the transcript.';

(async () => {
  const {browser, page} = await openPortal({width: 1180, height: 900, scale: 2});
  await sleep(7000);
  await page.evaluate(() => {
    const pill = document.querySelector('.session-pill, [class*="session-pill"], .session-rail button');
    if (pill) pill.click();
  });
  await sleep(3000);
  await stage(page);
  await sleep(600);

  await record(page, {dir: `${ROOT}/frames/media`, fps: 12, action: async (mark) => {
    await sleep(500);
    await clickAt(page, '[placeholder^="Type your message"]', {settle: 380});
    await page.keyboard.type(PROMPT, {delay: 36});
    await sleep(400);
    await page.keyboard.press('Enter');
    mark('sent');
    await waitFor(page, () => /agent-portal show/i.test(document.body.innerText), {timeout: 90000});
    mark('command');
    await waitFor(page, () => !!document.querySelector('.rizzma-figure'), {timeout: 60000}).catch(() => {});
    mark('figure');
    await page.evaluate(() => {
      const fig = document.querySelector('.rizzma-figure');
      if (fig) fig.scrollIntoView({block: 'center'});
    });
    await sleep(1400);
    // The figure ships as a poster until you ask for the runtime; clicking
    // mounts it and starts the animation.
    // Move the cursor to the button, then dispatch the click in-page: headless
    // Chrome's hit-testing misses this overlay button, though a real click on it
    // mounts the figure exactly like this.
    await cursorTo(page, '.rizzma-mount', {settle: 520});
    await page.evaluate(() => {
      const c = document.getElementById('__cur');
      c.classList.remove('__click'); void c.offsetWidth; c.classList.add('__click');
      document.querySelector('.rizzma-mount').click();
    });
    await waitFor(page, () => !!document.querySelector('.rizzma-controls'), {timeout: 30000}).catch(() => {});
    // Mounting leaves the figure paused at 0.0s; press Play.
    await cursorTo(page, '.rizzma-controls button', {settle: 460});
    await page.evaluate(() => {
      const c = document.getElementById('__cur');
      c.classList.remove('__click'); void c.offsetWidth; c.classList.add('__click');
      document.querySelector('.rizzma-controls button').click();
    });
    mark('playing');
    await sleep(7000);
    mark('end');
  }});
  await page.screenshot({path: `${ROOT}/shots/media-final.png`});
  console.log('TAIL:', (await page.evaluate(() => document.body.innerText)).slice(-380).replace(/\n+/g, ' | '));
  await browser.close();
})().catch(e => {console.error('ERR', e.message); process.exit(1);});
