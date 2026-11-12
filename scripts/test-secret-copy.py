#!/usr/bin/env python3
"""Real WASM clipboard tests with local canaries and mocked secrets API."""
import argparse
import asyncio
import functools
import http.server
import json
import threading
from pathlib import Path
from urllib.parse import urlparse
from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
USER = '11111111-1111-4111-8111-111111111111'
SECRET = '00000000-0000-4000-8000-000000000001'
CANARY = 'synthetic-clipboard-only\nsecond line'

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path == '/settings': self.path = '/index.html'
        super().do_GET()
    def log_message(self, *_): pass

async def exercise(browser, base, output, width):
    context = await browser.new_context(viewport=dict(width=width, height=950), permissions=['clipboard-read', 'clipboard-write'], service_workers='block')
    page = await context.new_page()
    copies, errors = [], []
    release = asyncio.Event()
    page.on('pageerror', lambda e: errors.append(str(e)))
    async def api(route):
        path = urlparse(route.request.url).path
        if path == '/api/config': data = dict(app_title='Portal', server_version='fixture', protocol_version=1)
        elif path == '/api/auth/me': data = dict(id=USER, email='fixture@example.invalid', name='Fixture', is_admin=True)
        elif path == '/api/portal-connections': data = dict(connections=[])
        elif path == '/api/secrets/verification': data = dict(verified=False, providers=['google'])
        elif path == '/api/secrets/verification/check': data = dict(verified=True, providers=['google'])
        elif path == '/api/secrets':
            data = dict(secrets=[dict(id=SECRET,owner_user_id=USER,namespace='personal',name='DEPLOY_TOKEN',description='Deployment credential',tags=[],current_version=3,generation=7,created_at='2026-11-12T00:00:00Z',updated_at='2026-11-12T00:00:00Z',deleted_at=None,deleted_by=None,can_read=True,can_use=True,can_write=True,can_manage=True,grants=[])],can_manage_organization=True)
        elif path == '/api/secrets/members': data = dict(members=[dict(user_id=USER,display_name='Fixture',email='fixture@example.invalid',identities=[])])
        elif path.endswith('/versions'): data = dict(versions=[dict(version=3,created_at='2026-11-12T00:00:00Z',created_by=USER,current=True)])
        elif path.endswith('/copy'):
            copies.append(route.request)
            assert route.request.method == 'POST'
            assert route.request.headers['x-portal-secrets'] == '1'
            assert json.loads(route.request.post_data)['version'] == 3
            await release.wait()
            if len(copies) == 2:
                await route.fulfill(status=403, body='SENSITIVE_ERROR_MUST_NOT_RENDER')
                return
            data = dict(value=CANARY, version=3)
        else: data = {}
        await route.fulfill(content_type='application/json', body=json.dumps(data))
    await context.route('**/api/**', api)
    await page.goto(base + '/settings?tab=secrets')
    await expect(page.get_by_role('button', name='DEPLOY_TOKEN', exact=True)).to_have_count(0)
    await page.get_by_role('button', name='Check Portal access', exact=True).click()
    await page.get_by_role('button', name='DEPLOY_TOKEN', exact=True).click()
    copy = page.get_by_role('button', name='Copy secret', exact=True)
    assert not copies
    await expect(copy).to_be_enabled()
    await copy.click()
    await expect(page.get_by_role('button', name='Copying…', exact=True)).to_be_disabled()
    await expect(page.get_by_text('Copying to clipboard…', exact=True)).to_be_visible()
    release.set()
    await expect(page.get_by_text('Copied to clipboard', exact=True)).to_be_visible()
    assert await page.evaluate('navigator.clipboard.readText()') == CANARY
    assert CANARY not in await page.content()
    assert CANARY not in await page.evaluate('JSON.stringify([localStorage, sessionStorage])')
    await page.screenshot(path=str(output / f'secret-copy-{width}.png'))
    await copy.click()
    await expect(page.get_by_role('alert')).to_contain_text('Could not copy.')
    assert 'SENSITIVE_ERROR_MUST_NOT_RENDER' not in await page.content()
    await expect(page.get_by_text('Copied to clipboard', exact=True)).to_have_count(0)
    # Browser denial must not claim success or reveal the fetched value.
    await page.evaluate("() => { navigator.clipboard.write = () => Promise.reject(new DOMException('Denied', 'NotAllowedError')); }")
    await copy.click()
    await expect(page.get_by_role('alert')).to_contain_text('Could not copy.')
    assert CANARY not in await page.content()
    assert len(copies) == 3 and not errors, errors
    assert len(context.pages) == 1
    await context.close()

async def main(args):
    args.output.mkdir(parents=True, exist_ok=True)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(args.dist)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch()
            for width in [1100, 390]: await exercise(browser, f'http://127.0.0.1:{server.server_port}', args.output, width)
            await browser.close()
    finally: server.shutdown()
    print('PASS: verification gate, delayed clipboard write, exact value, denied fetch/write, retry, no DOM disclosure')

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dist', type=Path, default=ROOT / 'frontend/dist')
    parser.add_argument('--output', type=Path, default=ROOT / 'reports/secret-copy')
    asyncio.run(main(parser.parse_args()))
