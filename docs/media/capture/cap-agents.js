const {openPortal, record, sleep, stage, clickAt, waitFor} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';

const pickPill = (name) => `[...document.querySelectorAll('.session-pill, [class*="session-pill"]')]
  .find(p => (p.innerText || '').includes('${name}'))`;

(async () => {
  const {browser, page} = await openPortal({width: 1180, height: 820, scale: 2});
  await sleep(4000);
  await page.evaluate(n => {
    const p = [...document.querySelectorAll('.session-pill, [class*="session-pill"]')]
      .find(e => (e.innerText || '').includes(n));
    if (p) p.click();
  }, 'claude-api');
  await sleep(3000);
  await stage(page);
  await sleep(600);

  const toPill = async (name, mark, label) => {
    await page.evaluate(n => {
      const p = [...document.querySelectorAll('.session-pill, [class*="session-pill"]')]
        .find(e => (e.innerText || '').includes(n));
      if (p) p.setAttribute('data-capture-target', '1');
    }, name);
    await clickAt(page, '[data-capture-target="1"]', {settle: 560});
    await page.evaluate(() => document.querySelectorAll('[data-capture-target]')
      .forEach(e => e.removeAttribute('data-capture-target')));
    mark(label);
  };

  await record(page, {dir: `${ROOT}/frames/agents`, fps: 12, action: async (mark) => {
    await sleep(1400);            // Claude's renderer
    mark('claude');
    await sleep(2200);
    await toPill('codex-api', mark, 'codex');   // Codex's renderer
    await sleep(4200);
    await toPill('claude-api', mark, 'back');
    await sleep(2400);
    mark('end');
  }});
  await page.screenshot({path: `${ROOT}/shots/agents-final.png`});
  await browser.close();
})().catch(e => {console.error('ERR', e.message); process.exit(1);});
