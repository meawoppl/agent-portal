#!/usr/bin/env python3
"""Exercise real-WASM secret controls with mocked HTTP/OAuth; never reads a real secret.
Build frontend first; run with a Python environment containing Playwright/Chromium.
"""
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
USER = '00000000-0000-4000-8000-000000000001'
SESSION = '00000000-0000-4000-8000-000000000002'
REQUEST = '00000000-0000-4000-8000-000000000003'
SECRET = '00000000-0000-4000-8000-000000000004'

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path in ['/dashboard', '/secrets/authorize', '/secrets/verified']:
            self.path = '/index.html'
        try:
            super().do_GET()
        except (BrokenPipeError, ConnectionResetError):
            pass  # The browser can close while background requests are pending.
    def log_message(self, *_):
        pass

async def exercise(browser, base, output, width, grouped=False):
    context = await browser.new_context(viewport=dict(width=width, height=950), service_workers="block")
    page = await context.new_page()
    state = dict(verified=False, request='pending', return_to='', starts=0, approvals=0)
    errors, navigations = [], []
    page.on('pageerror', lambda e: errors.append(str(e)))
    page.on('request', lambda r: navigations.append(r.url) if r.is_navigation_request() and r.frame == page.main_frame else None)
    session = dict(id=SESSION, user_id=USER, session_key=SESSION, session_name='Secret controls',
                   working_directory='/fixture', status='active', paused=False, hostname='fixture',
                   agent_type='claude', my_role='owner', last_activity='2026-11-12T00:00:00',
                   created_at='2026-11-12T00:00:00', updated_at='2026-11-12T00:00:00')
    async def api(route):
        path = urlparse(route.request.url).path
        if path == '/api/config': data = dict(app_title='Portal', server_version='fixture', protocol_version=1)
        elif path == '/api/auth/me': data = dict(id=USER, email='fixture@example.invalid', name='Fixture', is_admin=False)
        elif path == '/api/sessions': data = dict(sessions=[session])
        elif path == '/api/portal-connections': data = dict(connections=[])
        elif path == '/api/launchers': data = []
        elif path.endswith('/messages'):
            content = dict(type='portal', content=[dict(type='secret_access_group' if grouped else 'secret_access_request', request_id=REQUEST)])
            data = dict(total=1, messages=[dict(role='portal', content=json.dumps(content), created_at='2026-11-12T00:00:00Z')])
        elif path == '/api/secrets/verification': data = dict(verified=state['verified'], providers=['google'])
        elif path == '/api/secrets/verification/start':
            body = json.loads(route.request.post_data)
            assert body['provider'] == 'google'
            assert body['return_to'].startswith('/secrets/verified?channel=portal-secret-verification-')
            assert route.request.headers['x-portal-secrets'] == '1'
            state['starts'] += 1
            state['return_to'] = body['return_to']
            data = dict(url=base + '/mock-provider')
        elif path.endswith('/decision') or path == '/api/secrets/authorize/approve':
            state['approvals'] += 1
            state['request'] = 'approved'
            data = {}
        elif path in [f'/api/secrets/requests/{REQUEST}', f'/api/secrets/request-groups/{REQUEST}']:
            data = dict(id=REQUEST, secret_id=SECRET, owner_user_id=USER, namespace='personal',
                        name='portal-test-canary', version=1, session_id=SESSION, launcher_id=SECRET,
                        env_name='CANARY', expires_in_seconds=900, state=state['request'],
                        created_at='2026-11-12T00:00:00Z', expires_at='2026-11-12T02:00:00Z',
                        binding_id=SECRET if state['request'] == 'approved' else None)
            if grouped:
                second = dict(data, id='00000000-0000-4000-8000-000000000005', name='second-credential', env_name='SECOND')
                data = dict(id=REQUEST, state=state['request'], items=[data, second])
        else: data = {}
        await route.fulfill(content_type='application/json', body=json.dumps(data))
    async def provider(route):
        path = urlparse(route.request.url).path
        if path == '/mock-provider':
            await route.fulfill(content_type='text/html', body='<button onclick="location.href=\'/mock-provider/success\'">Continue</button><button onclick="location.href=\'/mock-provider/cancel\'">Cancel</button>')
        else:
            state['verified'] = path.endswith('/success')
            suffix = '' if state['verified'] else '&secret_verification=failed'
            await route.fulfill(status=302, headers={'location': base + state['return_to'] + suffix})
    async def ws(route):
        def message(raw):
            if json.loads(raw).get('type') == 'Hello':
                route.send(json.dumps(dict(type='HelloAck', protocol_version=1, success=True)))
        route.on_message(message)
    await context.route('**/api/**', api)
    await context.route(lambda url: '/mock-provider' in url, provider)
    await context.route_web_socket('**/ws/**', ws)
    state['verified'] = True
    # Transcript card: current proof enables approval; only the card updates.
    await page.goto(base + f'/dashboard?session={SESSION}')
    card = page.locator('.secret-access-request')
    approve_label = 'Approve all credentials' if grouped else 'Approve access'
    if grouped:
        await expect(card.locator('.secret-access-group > ul > li')).to_have_count(2)
        await expect(card.locator('.secret-access-group > ul')).to_contain_text('second-credential')
    await expect(card.get_by_role('button', name=approve_label, exact=True)).to_be_enabled(timeout=30000)
    await card.screenshot(path=str(output / f'secret-controls-{grouped}-pending-{width}.png'))
    count = len(navigations)
    await card.get_by_role('button', name=approve_label, exact=True).click()
    await expect(card.locator('.secret-access-group > p') if grouped else card.locator('.secret-access-status')).to_contain_text('Approved')
    await expect(card.locator('.secrets-verification')).to_have_count(0)
    await expect(card.get_by_role('button', name=approve_label)).to_have_count(0)
    await expect(card.locator('details').first).not_to_have_attribute('open', '')
    refresh = card.get_by_role('button', name='Refresh status')
    await expect(refresh).to_be_enabled()
    assert (await refresh.bounding_box())['height'] >= 44
    await page.keyboard.press("Tab")
    await refresh.focus()
    assert await refresh.evaluate("e => getComputedStyle(e).outlineStyle") != 'none'
    await refresh.click()
    await expect(card.locator('.secret-access-group > p') if grouped else card.locator('.secret-access-status')).to_contain_text('Approved')
    assert len(navigations) == count and state['approvals'] == 1
    assert await card.evaluate('e => e.scrollWidth <= e.clientWidth + 1')
    await card.screenshot(path=str(output / f'secret-controls-{grouped}-approved-{width}.png'))
    assert not errors, errors
    await context.close()

async def main(args):
    args.output.mkdir(parents=True, exist_ok=True)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(args.dist)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch()
            for width in [1100, 390]:
                await exercise(browser, f'http://127.0.0.1:{server.server_port}', args.output, width, args.groups)
            await browser.close()
    finally: server.shutdown()
    print('PASS: desktop/mobile card approval, refresh without reload, touch targets, keyboard focus')

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--groups', action='store_true')
    parser.add_argument('--dist', type=Path, default=ROOT / 'frontend/dist')
    parser.add_argument('--output', type=Path, default=ROOT / 'reports/secret-controls')
    asyncio.run(main(parser.parse_args()))
