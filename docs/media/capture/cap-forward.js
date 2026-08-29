const {openPortal, record, sleep, stage, clickAt, waitFor} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';
const PROMPT = 'Serve target/doc on port 8899, then run agent-portal forward 8899.';
const SID = process.env.DEMO_SID;

(async () => {
  const {browser, page} = await openPortal({width: 1180, height: 860, scale: 2});
  await sleep(7000);                    // session boots
  await page.evaluate(() => {
    const pill = document.querySelector('.session-pill, [class*="session-pill"], .session-rail button');
    if (pill) pill.click();
  });
  await sleep(3000);
  await stage(page);
  await sleep(600);

  await record(page, {dir: `${ROOT}/frames/forward`, fps: 12, action: async (mark) => {
    await sleep(500);
    await clickAt(page, '[placeholder^="Type your message"]', {settle: 380});
    await page.keyboard.type(PROMPT, {delay: 34});
    await sleep(400);
    await page.keyboard.press('Enter');
    mark('sent');

    // the agent starts the server, then asks the portal for a door
    await waitFor(page, () => /http\.server/i.test(document.body.innerText), {timeout: 90000});
    mark('server');
    await waitFor(page, () => !!document.querySelector('.forward-chip'), {timeout: 90000});
    mark('chip');
    // Dev-only: *.localhost is cross-site, so a private forward's cookie is not
    // sent to the iframe. Public needs no cookie. On a real domain the private
    // preview works as shipped.
    await page.evaluate(async (sid) => {
      await fetch(`/api/sessions/${sid}/forwards/public`, {
        method: 'PATCH', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({public: true}),
      });
    }, process.env.DEMO_SID);
    await waitFor(page, () => !!document.querySelector('.forward-chip.is-up'), {timeout: 30000}).catch(() => {});
    mark('green');
    await sleep(2200);

    await clickAt(page, '.forward-chip a', {settle: 560});
    await waitFor(page, () => !!document.querySelector('.forward-preview-bar'), {timeout: 20000});
    mark('preview');
    await sleep(2600);

    // Prove the embedded app is live, not a screenshot: click through to a
    // module page inside the tunnelled rustdoc.
    const frame = page.frames().find(f => f !== page.mainFrame() && /localhost:3100/.test(f.url()));
    if (frame) {
      try {
        await frame.click('a[href="figure/index.html"], a.mod, ul.block li a');
        mark('clicked');
      } catch (e) { console.log('  iframe click failed:', e.message); }
    } else {
      console.log('  no iframe handle');
    }
    await sleep(2800);
    mark('end');
  }});
  await page.screenshot({path: `${ROOT}/shots/forward-final.png`});
  console.log('TAIL:', (await page.evaluate(() => document.body.innerText)).slice(-500).replace(/\n+/g, ' | '));
  await browser.close();
})().catch(e => {console.error('ERR', e.message); process.exit(1);});
