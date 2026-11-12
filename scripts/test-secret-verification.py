#!/usr/bin/env python3
"""Real-WASM inline verification tests; all HTTP responses are local fixtures."""
import argparse
import asyncio
import functools
import http.server
import json
import runpy
import threading
from pathlib import Path
from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
fixture = runpy.run_path(str(ROOT / 'scripts/test-secret-controls.py'))

async def exercise(browser, base, output, width):
    context = await browser.new_context(viewport=dict(width=width, height=950), service_workers='block')
    page = await context.new_page()
    release = asyncio.Event()
    checks = 0
    navigations = []
    page.on('request', lambda r: navigations.append(r.url) if r.is_navigation_request() else None)
    async def api(route):
        nonlocal checks
        path = route.request.url.split(base)[-1]
        if path == '/api/config': data = dict(app_title='Portal', server_version='fixture', protocol_version=1)
        elif path == '/api/auth/me': data = dict(id=fixture['USER'], email='fixture@example.invalid', name='Fixture', is_admin=False)
        elif path == '/api/secrets/verification': data = dict(verified=False, providers=['google'])
        elif path == '/api/secrets/verification/check':
            assert route.request.method == 'POST'
            assert route.request.headers['x-portal-secrets'] == '1'
            checks += 1
            await release.wait()
            release.clear()
            if checks == 1:
                await route.fulfill(status=503, body='Unavailable')
                return
            data = dict(verified=True, providers=['google'])
        elif path.startswith('/api/secrets/authorize/'):
            raise AssertionError('Verification must not automatically approve or deny access')
        else: data = {}
        await route.fulfill(content_type='application/json', body=json.dumps(data))
    await context.route('**/api/**', api)
    await page.goto(base + '/secrets/authorize?code=ORIGINAL')
    await page.get_by_label('Code shown by your CLI').fill('KEEP-MY-DRAFT')
    await page.get_by_role('checkbox').check()
    initial = len(navigations)
    approve = page.get_by_role('button', name='Approve CLI', exact=True)
    await expect(approve).to_be_disabled()
    await page.get_by_role('button', name='Verify Portal session', exact=True).click()
    await expect(page.get_by_role('status')).to_have_text('Checking your Portal session…')
    await expect(page.locator('.secret-verification-spinner')).to_be_visible()
    assert len(context.pages) == 1
    release.set()
    await expect(page.get_by_role('alert')).to_be_visible()
    await expect(approve).to_be_disabled()
    await page.get_by_role('button', name='Try again', exact=True).click()
    await expect(page.get_by_role('status')).to_have_text('Checking your Portal session…')
    release.set()
    await expect(page.get_by_role('status')).to_have_text('Portal session verified')
    await expect(approve).to_be_enabled()
    await expect(page.get_by_label('Code shown by your CLI')).to_have_value('KEEP-MY-DRAFT')
    await expect(page.get_by_role('checkbox')).to_be_checked()
    await page.evaluate("window.dispatchEvent(new Event('focus'))")
    await expect(page.get_by_role('button', name='Verify Portal session', exact=True)).to_have_count(0)
    assert len(navigations) == initial and len(context.pages) == 1 and checks == 2
    await page.screenshot(path=str(output / f'inline-verification-{width}.png'))
    await context.close()

async def main(args):
    args.output.mkdir(parents=True, exist_ok=True)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(fixture['Handler'], directory=str(args.dist)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch()
            for width in [1100, 390]:
                await exercise(browser, f'http://127.0.0.1:{server.server_port}', args.output, width)
            await browser.close()
    finally: server.shutdown()
    print('PASS: inline spinner, failure/retry, stable success, no navigation/popup/automatic approval, preserved draft')

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dist', type=Path, default=ROOT / 'frontend/dist')
    parser.add_argument('--output', type=Path, default=ROOT / 'reports/secret-verification')
    asyncio.run(main(parser.parse_args()))
