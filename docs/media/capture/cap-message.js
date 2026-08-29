const {execSync} = require('child_process');
const {openPortal, record, sleep, stage, waitFor} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';

const FROM = process.argv[2];   // rate-cache
const TO   = process.argv[3];   // docs-sweep
const TEXT = 'Docstring landed on convert() in rate-cache — can you sweep the README for stale wording?';

(async () => {
  const {browser, page} = await openPortal({width: 1180, height: 820, scale: 2});
  await sleep(2500);
  // open the recipient session so the message lands on camera
  await page.evaluate((name) => {
    const pills = [...document.querySelectorAll('.session-pill, [class*="session-pill"]')];
    const hit = pills.find(p => (p.innerText || '').includes(name));
    (hit || pills[0]).click();
  }, 'docs-sweep');
  await sleep(3000);
  await stage(page);
  await sleep(600);

  await record(page, {dir: `${ROOT}/frames/message`, fps: 12, action: async (mark) => {
    await sleep(1200);
    await page.evaluate(async (to, from, text) => {
      await fetch(`/api/agent/sessions/${to}/message`, {
        method: 'POST', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({message: text, from}),
      });
    }, TO, FROM, TEXT);
    mark('sent');
    await waitFor(page, () => /message from agent|rate-cache/i.test(document.body.innerText), {timeout: 30000}).catch(() => {});
    mark('landed');
    await sleep(9000);
    mark('end');
  }});
  await page.screenshot({path: `${ROOT}/shots/message-final.png`});
  await browser.close();
})().catch(e => {console.error('ERR', e); process.exit(1);});
