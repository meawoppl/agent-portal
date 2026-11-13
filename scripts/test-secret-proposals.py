#!/usr/bin/env python3
"""Real-WASM batch proposal review; synthetic local API fixtures only."""
import argparse, asyncio, copy, functools, http.server, json, threading, runpy
from datetime import timedelta
from pathlib import Path
from urllib.parse import urlparse
from playwright.async_api import async_playwright, expect
ROOT=Path(__file__).resolve().parents[1]
f=runpy.run_path(str(ROOT/'scripts/test-secret-controls.py'))
USER,SESSION,REQUEST,SECRET,NOW=[f[k] for k in ['USER','SESSION','REQUEST','SECRET','NOW']]
async def exercise(browser,base,output,width,expire=False,fail=False,update=False):
    context=await browser.new_context(viewport=dict(width=width,height=950),service_workers='block')
    page=await context.new_page();await page.clock.install(time=NOW)
    counters=dict(checks=0,decisions=0,loads=0)
    release,poll_release,poll_started=[asyncio.Event() for _ in range(3)]
    errors=[];page.on('pageerror',lambda e:errors.append(str(e)))
    navigations=[];page.on('request',lambda r:navigations.append(r.url) if r.is_navigation_request() and r.frame==page.main_frame else None)
    session=dict(id=SESSION,user_id=USER,session_key=SESSION,session_name='Proposed credentials',working_directory='/fixture',status='active',paused=False,hostname='fixture',agent_type='claude',my_role='owner',last_activity=NOW.isoformat(),created_at=NOW.isoformat(),updated_at=NOW.isoformat())
    data=dict(id=REQUEST,session_id=SESSION,state='pending',expires_at=(NOW+timedelta(seconds=60)).isoformat(),items=[dict(secret_id=SECRET,version_id=SECRET,namespace='personal',name='AWS_ACCESS_KEY_ID',description='Access key ID',recipients=[dict(user_id=USER,display_name='Jazz',email='jazz@example.test')]),dict(secret_id=USER,version_id=USER,namespace='personal',name='AWS_SECRET_ACCESS_KEY',description='Secret access key',recipients=[])])
    if update:
        data['items'][1]['update']=dict(version_id=SECRET,version=3,generation=7)
    async def api(route):
        path=urlparse(route.request.url).path
        if path=='/api/config':result=dict(app_title='Portal',server_version='fixture',protocol_version=1)
        elif path=='/api/auth/me':result=dict(id=USER,email='owner@example.test',name='Owner',is_admin=False)
        elif path=='/api/sessions':result=dict(sessions=[session])
        elif path=='/api/portal-connections':result=dict(connections=[])
        elif path=='/api/launchers':result=[]
        elif path.endswith('/messages'):
            content=dict(type='portal',content=[dict(type='secret_deposit_proposal',request_id=REQUEST)])
            result=dict(total=1,messages=[dict(role='portal',content=json.dumps(content),created_at=NOW.isoformat())])
        elif path=='/api/secrets/verification/check':
            counters['checks']+=1;assert route.request.headers['x-portal-secrets']=='1'
            await release.wait();result=dict(verified=not fail,providers=['google'])
        elif path.endswith('/decision'):
            counters['decisions']+=1;assert counters['checks']==1 and not fail
            body=json.loads(route.request.post_data);assert body['approve'] and len(body['items'])==2
            assert body['confirm_updates']==([USER] if update else [])
            assert body['items'][0]['name']=='AWS_KEY_ID' and body['items'][0]['recipients']==[]
            data['state']='approved'
            for item,edit in zip(data['items'],body['items']):
                item.update(name=edit['name'],description=edit['description'])
                item['recipients']=[r for r in item['recipients'] if r['user_id'] in edit['recipients']]
            result=data
        elif path==f'/api/secrets/deposits/{REQUEST}':
            counters['loads']+=1;result=copy.deepcopy(data)
            if counters['loads']==2 and not expire and not fail:
                poll_started.set();await poll_release.wait()
        else:result={}
        await route.fulfill(content_type='application/json',body=json.dumps(result))
    async def ws(route):
        def message(raw):
            if json.loads(raw).get('type')=='Hello':route.send(json.dumps(dict(type='HelloAck',protocol_version=1,success=True)))
        route.on_message(message)
    await context.route('**/api/**',api);await context.route_web_socket('**/ws/**',ws)
    await page.goto(base+f'/dashboard?session={SESSION}')
    card=page.locator('.secret-deposit-card');approve=card.get_by_role('button',name='Approve 2 secrets',exact=True)
    await expect(approve).to_be_enabled(timeout=30000)
    await expect(card.locator('.secret-deposit-item')).to_have_count(2)
    if update:
        await expect(card.get_by_text('Update version 3 to 4',exact=True)).to_be_visible()
        await expect(card.get_by_label('Personal secret',exact=True).nth(1)).to_be_disabled()
        await expect(card.get_by_text('Existing sharing is retained.',exact=True)).to_be_visible()
    assert await card.evaluate('e=>e.scrollWidth<=e.clientWidth+1')
    assert REQUEST not in await card.inner_text() and SECRET not in await card.inner_text()
    await card.screenshot(path=str(output/f'set-{width}-{update}-pending.png'))
    before=len(navigations)
    if expire:
        await page.clock.fast_forward(62000)
        await expect(card.get_by_role('button')).to_have_count(0)
        await expect(card.get_by_role('status')).to_have_text('Expired')
        assert counters['checks']==counters['decisions']==0
    else:
        await card.get_by_label('Personal secret',exact=True).nth(0).fill('AWS_KEY_ID')
        await card.get_by_role('checkbox').uncheck()
        if not fail:
            await page.clock.fast_forward(2001);await asyncio.wait_for(poll_started.wait(),10)
        await approve.click();await expect(card.get_by_role('status')).to_have_text('Verifying Portal access…')
        await expect(approve).to_be_disabled();await expect(card.locator('.secret-verification-spinner')).to_be_visible()
        await approve.dispatch_event('click');assert counters['checks']==1 and counters['decisions']==0
        release.set()
        if fail:
            await expect(card.get_by_role('alert')).to_contain_text('verification did not succeed');assert counters['decisions']==0
        else:
            await expect(card.get_by_role('status')).to_have_text('approved')
            poll_release.set();await page.clock.run_for(100)
            await expect(card.get_by_role('status')).to_have_text('approved')
            await expect(card.get_by_role('button')).to_have_count(0)
            assert counters['decisions']==1
    assert len(navigations)==before and not errors,errors
    await context.close()
async def main(args):
    args.output.mkdir(parents=True,exist_ok=True)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(f['Handler'],directory=str(args.dist)))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        async with async_playwright() as p:
            browser=await p.chromium.launch();base=f'http://127.0.0.1:{server.server_port}'
            for width in [1200,390]:
                await exercise(browser,base,args.output,width)
                await exercise(browser,base,args.output,width,update=True)
            await exercise(browser,base,args.output,390,expire=True)
            await exercise(browser,base,args.output,390,fail=True)
            await browser.close()
    finally:server.shutdown()
    print('Secret proposal browser assertions passed')
if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--dist',type=Path,default=ROOT/'frontend/dist');parser.add_argument('--output',type=Path,default=ROOT/'reports/secret-proposals')
    asyncio.run(main(parser.parse_args()))
