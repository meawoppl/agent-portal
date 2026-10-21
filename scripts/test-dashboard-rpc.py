#!/usr/bin/env python3
"""Browser regression: Computers uses one correlated socket, never launcher REST.
Run after trunk build: uv run --with playwright python scripts/test-dashboard-rpc.py
"""
import asyncio
import functools
import http.server
import json
from pathlib import Path
import threading
from urllib.parse import urlparse
from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
USER = '00000000-0000-4000-8000-000000000001'
HOST = '00000000-0000-4000-8000-000000000002'

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path in ('/settings', '/dashboard'):
            self.path = '/index.html'
        super().do_GET()
    def log_message(self, *_):
        pass

async def main():
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(ROOT / 'frontend/dist')))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f'http://127.0.0.1:{server.server_port}'
    operations, forbidden, sockets, errors = [], [], [], []
    host = dict(launcher_id=HOST, launcher_name='Test host', hostname='rpc-host', connected=True,
                running_sessions=0, active_sessions=0, version='99.0.0', capabilities=['launcher.restart'])

    async def api(route):
        path = urlparse(route.request.url).path
        if path.startswith('/api/launchers') or path == '/api/launch':
            forbidden.append(path)
            await route.fulfill(status=500, body='Launcher REST must not be used')
            return
        data = {}
        if path == '/api/auth/me':
            data = dict(id=USER, email='test@example.invalid', name='Test', is_admin=False)
        elif path == '/api/portal-connections':
            data = dict(connections=[])
        elif path == '/api/sessions':
            data = dict(sessions=[])
        await route.fulfill(content_type='application/json', body=json.dumps(data))

    async def ws(route):
        sockets.append(route)
        def message(raw):
            msg = json.loads(raw)
            assert msg['type'] == 'Dashboard', msg
            op = msg['request']['operation']
            operations.append(op)
            if op == 'list_launchers':
                data = dict(operation='launchers', data=[host])
            elif op == 'probe_agents':
                data = dict(operation='agents', data=dict(agents=[dict(agent_type='claude', installed=True, version='test'), dict(agent_type='codex', installed=True, version='test')]))
            elif op == 'restart_launcher':
                route.send(json.dumps(dict(type='DashboardResult', request_id=msg['request_id'], result=dict(Err=dict(status=403, message='Fixture denied restart')))))
                return
            else:
                raise AssertionError(op)
            route.send(json.dumps(dict(type='DashboardResult', request_id=msg['request_id'], result=dict(Ok=data))))
        route.on_message(message)
        route.send(json.dumps(dict(type='LaunchersChanged')))

    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch()
            page = await browser.new_page(viewport=dict(width=1440, height=1000))
            page.on('pageerror', lambda e: errors.append(str(e)))
            await page.route('**/api/**', api)
            await page.route_web_socket('**/ws/client**', ws)
            await page.goto(base + '/settings')
            await page.get_by_role('button', name='Computers', exact=True).click()
            await expect(page.get_by_text('rpc-host', exact=True)).to_be_visible()
            assert 'list_launchers' in operations and 'probe_agents' in operations, operations
            assert len(sockets) == 1, len(sockets)
            await page.get_by_role('button', name='Restart', exact=True).click()
            await page.get_by_role('button', name='Confirm Restart', exact=True).click()
            await expect(page.get_by_text('Fixture denied restart', exact=False)).to_be_visible()
            assert operations.count('restart_launcher') == 1
            assert not forbidden, forbidden
            assert not errors, errors
            (ROOT / 'builds').mkdir(exist_ok=True)
            await page.screenshot(path=str(ROOT / 'builds/dashboard-rpc.png'))
            await browser.close()
            print('PASS: one socket, launcher list/probe/restart, error detail, no launcher REST')
    finally:
        server.shutdown()

if __name__ == '__main__':
    asyncio.run(main())
