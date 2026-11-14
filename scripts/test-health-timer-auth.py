#!/usr/bin/env python3
"""Real WASM regression: logged-out time must not produce health reminders."""
import asyncio
import functools
import http.server
import json
import threading
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlparse
from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
USER = '11111111-1111-4111-8111-111111111111'
KEY = 'agent_portal.health_timer.v1'
RESET = 'agent_portal.health_timer.reset_after_login'

class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_): pass

async def main():
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(ROOT/'frontend/dist')))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch()
            context = await browser.new_context(service_workers='block')
            page = await context.new_page()
            await page.clock.install(time=datetime(2026, 11, 14, 12, tzinfo=timezone.utc))
            await page.add_init_script('''(() => {
                if (!localStorage.getItem('agent_portal.health_timer.v1')) localStorage.setItem('agent_portal.health_timer.v1', JSON.stringify({enabled:true,cadence_minutes:1,message:'Take a break',last_confirmed_at:'2026-11-13T00:00:00.000Z'}));
            })();''')
            logged_out, unavailable = True, False
            auth_checks = 0
            hold_check = False
            check_started, release_check = asyncio.Event(), asyncio.Event()
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            async def api(route):
                nonlocal auth_checks
                path = urlparse(route.request.url).path
                if path == '/api/auth/me':
                    auth_checks += 1
                    if logged_out or unavailable:
                        await route.fulfill(status=401 if logged_out else 503, body='Unavailable')
                        return
                    data = dict(id=USER,email='fixture@example.invalid',name='Fixture',is_admin=False)
                    if hold_check:
                        check_started.set()
                        await release_check.wait()
                elif path == '/api/config': data = dict(app_title='Portal', server_version='fixture', protocol_version=1, auth_providers=['google'])
                elif path == '/api/portal-connections': data = dict(connections=[])
                else: data = {}
                await route.fulfill(content_type='application/json', body=json.dumps(data))
            await page.route('**/api/**', api)
            dialog = page.get_by_role('dialog', name='Health timer', exact=True)
            async def timestamp():
                return await page.evaluate('(key) => JSON.parse(localStorage.getItem(key)).last_confirmed_at', KEY)
            await page.goto(f'http://127.0.0.1:{server.server_port}/')
            await expect(page.get_by_role('button', name='Sign in with Google')).to_be_visible()
            await page.wait_for_function('(key) => localStorage.getItem(key) === "1"', arg=RESET)
            await page.clock.fast_forward(600_000)
            await expect(dialog).to_have_count(0)
            assert auth_checks == 1, 'no reminder auth checks while logged out'
            # A fresh login gets a full interval despite the long anonymous gap.
            logged_out = False
            await page.reload()
            await page.wait_for_function('(key) => localStorage.getItem(key) === null', arg=RESET)
            start = await timestamp()
            await page.clock.fast_forward(59_000)
            await expect(dialog).to_have_count(0)
            await page.clock.fast_forward(1_100)
            await expect(dialog).to_be_visible()
            assert auth_checks >= 3, 'due reminders must recheck authentication'
            await dialog.get_by_role('button', name='Confirm', exact=True).click()
            await expect(dialog).to_have_count(0)
            assert await timestamp() != start
            # Full page refresh while signed in preserves the remaining time.
            await page.clock.fast_forward(30_000)
            confirmed = await timestamp()
            await page.reload()
            await expect(page.get_by_role('button', name='Sign in with Google')).to_be_visible()
            assert await timestamp() == confirmed
            await page.clock.fast_forward(29_000)
            await expect(dialog).to_have_count(0)
            await page.clock.fast_forward(1_100)
            await expect(dialog).to_be_visible()
            await dialog.get_by_role('button', name='Confirm', exact=True).click()
            # Cookie expiry on a mounted, previously authenticated page suppresses delivery.
            logged_out = True
            await page.clock.fast_forward(60_000)
            await page.wait_for_function('(key) => localStorage.getItem(key) === "1"', arg=RESET)
            await expect(dialog).to_have_count(0)
            checked = auth_checks
            await page.clock.fast_forward(600_000)
            assert auth_checks == checked, 'expired sessions must stop the timer'
            # Network failure is not logout: do not show or reset; retry later.
            logged_out = False
            await page.reload()
            await page.wait_for_function('(key) => localStorage.getItem(key) === null', arg=RESET)
            start = await timestamp()
            unavailable = True
            async with page.expect_response(lambda response: response.url.endswith('/api/auth/me') and response.status == 503):
                await page.clock.fast_forward(60_000)
            await page.clock.run_for(100)
            await expect(dialog).to_have_count(0)
            assert await timestamp() == start
            unavailable = False
            await page.clock.fast_forward(30_000)
            await expect(dialog).to_be_visible()
            # Cross-tab logout/reset dismisses a reminder that is already open.
            await page.evaluate('''([key, reset]) => {
                localStorage.setItem(reset, '1');
                window.dispatchEvent(new StorageEvent('storage', {key}));
            }''', [KEY, RESET])
            await expect(dialog).to_have_count(0)
            # A successful response started before logout must not open a late modal.
            await page.reload()
            await page.wait_for_function('(key) => localStorage.getItem(key) === null', arg=RESET)
            hold_check = True
            await page.clock.fast_forward(60_000)
            await asyncio.wait_for(check_started.wait(), 5)
            await page.evaluate("""([key, reset]) => {
                localStorage.setItem(reset, '1');
                window.dispatchEvent(new StorageEvent('storage', {key}));
            }""", [KEY, RESET])
            await page.clock.run_for(100)
            release_check.set()
            await page.clock.run_for(100)
            await expect(dialog).to_have_count(0)
            assert not errors, errors
            await browser.close()
    finally:
        server.shutdown()
    print('PASS: anonymous suppression, login reset, signed-in refresh, confirmation, cookie expiry, network retry, cross-tab logout')

if __name__ == '__main__': asyncio.run(main())
