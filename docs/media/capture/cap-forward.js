const {spawn, execSync} = require('child_process');
const {openPortal, record, sleep, stage, clickAt, cursorTo, waitFor} = require('./lib.js');
const ROOT = process.env.DEMO_ROOT || '/tmp/readme-demo';
const BASE_HTTP = process.env.DEMO_URL || 'http://localhost:3100';

const SID = process.argv[2];
const PORT = 8899;
let server = null;

const startServer = () => {
  server = spawn('python3', ['-m', 'http.server', String(PORT), '--bind', '127.0.0.1'],
                 {cwd: `${ROOT}/site`, stdio: 'ignore'});
};
const stopServer = () => { if (server) { server.kill('SIGINT'); server = null; } };

(async () => {
  // clean slate: no forward, no server
  execSync(`curl -s -X DELETE ${BASE_HTTP}/api/sessions/${SID}/forwards`);
  const {browser, page} = await openPortal({width: 1180, height: 820, scale: 2});
  await sleep(2500);
  await page.evaluate(() => {
    const pill = document.querySelector('.session-pill, [class*="session-pill"], .session-rail button');
    if (pill) pill.click();
  });
  await sleep(2500);
  await stage(page);
  await sleep(600);

  await record(page, {dir: `${ROOT}/frames/forward`, fps: 12, action: async (mark) => {
    await sleep(900);
    // the agent asks for a door
    await page.evaluate(async (sid) => {
      await fetch(`/api/sessions/${sid}/forwards`, {
        method: 'POST', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({port: 8899}),
      });
      await fetch(`/api/sessions/${sid}/forwards/public`, {
        method: 'PATCH', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({public: true}),
      });
    }, SID);
    await waitFor(page, () => !!document.querySelector('.forward-chip'));
    mark('chip');
    await sleep(1800);
    await cursorTo(page, '.forward-chip', {settle: 500});   // hover it while red
    await sleep(400);
    mark('red_hover');

    startServer();
    await waitFor(page, () => !!document.querySelector('.forward-chip.is-up'), {timeout: 30000});
    mark('green');
    await sleep(2000);

    await clickAt(page, '.forward-chip a', {settle: 600});  // open the preview
    await waitFor(page, () => !!document.querySelector('.forward-preview-bar'), {timeout: 15000});
    mark('preview');
    await sleep(4000);
    mark('end');
  }});
  await page.screenshot({path: `${ROOT}/shots/forward-final.png`});
  stopServer();
  await browser.close();
})().catch(e => {console.error('ERR', e); stopServer(); process.exit(1);});
