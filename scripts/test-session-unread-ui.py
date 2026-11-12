#!/usr/bin/env python3
"""Verify desktop unread tabs against saved receipts using the built dashboard.

Run after trunk build with Python Playwright and Chromium/WebKit installed.
All API and websocket traffic is intercepted; no real sessions are changed.
"""
import argparse
import asyncio
import functools
import http.server
import json
import re
import threading
from pathlib import Path
from urllib.parse import urlparse

from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
USER, A, B, C, READ_REPLY, UNREAD_REPLY, NEW_REPLY = [
    f'00000000-0000-4000-8000-{i:012d}' for i in range(1, 8)
]
AT = '2026-10-01T12:00:00.000001'
AWAITING = re.compile(r'\bawaiting\b')


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path == '/dashboard':
            self.path = '/index.html'
        super().do_GET()

    def log_message(self, *_):
        pass


async def exercise(browser, base, name):
    context = await browser.new_context(viewport=dict(width=1440, height=900), service_workers='block')
    page = await context.new_page()
    errors, updates = [], []
    fail_feed = False
    page.on('pageerror', lambda error: errors.append(str(error)))
    sessions = [dict(id=id, user_id=USER, session_key=id, session_name=label,
                     working_directory='/work/project', status='active', paused=False,
                     hostname='test-host', agent_type='codex', my_role='owner',
                     last_activity=AT, created_at=AT, updated_at=AT)
                for id, label in [(A, 'Current work'), (B, 'Old read session'), (C, 'Unread session')]]
    rows = {id: dict(session_id=id, reply_id=None, reply_at=None, preview='', unread=False,
                    needs_approval=False, working=False, connected=True, muted=False)
            for id in (A, B, C)}
    rows[B].update(reply_id=READ_REPLY, reply_at=AT+'Z', preview='Already read')
    rows[C].update(reply_id=UNREAD_REPLY, reply_at=AT+'Z', preview='New response', unread=True)

    async def api(route):
        path = urlparse(route.request.url).path
        if path == '/api/session-attention':
            if fail_feed:
                await route.fulfill(status=503, body='Temporary failure')
                return
            data = dict(sessions=list(rows.values()))
        elif path.endswith('/attention'):
            id = path.split('/')[3]
            req = route.request.post_data_json
            updates.append((id, req))
            if req.get('seen_reply_id') and req['seen_reply_id'] == rows[id]['reply_id']:
                rows[id]['unread'] = False
            await route.fulfill(status=204)
            return
        elif path == '/api/sessions':
            data = dict(sessions=sessions)
        elif path == '/api/config':
            data = dict(app_title='Agent Portal', server_version='2.15.1822', protocol_version=1)
        elif path == '/api/auth/me':
            data = dict(id=USER, email='test@example.invalid', name='Test', is_admin=False)
        elif path == '/api/portal-connections':
            data = dict(connections=[])
        elif path == '/api/agent/sessions':
            data = dict(sessions=[])
        elif path.endswith('/messages'):
            id = path.split('/')[3]
            at = rows[id]['reply_at']
            messages = [] if not at else [dict(role='unknown', created_at=at,
                content=json.dumps(dict(type='turn.completed', usage=dict(input_tokens=1, output_tokens=1, cached_input_tokens=0))),
                meta=dict(created_at=at))]
            data = dict(messages=messages, total=len(messages))
        elif path.endswith('/turn-metrics') or path == '/api/metrics/recent':
            data = dict(metrics=[], latest_by_session=[])
        elif path.endswith('/edit-stack'):
            data = dict(items=[])
        elif path == '/api/launchers':
            data = []
        else:
            data = {}
        await route.fulfill(content_type='application/json', body=json.dumps(data))

    async def ws(route):
        def receive(raw):
            if json.loads(raw)['type'] == 'Hello':
                route.send(json.dumps(dict(type='HelloAck', protocol_version=1, success=True)))
        route.on_message(receive)

    await page.route('**/api/**', api)
    await page.route_web_socket('**/ws/**', ws)
    pill = lambda id: page.locator(f'.session-pill[data-session-key="0/{id}"]')
    await page.goto(f'{base}/dashboard?session={A}')
    await expect(page.locator('.session-pill')).to_have_count(3, timeout=60000)
    # Wait for the historical completion to render in the hidden view. That
    # hydration used to reconstruct an unread flag despite the saved receipt.
    await expect(page.locator(f'[data-rendered-reply="{READ_REPLY}"]')).to_have_count(1, timeout=15000)
    await expect(pill(B)).not_to_have_class(AWAITING)
    await expect(pill(C)).to_have_class(AWAITING)
    await expect(page).to_have_title('(1) Agent Portal')
    assert not any(req.get('seen_reply_id') for _, req in updates), 'background hydration must not read replies'

    # Read the actual unread response, move away, then reload twice.
    await pill(C).click()
    await expect(pill(C)).not_to_have_class(AWAITING, timeout=15000)
    assert any(id == C and req.get('seen_reply_id') == UNREAD_REPLY for id, req in updates)
    await pill(A).click()
    for _ in range(2):
        await page.reload()
        await expect(page.locator(f'[data-rendered-reply="{READ_REPLY}"]')).to_have_count(1, timeout=15000)
        await expect(page.locator(f'[data-rendered-reply="{UNREAD_REPLY}"]')).to_have_count(1, timeout=15000)
        await expect(page.locator('.session-pill.awaiting')).to_have_count(0)
        await expect(page).to_have_title('Agent Portal')

    # The feed can announce a new reply before its background transcript has
    # arrived. It must still light the tab, and the old reply must not ack it.
    rows[C].update(reply_id=NEW_REPLY, reply_at='2026-10-02T12:00:00Z', unread=True)
    await expect(pill(C)).to_have_class(AWAITING, timeout=12000)
    assert rows[C]['unread']
    # Pending approvals remain actionable after focusing their conversation.
    rows[B].update(needs_approval=True)
    await expect(pill(B)).to_have_class(AWAITING, timeout=12000)
    await pill(B).click()
    await expect(page).to_have_title('(2) Agent Portal', timeout=12000)
    await expect(pill(B)).to_have_class(AWAITING)
    rows[B].update(needs_approval=False)
    await expect(pill(B)).not_to_have_class(AWAITING, timeout=12000)

    # A cold load during a feed outage cannot invent unread state from history.
    fail_feed = True
    await page.reload()
    await expect(page.locator('.session-view-messages')).to_have_count(3, timeout=15000)
    await expect(page.locator('.session-pill.awaiting')).to_have_count(0)
    fail_feed = False
    await expect(pill(C)).to_have_class(AWAITING, timeout=12000)
    await expect(pill(B)).not_to_have_class(AWAITING)
    assert not errors, errors
    await context.close()
    print(f'PASS {name}: saved reads, repeated reload, new reply, pending approval, feed recovery')


async def main(selected):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(ROOT/'frontend/dist')))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        async with async_playwright() as p:
            for name, engine in [('chromium', p.chromium), ('webkit', p.webkit)]:
                if selected not in ('all', name):
                    continue
                browser = await engine.launch()
                await exercise(browser, f'http://127.0.0.1:{server.server_port}', name)
                await browser.close()
    finally:
        server.shutdown()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--browser', choices=['all', 'chromium', 'webkit'], default='all')
    asyncio.run(main(parser.parse_args().browser))
