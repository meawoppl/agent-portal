#!/usr/bin/env python3
"""Exercise the built UI with cross-session API fixtures in Chromium and WebKit.

Run after `trunk build` with Playwright installed. No real sessions are mutated.
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
A = '00000000-0000-4000-8000-000000000002'
B = '00000000-0000-4000-8000-000000000003'
C = '00000000-0000-4000-8000-000000000004'
REPLY = '00000000-0000-4000-8000-000000000005'
RETAINED_REPLY = '00000000-0000-4000-8000-000000000006'
NEWER_REPLY = '00000000-0000-4000-8000-000000000007'
AT = '2026-10-31T19:00:00.000001'

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path in ('/dashboard', '/settings'):
            self.path = '/index.html'
        super().do_GET()
    def log_message(self, *_):
        pass

async def exercise(browser, base, device, name):
    context = await browser.new_context(**device, service_workers='block')
    page = await context.new_page()
    errors, fetched, updates = [], [], []
    pruned = False
    fail_ack = False
    replace_before_ack = False
    page.on('pageerror', lambda error: errors.append(f'{error}\n{error.stack}'))
    sessions = [dict(id=id, user_id=USER, session_key=id, session_name=label,
                     working_directory='/work/project', status='active', paused=False,
                     hostname='test-host', agent_type='codex', my_role='owner',
                     last_activity=AT, created_at=AT, updated_at=AT)
                for id, label in [(A, 'Portal dev'), (B, 'AstroPIC'), (C, 'Alignment Tools')]]
    rows = {id: dict(session_id=id, reply_id=None, reply_at=None, preview='', unread=False,
                    needs_approval=False, working=False, connected=True, muted=False)
            for id in (A, B, C)}
    rows[B].update(working=True)
    rows[C].update(needs_approval=True)
    async def api(route):
        nonlocal replace_before_ack
        path = urlparse(route.request.url).path
        if path == '/api/session-attention':
            data = dict(sessions=list(rows.values()))
        elif path.endswith('/attention'):
            id = path.split('/')[3]
            req = route.request.post_data_json
            updates.append((id, req))
            if req.get('seen_reply_id') and not req.get('viewing'):
                if fail_ack:
                    await route.fulfill(status=500, body='Temporary failure')
                    return
                if replace_before_ack:
                    rows[id].update(reply_id=NEWER_REPLY, unread=True, preview='A newer response needs attention.')
                    replace_before_ack = False
            if req.get('seen_reply_id') == rows[id]['reply_id'] and req.get('seen_reply_id'):
                rows[id]['unread'] = False
            if req.get('muted') is not None:
                rows[id]['muted'] = req['muted']
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
            fetched.append(id)
            messages = []
            if id == B and rows[B]['reply_id'] and not pruned:
                messages = [dict(role='unknown', created_at=AT,
                                content=json.dumps(dict(type='turn.completed', usage=dict(input_tokens=1, output_tokens=1, cached_input_tokens=0))),
                                meta=dict(created_at=AT))]
            data = dict(messages=messages, total=len(messages))
        elif path.endswith('/turn-metrics') or path == '/api/metrics/recent':
            data = dict(metrics=[], latest_by_session=[])
        elif path.endswith('/edit-stack'):
            data = dict(items=[])
        elif path == '/api/push/vapid-key':
            await route.fulfill(status=404, body='Not configured')
            return
        elif path == '/api/launchers':
            data = []
        else:
            data = {}
        await route.fulfill(content_type='application/json', body=json.dumps(data))
    async def ws(route):
        def receive(raw):
            packet = json.loads(raw)
            if packet['type'] == 'Hello':
                route.send(json.dumps(dict(type='HelloAck', protocol_version=1, success=True)))
        route.on_message(receive)
    await page.route('**/api/**', api)
    await page.route_web_socket('**/ws/**', ws)
    await page.goto(f'{base}/dashboard?session={A}')
    await expect(page.get_by_role('button', name='Ready sessions: 1', exact=True)).to_be_visible(timeout=60000)
    textarea = page.locator('.session-view-wrapper.focused textarea').first
    await expect(textarea).to_be_enabled()
    await textarea.fill('Keep this unsent draft')
    # Complete an unopened session. No transcript subscription is required.
    rows[B].update(working=False, unread=True, reply_id=REPLY, reply_at=AT+'Z', preview='The CAD changes are ready for review.')
    await expect(page.get_by_role('button', name='Ready sessions: 2', exact=True)).to_be_visible(timeout=12000)
    assert B not in fetched, 'background transcript must stay unloaded'
    await expect(page.locator('.ready-toast')).to_contain_text('AstroPIC')
    await expect(textarea).to_have_value('Keep this unsent draft')
    await page.get_by_role('button', name='Ready sessions: 2', exact=True).click()
    dialog = page.get_by_role('dialog', name='Sessions', exact=True)
    await expect(dialog.get_by_text('Needs approval', exact=True)).to_be_visible()
    await expect(dialog.get_by_text('New reply', exact=True)).to_be_visible()
    await expect(dialog.get_by_text('The CAD changes are ready for review.', exact=True)).to_be_visible()
    assert await dialog.evaluate('(e) => e.getBoundingClientRect().width <= innerWidth')
    assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    # Opening the sheet alone must not acknowledge either session.
    assert not any(id == B and req.get('seen_reply_id') for id, req in updates)
    await dialog.get_by_role('button', name='Mute notifications for AstroPIC').click()
    await expect(dialog.get_by_role('button', name='Unmute notifications for AstroPIC')).to_be_visible(timeout=12000)
    (ROOT/'builds').mkdir(exist_ok=True)
    await page.screenshot(path=str(ROOT/'builds'/f'mobile-ready-{name}.png'))
    await dialog.locator('.ready-open').filter(has_text='AstroPIC').click()
    await expect(page.locator('.ready-current')).to_contain_text('AstroPIC')
    await expect(page.get_by_role('button', name='Ready sessions: 1', exact=True)).to_be_visible(timeout=15000)
    assert any(id == B and req.get('seen_reply_id') == REPLY for id,req in updates)
    # Unresolved approvals remain in the queue after viewing.
    await page.get_by_role('button', name='Next ready session').click()
    await expect(page.locator('.ready-current')).to_contain_text('Alignment Tools')
    await expect(page.get_by_role('button', name='Ready sessions: 1', exact=True)).to_be_visible()
    await expect(page.get_by_role('button', name='Next ready session')).to_be_disabled()
    await page.locator('.ready-current').click()
    await page.get_by_role('dialog').locator('.ready-open').filter(has_text='Portal dev').click()
    await expect(page.locator('.session-view-wrapper.focused textarea').first).to_have_value('Keep this unsent draft')
    # The sheet's settings entry must preserve the document, session and draft.
    await page.evaluate('window.readyDraftDocument = true')
    await page.locator('.ready-current').click()
    await page.get_by_role('button', name='Notification settings', exact=True).click()
    await expect(page.get_by_role('heading', name='Settings', exact=True)).to_be_visible()
    await page.get_by_role('button', name='< Back', exact=True).click()
    await expect(page.get_by_role('dialog', name='Sessions', exact=True)).not_to_be_visible()
    await expect(page.locator('.ready-current')).to_contain_text('Portal dev')
    await expect(page.locator('.session-view-wrapper.focused textarea').first).to_have_value('Keep this unsent draft')
    assert await page.evaluate('window.readyDraftDocument === true'), 'settings must not reload the document'
    # Receipt survives reload; queue is not reconstructed from every idle session.
    # Observe the new page's lease, not the previous page's final heartbeat.
    updates.clear()
    await page.reload()
    await expect(page.get_by_role('button', name='Ready sessions: 1', exact=True)).to_be_visible(timeout=15000)
    await page.set_viewport_size(dict(width=320,height=520))
    await expect(page.locator('.mobile-ready-bar')).to_be_visible()
    assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    # A fresh mobile page starts without a selected transcript.
    await page.locator('.ready-current').click()
    await page.get_by_role('dialog').locator('.ready-open').filter(has_text='Portal dev').click()
    # Settings covers the transcript, so it must release the viewing lease.
    for _ in range(80):
        if updates and updates[-1][1].get('viewing'):
            break
        await asyncio.sleep(0.1)
    assert updates and updates[-1][1].get('viewing'), 'visible conversation should renew its lease'
    await page.get_by_role('button', name='Settings', exact=True).click()
    for _ in range(80):
        if not updates[-1][1].get('viewing'):
            break
        await asyncio.sleep(0.1)
    assert not updates[-1][1].get('viewing'), 'settings overlay must not silence phone pushes'
    await page.get_by_text('Notifications', exact=True).first.click()
    await expect(page.get_by_text('Android:', exact=False) if name=='android' else page.get_by_text('iPhone/iPad:', exact=False)).to_be_visible()
    await page.get_by_role('button', name='< Back', exact=True).click()
    await page.set_viewport_size(dict(width=device['viewport']['width'], height=device['viewport']['height']))
    # Persistent approvals must not trap Next between the first two sessions.
    for row in rows.values():
        row.update(working=False, needs_approval=True)
    await expect(page.get_by_role('button', name='Ready sessions: 3', exact=True)).to_be_visible(timeout=12000)
    sequence = []
    for _ in range(4):
        previous = await page.locator('.ready-current').inner_text()
        await page.get_by_role('button', name='Next ready session').click()
        await expect(page.locator('.ready-current')).not_to_have_text(previous)
        sequence.append(await page.locator('.ready-current').inner_text())
    assert len(set(sequence[:3])) == 3 and sequence[0] == sequence[3], sequence
    # Retained snapshots have no terminal message in the loaded transcript.
    pruned = True
    for row in rows.values():
        row.update(needs_approval=False)
    rows[B].update(unread=True, reply_id=RETAINED_REPLY, reply_at='2026-10-31T19:10:00Z')
    rows[C].update(unread=True, reply_id=REPLY, reply_at=AT+'Z', needs_approval=True)
    await expect(page.get_by_role('button', name='Ready sessions: 2', exact=True)).to_be_visible(timeout=12000)
    await page.locator('.ready-current').click()
    await page.get_by_role('dialog').locator('.ready-open').filter(has_text='AstroPIC').click()
    updates.clear()
    for _ in range(80):
        if any(id == B and req.get('viewing') for id, req in updates):
            break
        await asyncio.sleep(0.1)
    assert any(id == B and req.get('viewing') for id, req in updates)
    assert not any(req.get('seen_reply_id') for _, req in updates), 'absent transcripts must not auto-ack'
    await page.get_by_role('button', name='Ready sessions: 2', exact=True).click()
    dialog = page.get_by_role('dialog', name='Sessions', exact=True)
    mark_b = dialog.get_by_role('button', name='Mark reply read for AstroPIC', exact=True)
    # A failed save is visible and leaves the reply available to retry.
    await page.set_viewport_size(dict(width=320, height=520))
    assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    fail_ack = True
    await mark_b.click()
    await expect(dialog.get_by_role('alert')).to_contain_text('Could not save')
    assert rows[B]['unread']
    fail_ack = False
    # A newer completion arriving after the click cannot be cleared by that click.
    replace_before_ack = True
    await mark_b.click()
    await expect(dialog.get_by_role('alert')).not_to_be_visible()
    assert rows[B]['unread'] and rows[B]['reply_id'] == NEWER_REPLY
    assert any(id == B and req.get('seen_reply_id') == RETAINED_REPLY for id, req in updates)
    # Wait for the next feed to deliver the new ID, then acknowledge it explicitly.
    await expect(dialog.get_by_text('A newer response needs attention.', exact=True)).to_be_visible(timeout=12000)
    await mark_b.click()
    await expect(page.get_by_role('button', name='Ready sessions: 1', exact=True)).to_be_visible(timeout=12000)
    assert not rows[B]['unread']
    # Reading a reply never resolves a pending approval.
    await dialog.get_by_role('button', name='Mark reply read for Alignment Tools', exact=True).click()
    await expect(dialog.get_by_role('button', name='Mark reply read for Alignment Tools', exact=True)).not_to_be_visible(timeout=12000)
    await expect(dialog.get_by_text('Needs approval', exact=True)).to_be_visible()
    await expect(page.get_by_role('button', name='Ready sessions: 1', exact=True)).to_be_visible()
    assert not rows[C]['unread'] and rows[C]['needs_approval']
    # The new row actions fit the narrowest supported phone viewport.
    await page.set_viewport_size(dict(width=320, height=520))
    assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    assert not errors, errors
    await context.close()
    print(f'PASS {name}: background completion, queue, approval, mute, precise receipt, draft, reload, narrow viewport, settings releases presence, setup guidance, settings draft, round-robin Next, retained reply acknowledgment, stale acknowledgment, retry')

async def main(selected):
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Handler,directory=str(ROOT/'frontend/dist')))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        async with async_playwright() as p:
            for name, engine, device in [('android', p.chromium, 'Pixel 7'), ('iphone', p.webkit, 'iPhone 13')]:
                if selected not in ('all', name):
                    continue
                browser = await engine.launch()
                await exercise(browser, f'http://127.0.0.1:{server.server_port}', p.devices[device], name)
                await browser.close()
    finally:
        server.shutdown()

if __name__=='__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--browser', choices=['all', 'android', 'iphone'], default='all')
    asyncio.run(main(parser.parse_args().browser))
