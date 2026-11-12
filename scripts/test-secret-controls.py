#!/usr/bin/env python3
"""Real-WASM credential cards: compact scope and automatic expiry with local fixtures."""
import argparse
import asyncio
import functools
import http.server
import json
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.parse import urlparse
from playwright.async_api import async_playwright, expect
ROOT = Path(__file__).resolve().parents[1]
USER = '00000000-0000-4000-8000-000000000001'
SESSION = '00000000-0000-4000-8000-000000000002'
REQUEST = '00000000-0000-4000-8000-000000000003'
SECRET = '00000000-0000-4000-8000-000000000004'
NOW = datetime(2026, 11, 12, 12, tzinfo=timezone.utc)
class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path in ['/dashboard', '/secrets/authorize', '/secrets/verified']: self.path = '/index.html'
        try: super().do_GET()
        except (BrokenPipeError, ConnectionResetError): pass
    def log_message(self, *_): pass
async def exercise(browser, base, output, width, grouped=False, approve=False, snapshot_only=False, fail_verify=False):
    context = await browser.new_context(viewport=dict(width=width,height=950),service_workers='block')
    page = await context.new_page()
    await page.clock.install(time=NOW)
    state = dict(request='pending',approvals=0,loads=0,checks=0)
    verify_release, decision_release, poll_release, poll_started = [asyncio.Event() for _ in range(4)]
    errors, navigations = [], []
    page.on('pageerror',lambda e: errors.append(str(e)))
    page.on('request',lambda r: navigations.append(r.url) if r.is_navigation_request() and r.frame == page.main_frame else None)
    session = dict(id=SESSION,user_id=USER,session_key=SESSION,session_name='Credential access',working_directory='/fixture',status='active',paused=False,hostname='fixture',agent_type='claude',my_role='owner',last_activity=NOW.isoformat(),created_at=NOW.isoformat(),updated_at=NOW.isoformat())
    async def api(route):
        path = urlparse(route.request.url).path
        if path == '/api/config': data = dict(app_title='Portal',server_version='fixture',protocol_version=1)
        elif path == '/api/auth/me': data = dict(id=USER,email='fixture@example.invalid',name='Fixture',is_admin=False)
        elif path == '/api/sessions': data = dict(sessions=[session])
        elif path == '/api/portal-connections': data = dict(connections=[])
        elif path == '/api/launchers': data = []
        elif path.endswith('/messages'):
            content = dict(type='portal',content=[dict(type='secret_access_group' if grouped else 'secret_access_request',request_id=REQUEST)])
            data = dict(total=1,messages=[dict(role='portal',content=json.dumps(content),created_at=NOW.isoformat())])
        elif path == '/api/secrets/verification': data = dict(verified=False,providers=['google'])
        elif path == '/api/secrets/verification/check':
            assert route.request.method == 'POST' and route.request.headers['x-portal-secrets'] == '1'
            state['checks'] += 1
            await verify_release.wait()
            data = dict(verified=not fail_verify,providers=['google'])
        elif path.endswith('/decision'):
            assert state['checks'] == 1 and not fail_verify
            state['approvals'] += 1
            await decision_release.wait()
            state['request'] = 'approved'
            data = {}
        elif path in [f'/api/secrets/requests/{REQUEST}',f'/api/secrets/request-groups/{REQUEST}']:
            state['loads'] += 1
            if not approve and not snapshot_only and state['loads'] == 1:
                await route.fulfill(status=503,body='unavailable')
                return
            data = dict(id=REQUEST,secret_id=SECRET,owner_user_id=USER,namespace='personal',name='deploy-token',version=3,session_id=SESSION,launcher_id=SECRET,env_name='DEPLOY_TOKEN',expires_in_seconds=900,state=state['request'],created_at=NOW.isoformat(),expires_at=(NOW+timedelta(seconds=300 if state['request']=='approved' else 60)).isoformat(),binding_id=SECRET if state['request']=='approved' else None)
            if grouped:
                second = dict(data,id=SECRET,name='registry-token',env_name='REGISTRY_TOKEN',namespace='organization')
                data = dict(id=REQUEST,state=state['request'],items=[data,second])
            if approve and state['loads'] == 2:
                # Hold a background poll and return its pre-approval snapshot late.
                poll_started.set()
                await poll_release.wait()
        else: data = {}
        await route.fulfill(content_type='application/json',body=json.dumps(data))
    async def ws(route):
        def message(raw):
            if json.loads(raw).get('type') == 'Hello': route.send(json.dumps(dict(type='HelloAck',protocol_version=1,success=True)))
        route.on_message(message)
    await context.route('**/api/**',api)
    await context.route_web_socket('**/ws/**',ws)
    await page.goto(base+f'/dashboard?session={SESSION}')
    card = page.locator('.secret-access-request')
    if not approve and not snapshot_only:
        await expect(card.get_by_role('alert')).to_contain_text('Retrying automatically')
        await page.clock.fast_forward(5_001)
    approval = card.get_by_role('button',name='Approve all credentials' if grouped else 'Approve access',exact=True)
    await expect(approval).to_be_enabled(timeout=30000)
    if grouped: await expect(card.locator('.secret-access-group > ul > li')).to_have_count(2)
    await card.screenshot(path=str(output/f'credential-{grouped}-pending-{width}.png'))
    print(f'card grouped={grouped} width={width} height={(await card.bounding_box())["height"]}')
    if snapshot_only:
        await context.close()
        return
    await expect(card.get_by_role('button',name='Refresh status')).to_have_count(0)
    assert (await approval.bounding_box())['height'] >= 44
    assert await card.evaluate('e => e.scrollWidth <= e.clientWidth + 1')
    count = len(navigations)
    if approve:
        await expect(card.get_by_role('button',name='Verify Portal session')).to_have_count(0)
        assert state['checks'] == 0
        await page.clock.fast_forward(5_001)
        await asyncio.wait_for(poll_started.wait(), timeout=10)
        await expect(approval).to_be_enabled()
        await approval.click()
        await expect(card.get_by_role('status')).to_have_text('Verifying your Portal session…')
        await expect(approval).to_be_disabled()
        await expect(card.locator('.secret-verification-spinner')).to_be_visible()
        await approval.dispatch_event('click')
        assert state['approvals'] == 0
        verify_release.set()
        if fail_verify:
            await expect(card.get_by_role('alert')).to_contain_text('verification did not succeed')
            assert state['approvals'] == 0 and state['checks'] == 1
            await expect(approval).to_be_enabled()
            poll_release.set()
            await context.close()
            return
        await expect(card.get_by_role('status')).to_have_text('Approving access…')
        await expect(approval).to_be_disabled()
        assert state['approvals'] == 1
        await card.screenshot(path=str(output/f'credential-{grouped}-approving-{width}.png'))
        decision_release.set()
        await expect(card.locator('.secret-access-status')).to_have_text('Approved')
        poll_release.set()
        await page.clock.run_for(100)
        await expect(card.locator('.secret-access-status')).to_have_text('Approved')
        assert state['checks'] == 1 and state['approvals'] == 1
        await expect(card.get_by_role('button')).to_have_count(0)
        await page.clock.fast_forward(61_000)
        await expect(card.locator('.secret-access-status')).to_have_text('Approved')
        await page.clock.set_system_time(NOW+timedelta(seconds=301))
        await page.evaluate("window.dispatchEvent(new Event('focus'))")
    else:
        await page.clock.fast_forward(60_001)
    await expect(card.locator('.secret-access-status')).to_have_text('Expired')
    await expect(card.get_by_role('button')).to_have_count(0)
    await expect(card.locator('.secrets-verification')).to_have_count(0)
    await page.clock.fast_forward(6_000)
    await expect(card.locator('.secret-access-status')).to_have_text('Expired')
    assert state['approvals'] == int(approve) and len(navigations) == count
    assert not errors, errors
    await card.screenshot(path=str(output/f'credential-{grouped}-{approve}-expired-{width}.png'))
    await context.close()
async def main(args):
    args.output.mkdir(parents=True,exist_ok=True)
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Handler,directory=str(args.dist)))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch()
            for width in [1100,390]:
                for approve in ([False] if args.snapshot_only else [False,True]):
                    await exercise(browser,f'http://127.0.0.1:{server.server_port}',args.output,width,args.groups,approve,args.snapshot_only)
            if not args.snapshot_only:
                await exercise(browser,f'http://127.0.0.1:{server.server_port}',args.output,1100,args.groups,True,False,True)
            await browser.close()
    finally: server.shutdown()
    print('PASS: one-click verification/approval, progress, poll race, no duplicate POST, verification failure, expiry, desktop/mobile')
if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--groups',action='store_true')
    parser.add_argument('--snapshot-only',action='store_true')
    parser.add_argument('--dist',type=Path,default=ROOT/'frontend/dist')
    parser.add_argument('--output',type=Path,default=ROOT/'reports/secret-controls')
    asyncio.run(main(parser.parse_args()))
