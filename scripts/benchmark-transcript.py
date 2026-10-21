#!/usr/bin/env python3
"""Deterministic browser hydration/replay benchmark; no live backend or model calls.
Build frontend first. Use --dist to measure a specific (preferably release) bundle.
"""
import argparse
import asyncio
import functools
import http.server
import json
from pathlib import Path
import statistics
import threading
from urllib.parse import urlparse
from uuid import UUID, uuid5
from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
USER = '00000000-0000-4000-8000-000000000001'
SESSION = '00000000-0000-4000-8000-000000000002'

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path == '/dashboard':
            self.path = '/index.html'
        super().do_GET()
    def log_message(self, *_):
        pass

def fixture(markers, prefix):
    """100 counted rows, with a large free-thinking burst inside the retained tail."""
    rows = []
    for index in range(100):
        if index == 50:
            rows.extend(dict(type='system', subtype='thinking_tokens', session_id=SESSION,
                             uuid=str(uuid5(UUID(SESSION), f'{prefix}-thinking-{n}')),
                             estimated_tokens=n + 1, estimated_tokens_delta=1)
                        for n in range(markers))
        role = 'user' if index % 2 == 0 else 'assistant'
        message = dict(role=role, content=[dict(
            type='text', text=f'{prefix} item {index:03d}. ' + 'Deterministic transcript content. ' * 32)])
        if role == 'assistant':
            message.update(id=f'msg_fixture_{prefix}_{index}', model='claude-sonnet-4-5',
                           stop_reason='end_turn')
        rows.append(dict(type=role, session_id=SESSION,
                         uuid=str(uuid5(UUID(SESSION), f'{prefix}-message-{index}')),
                         message=message))
    return rows

async def assert_typed_window(page, prefix, markers):
    """A visible sentinel alone also passes inside the raw-JSON fallback."""
    await expect(page.locator('.raw-json')).to_have_count(0)
    await expect(page.locator('.user-text')).to_have_count(50)
    await expect(page.locator('.assistant-text')).to_have_count(50)
    # All counted rows survive, rather than just the newest sentinel.
    for index in range(100):
        kind = '.user-text' if index % 2 == 0 else '.assistant-text'
        await expect(page.locator(kind).filter(has_text=f'{prefix} item {index:03d}.')).to_have_count(1)
    await expect(page.locator('.thinking-pulse-group')).to_have_count(int(markers > 0))
    if markers:
        # The title is the typed estimate, independent of the count-up animation.
        await expect(page.locator('.thinking-pulse-group .message-count')).to_have_attribute(
            'title', f'~{markers} thinking tokens')


async def main(args):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(args.dist)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    results = []
    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch()
            browser_version = browser.version
            for markers in args.markers:
                for iteration in range(args.repeat):
                    page = await browser.new_page(viewport=dict(width=args.width, height=1000))
                    errors, session_sockets = [], []
                    registered = asyncio.Event()
                    page.on('pageerror', lambda error: errors.append(str(error)))
                    await page.add_init_script("""window.benchmarkStart = (name, sentinel) => {
                        const start = performance.now();
                        window[name] = new Promise(resolve => {
                            const observer = new MutationObserver(() => {
                                if (!document.body?.textContent.includes(sentinel)) return;
                                observer.disconnect();
                                requestAnimationFrame(() => requestAnimationFrame(() => resolve(performance.now() - start)));
                            });
                            observer.observe(document, {subtree:true, childList:true, characterData:true});
                        });
                    };""")
                    rows = fixture(markers, 'Hydration')
                    session = dict(id=SESSION, user_id=USER, session_key=SESSION,
                                   session_name='Benchmark', working_directory='/fixture', status='active',
                                   paused=False, hostname='fixture', agent_type='claude', my_role='owner',
                                   last_activity='2026-10-21T00:00:00', created_at='2026-10-21T00:00:00', updated_at='2026-10-21T00:00:00')
                    async def api(route):
                        path = urlparse(route.request.url).path
                        if path.endswith('/messages'):
                            assert 'render_limit=100' in route.request.url
                            data = dict(total=100000, messages=[dict(role='assistant', content=json.dumps(row), created_at='2026-10-21T00:00:00Z') for row in rows])
                            await page.evaluate("benchmarkStart('hydrationMeasurement', 'Hydration item 099.')")
                        elif path == '/api/auth/me': data = dict(id=USER, email='benchmark@example.invalid', name='Fixture', is_admin=False)
                        elif path == '/api/sessions': data = dict(sessions=[session])
                        elif path == '/api/portal-connections': data = dict(connections=[])
                        elif path == '/api/launchers': data = []
                        else: data = {}
                        await route.fulfill(content_type='application/json', body=json.dumps(data))
                    async def ws(route):
                        def message(raw):
                            if json.loads(raw).get('type') == 'Register':
                                session_sockets.append(route)
                                registered.set()
                        route.on_message(message)
                    await page.route('**/api/**', api)
                    await page.route_web_socket('**/ws/**', ws)
                    await page.goto(f'http://127.0.0.1:{server.server_port}/dashboard?session={SESSION}')
                    await expect(page.get_by_text('Hydration item 099.', exact=False)).to_be_visible(timeout=60000)
                    hydration = await page.evaluate("window.hydrationMeasurement")
                    await assert_typed_window(page, 'Hydration', markers)
                    await asyncio.wait_for(registered.wait(), timeout=15)
                    replay = fixture(markers, 'Replay')
                    await page.evaluate("benchmarkStart('replayMeasurement', 'Replay item 099.')")
                    session_sockets[-1].send(json.dumps(dict(type='HistoryBatch', entries=[dict(content=row) for row in replay], last_created_at='2026-10-21T00:00:01Z')))
                    await expect(page.get_by_text('Replay item 099.', exact=False)).to_be_visible(timeout=60000)
                    replay_ms = await page.evaluate("window.replayMeasurement")
                    await assert_typed_window(page, 'Replay', markers)
                    await expect(page.locator('.user-text, .assistant-text').filter(has_text='Hydration item')).to_have_count(0)
                    assert not errors, errors
                    results.append(dict(markers=markers, iteration=iteration, hydration_wall_ms=round(hydration, 1), replay_wall_ms=round(replay_ms, 1), dom_nodes=await page.locator('*').count()))
                    await page.close()
            await browser.close()
    finally:
        server.shutdown()
    report = dict(label=args.label, width=args.width, height=1000, browser_version=browser_version,
                  fixture_version=2, dist=str(args.dist.resolve()), samples=results,
                  medians=[dict(markers=n, hydration_wall_ms=statistics.median(r['hydration_wall_ms'] for r in results if r['markers']==n), replay_wall_ms=statistics.median(r['replay_wall_ms'] for r in results if r['markers']==n)) for n in args.markers])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps(report, indent=2))

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dist', type=Path, default=ROOT/'frontend/dist')
    parser.add_argument('--label', default='unspecified build; do not compare timings across profiles')
    parser.add_argument('--repeat', type=int, default=3)
    parser.add_argument('--width', type=int, default=1440)
    parser.add_argument('--markers', nargs='+', type=int, default=[0,5000])
    parser.add_argument('--output', type=Path, default=ROOT/'builds/transcript-benchmark.json')
    asyncio.run(main(parser.parse_args()))
