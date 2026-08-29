const {openPortal, record, sleep, stage, stageKeys, pressKey, waitFor, clickAt} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';
const WAITING = process.env.WAITING_SID;

(async () => {
  const {browser, page} = await openPortal({width: 1180, height: 820, scale: 2});
  await sleep(9000);

  // Give one session something that needs an answer, so `w` has somewhere to go.
  if (WAITING) {
    await page.evaluate(async (sid) => {
      await fetch(`/api/agent/sessions/${sid}/message`, {
        method: 'POST', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({message: 'Run `git status --porcelain` and tell me what changed.'}),
      });
    }, WAITING);
  }
  await sleep(14000);   // let it reach the permission prompt

  // Start on a session that is NOT the one waiting, so `w` has somewhere to go.
  await page.evaluate(() => {
    const pills = [...document.querySelectorAll('.session-pill, [class*="session-pill"]')];
    const start = pills.find(p => /rate-cache/.test(p.innerText || '')) || pills[0];
    start.click();
  });
  await sleep(2500);
  await stage(page);
  await stageKeys(page);
  await sleep(700);

  await record(page, {dir: `${ROOT}/frames/nav`, fps: 12, action: async (mark) => {
    await sleep(700);
    await pressKey(page, 'k', 'Ctrl + K', {modifiers: ['Control'], hold: 900});
    mark('navmode');
    await pressKey(page, 'ArrowDown', '↓', {hold: 700});
    await pressKey(page, 'ArrowDown', '↓', {hold: 700});
    mark('moved');
    await pressKey(page, '1', '1', {hold: 1200});   // jump straight to a session by number
    mark('numbered');
    await pressKey(page, 'Enter', '⏎', {hold: 1200});
    mark('accepted');
    await sleep(1400);
    mark('end');
  }});
  await page.screenshot({path: `${ROOT}/shots/nav-final.png`});
  await browser.close();
})().catch(e => {console.error('ERR', e.message); process.exit(1);});
