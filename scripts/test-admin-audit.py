#!/usr/bin/env python3
"""Real WASM smoke test of the admin audit tab with synthetic API fixtures."""
import asyncio
import functools
import http.server
import json
import os
import threading
from pathlib import Path
from urllib.parse import urlparse, parse_qs
from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
USER = '11111111-1111-4111-8111-111111111111'
class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path == '/admin': self.path = '/index.html'
        super().do_GET()
    def log_message(self, *_): pass

def event(i, category, name, outcome):
    return dict(id=f'00000000-0000-4000-8000-{i:012}', occurred_at='2026-11-13T03:00:00.000000Z', category=category,
        event_type=name, actor_kind='user', actor_user_id=USER, actor_email='alex@example.invalid', owner_user_id=None,
        session_id=None, launcher_id=None, target_kind='secret' if category=='secrets' else 'user', target_id=USER,
        operation_id=None, outcome=outcome, details={})

async def exercise(browser, base, width):
    context = await browser.new_context(viewport=dict(width=width,height=950),service_workers='block')
    page = await context.new_page()
    errors=[]; queries=[]
    page.on('pageerror',lambda error: errors.append(str(error)))
    async def api(route):
        url=urlparse(route.request.url); q=parse_qs(url.query)
        if url.path == '/api/auth/me': data=dict(id=USER,email='alex@example.invalid',is_admin=True)
        elif url.path == '/api/config': data=dict(app_title='Portal',server_version='fixture',protocol_version=1)
        elif url.path == '/api/admin/users': data=dict(users=[dict(id=USER,email='alex@example.invalid',is_admin=True)])
        elif url.path == '/api/admin/sessions': data=dict(sessions=[])
        elif url.path == '/api/admin/audit':
            queries.append(q)
            rows=[event(1,'identity','login','success'),event(2,'computers','computer.update.requested','requested'),event(3,'secrets','secret.access_decision','approved')]
            if 'category' in q: rows=[row for row in rows if row['category']==q['category'][0]]
            if 'before_id' in q: rows=[event(4,'secrets','secret.binding_redeemed','allowed')]
            data=dict(events=rows,has_more='before_id' not in q,retention_days=365)
        else: data={}
        await route.fulfill(content_type='application/json',body=json.dumps(data))
    await context.route('**/api/**',api)
    await page.goto(base+'/admin')
    await page.get_by_role('button',name='Audit log',exact=True).click()
    await expect(page.locator('.audit-event')).to_have_count(3)
    await expect(page.get_by_text('Retention: 365 days',exact=False)).to_be_visible()
    await page.locator('.audit-event').first.locator('summary').click()
    await expect(page.locator('.audit-event').first.locator('dd').filter(has_text=USER).first).to_be_visible()
    await page.get_by_label('Category',exact=True).select_option('secrets')
    await expect(page.locator('.audit-event')).to_have_count(1)
    await page.get_by_label('Actor',exact=True).select_option(USER)
    await expect(page.get_by_role('button',name='Older events')).to_be_enabled()
    await page.get_by_role('button',name='Older events').click()
    await expect(page.get_by_text('secret binding redeemed',exact=True)).to_be_visible()
    await expect(page.get_by_role('button',name='Older events')).to_be_disabled()
    assert any('before_id' in q and q.get('actor')==[USER] for q in queries)
    await page.get_by_role('button',name='Latest events').click()
    await expect(page.get_by_text('secret access decision',exact=True)).to_be_visible()
    await page.get_by_label('Category',exact=True).select_option('')
    await expect(page.locator('.audit-event')).to_have_count(3)
    assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth'), 'horizontal overflow'
    output=ROOT/'reports';output.mkdir(exist_ok=True)
    await page.screenshot(path=str(output/f'admin-audit-{width}.png'),full_page=True)
    assert not errors,errors
    await context.close()

async def main():
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Handler,directory=str(ROOT/'frontend/dist')))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        async with async_playwright() as p:
            browser=await p.chromium.launch(headless=True, executable_path=os.environ.get("PLAYWRIGHT_CHROMIUM_EXECUTABLE"))
            for width in (1280,390): await exercise(browser,f'http://127.0.0.1:{server.server_port}',width)
            await browser.close()
    finally: server.shutdown()
    print('Audit tab: desktop/mobile filters, cursor pagination, details, and layout passed')
if __name__ == '__main__': asyncio.run(main())
