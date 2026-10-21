#!/usr/bin/env python3
"""Run after trunk build; checks child approval UI against typed API fixtures."""
import asyncio, functools, http.server, json, threading
from pathlib import Path
from urllib.parse import urlparse
from playwright.async_api import async_playwright, expect
ROOT=Path(__file__).resolve().parents[1]
USER='00000000-0000-4000-8000-000000000001'
SESSION='00000000-0000-4000-8000-000000000002'
HOST='00000000-0000-4000-8000-000000000003'
REQ='00000000-0000-4000-8000-000000000004'
class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path == '/dashboard': self.path='/index.html'
        super().do_GET()
    def log_message(self,*_): pass
async def main():
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Handler,directory=str(ROOT/'frontend/dist')))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    session=dict(id=SESSION,user_id=USER,session_key=SESSION,session_name='Parent agent',working_directory='/work/parent',status='active',paused=False,hostname='test-host',agent_type='codex',my_role='owner',last_activity='2026-10-21T00:00:00',created_at='2026-10-21T00:00:00',updated_at='2026-10-21T00:00:00')
    policy=dict(approval_mode='first_spawn',max_children=4,max_depth=2,idle_timeout_seconds=1800,stop_on_parent_end=False)
    request=dict(request_id=REQ,parent_session_id=SESSION,launcher_id=HOST,state='pending',created_at='2026-10-21T00:00:00Z',request=dict(request_id=REQ,launcher_id=HOST,prompt='Review the proposed change',launch=dict(working_directory='/work/child',session_name='Review worker',agent_type='codex',claude_args=[])))
    calls=[]
    async def api(route):
        path=urlparse(route.request.url).path
        if path.endswith('/spawn-policy'):
            if route.request.method=='PATCH':
                policy.update(route.request.post_data_json); calls.append('save')
            data=dict(policy=policy,first_spawn_approved=False)
        elif path.endswith('/spawn-requests'): data=dict(requests=[request])
        elif path.endswith('/approve'):
            calls.append('approve'); await route.fulfill(status=409,body='Request no longer pending'); return
        elif path=='/api/auth/me': data=dict(id=USER,email='test@example.invalid',name='Test',is_admin=False)
        elif path=='/api/sessions': data=dict(sessions=[session])
        elif path=='/api/launchers': data=[dict(launcher_id=HOST,launcher_name='Test host',hostname='test-host',connected=True,running_sessions=1,version='99.0.0')]
        elif path=='/api/portal-connections': data=dict(connections=[])
        else: data={}
        await route.fulfill(content_type='application/json',body=json.dumps(data))
    async def ws(route): route.on_message(lambda _:None)
    try:
        async with async_playwright() as p:
            browser=await p.chromium.launch()
            for width in (1440,390):
                page=await browser.new_page(viewport=dict(width=width,height=950))
                errors=[]; page.on('pageerror',lambda error:errors.append(str(error)))
                await page.route('**/api/**',api)
                await page.route_web_socket('**/ws/**',ws)
                await page.goto(f'http://127.0.0.1:{server.server_port}/dashboard?session={SESSION}')
                await page.get_by_role('button',name='Children',exact=True).click()
                dialog=page.get_by_role('dialog',name='Child sessions')
                await expect(dialog.get_by_text('/work/child',exact=True)).to_be_visible()
                await dialog.get_by_text('Worker policy and limits',exact=True).click()
                await expect(dialog.get_by_label('Approval',exact=True)).to_have_value('first_spawn')
                field=dialog.get_by_label('Maximum children',exact=True)
                await field.fill('12')
                await expect(field).to_be_focused()
                await dialog.get_by_role('button',name='Save worker policy').click()
                await expect(dialog.get_by_role('status')).to_be_visible()
                assert policy['max_children']==12
                await dialog.get_by_role('button',name='Approve worker').click()
                await expect(dialog.get_by_role('alert')).to_contain_text('Request no longer pending')
                assert await dialog.evaluate('(e)=>e.getBoundingClientRect().width <= innerWidth')
                assert not errors,errors
                (ROOT/'builds').mkdir(exist_ok=True)
                await page.screenshot(path=str(ROOT/f'builds/spawn-{width}.png'))
                await page.close()
            await browser.close()
            assert calls.count('approve')==2
            print('PASS: child request details, stable policy draft, save, approval failure, desktop/mobile')
    finally: server.shutdown()
if __name__=='__main__': asyncio.run(main())
