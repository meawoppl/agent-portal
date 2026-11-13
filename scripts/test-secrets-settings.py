#!/usr/bin/env python3
"""Exercise the real WASM inventory against synthetic API fixtures."""
import argparse
import asyncio
import functools
import http.server
import json
import threading
from pathlib import Path
from urllib.parse import urlparse, parse_qs
from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
USER = '11111111-1111-4111-8111-111111111111'
ALPHA_DESCRIPTION = 'Deployment credential for the synthetic staging database. Used by scheduled integration checks and test deployments; this deliberately long description should use the full inventory width rather than wrapping inside the Name column.'

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path == '/settings': self.path = '/index.html'
        super().do_GET()
    def log_message(self, *_): pass

def secret(i, name, namespace='personal', deleted=False):
    return dict(id=f'00000000-0000-4000-8000-{i:012d}', owner_user_id=USER, namespace=namespace, name=name, description=ALPHA_DESCRIPTION if name == 'Alpha' else (f'Synthetic credential for {name}' if name != 'Zulu' else ''), tags=['deleted'] if deleted else [], current_version=1, generation=1, created_at='2026-11-12T00:00:00Z', updated_at='2026-11-12T00:00:00Z', deleted_at='2026-11-12T00:00:00Z' if deleted else None, deleted_by=USER if deleted else None, can_read=True, can_use=True, can_write=True, can_manage=True, grants=[])

async def exercise(browser, base, output, width):
    context = await browser.new_context(viewport=dict(width=width, height=950), service_workers='block')
    page = await context.new_page()
    requests, errors = [], []
    release = asyncio.Event()
    checks = 0
    delete_requests = []
    delete_release = asyncio.Event()
    delete_started = asyncio.Event()
    alpha_deleted = False
    page.on('pageerror', lambda e: errors.append(str(e)))
    async def api(route):
        nonlocal checks, alpha_deleted
        url = urlparse(route.request.url)
        path = url.path
        requests.append(path)
        if path == '/api/config': data = dict(app_title='Portal',server_version='fixture',protocol_version=1)
        elif path == '/api/auth/me': data = dict(id=USER,email='fixture@example.invalid',name='Fixture',is_admin=True)
        elif path == '/api/portal-connections': data = dict(connections=[])
        elif path == '/api/secrets/verification': data = dict(verified=True,providers=['google'])
        elif path == '/api/secrets/verification/check':
            checks += 1
            await release.wait()
            if checks == 1:
                await route.fulfill(status=503,body='unavailable')
                return
            data = dict(verified=True,providers=['google'])
        elif path == '/api/secrets':
            removed = parse_qs(url.query).get('deleted') == ['true']
            data = dict(secrets=[secret(4,'Beta',deleted=True)] if removed else [secret(1,'Zulu'),secret(2,'bravo','organization')] + ([] if alpha_deleted else [secret(3,'Alpha')]),can_manage_organization=True)
        elif path == '/api/secrets/members': data = dict(members=[dict(user_id=USER,display_name='Fixture',email='fixture@example.invalid',identities=[])])
        elif path.endswith('/delete'):
            assert path == '/api/secrets/00000000-0000-4000-8000-000000000003/delete'
            assert route.request.method == 'POST'
            assert route.request.headers['x-portal-secrets'] == '1'
            assert json.loads(route.request.post_data) == dict(expected_generation=1)
            delete_requests.append(path)
            delete_started.set()
            await delete_release.wait()
            if len(delete_requests) == 1:
                await route.fulfill(status=503,body='unavailable')
                return
            alpha_deleted = True
            data = {}
        elif path.endswith('/versions'): data = dict(versions=[dict(version=1,created_at='2026-11-12T14:05:00Z',created_by=USER,current=True)])
        else: data = {}
        await route.fulfill(content_type='application/json',body=json.dumps(data))
    await context.route('**/api/**',api)
    await page.goto(base+'/settings?tab=secrets')
    check = page.get_by_role('button',name='Check Portal access',exact=True)
    await expect(check).to_be_visible()
    assert not any(p.startswith('/api/secrets') for p in requests)
    await expect(page.locator('.secrets-table')).to_have_count(0)
    await check.click()
    await expect(check).to_be_disabled()
    await expect(page.get_by_text('Checking Portal access…',exact=True)).to_be_visible()
    assert '/api/secrets' not in requests
    release.set()
    await expect(check).to_be_enabled()
    await expect(page.get_by_role('alert')).to_be_visible()
    assert '/api/secrets' not in requests
    await check.click()
    names = page.locator('.secrets-table .secret-name')
    await expect(names).to_have_text(['▸Alpha','▸bravo','▸Zulu'])
    assert 'Configured' not in await page.locator('.secrets-page').inner_text()
    alpha = page.get_by_role('button',name='Alpha',exact=True)
    await alpha.click()
    await expect(alpha).to_have_attribute('aria-expanded','true')
    expanded = page.locator('.secret-expanded-row')
    await expect(expanded).to_have_count(1)
    description = page.get_by_text(ALPHA_DESCRIPTION,exact=True)
    await expect(description).to_have_count(1)
    await expect(description).to_be_visible()
    assert await description.evaluate("e => e.closest('td').colSpan === 6")
    await expect(expanded.locator('dt')).to_have_count(0)
    await expect(page.get_by_role('columnheader',name='Updated',exact=True)).to_have_count(0)
    await expect(page.locator('.secrets-reference').first).to_contain_text('id/')
    await expect(expanded.get_by_text(ALPHA_DESCRIPTION,exact=True)).to_have_count(0)
    summary = page.locator('.secret-summary-row').filter(has=alpha)
    assert (await description.bounding_box())['width'] > (await summary.locator('th').bounding_box())['width']
    await expect(summary.locator('.secrets-owner')).to_be_visible()
    actions = expanded.locator('.secret-detail-actions')
    await expect(actions.get_by_role('button')).to_have_text(['Copy secret','Replace value','Delete secret'])
    assert 'Delete adds' not in await expanded.inner_text()
    history = expanded.locator('.secrets-versions')
    await expect(history.locator('li')).to_have_count(1)
    assert await history.evaluate("e => getComputedStyle(e).paddingLeft === '0px' && getComputedStyle(e).listStyleType === 'none'")
    for label in ['Name','Scope','Version','Owner','Reference','Shared with']:
        heading = page.get_by_role('columnheader').filter(has=page.get_by_role('button',name=label,exact=False))
        button = heading.get_by_role('button')
        await button.click()
        await expect(heading).to_have_attribute('aria-sort','descending' if label == 'Name' else 'ascending')
        await button.click()
        await expect(heading).to_have_attribute('aria-sort','ascending' if label == 'Name' else 'descending')
    await page.get_by_role('button',name='Name',exact=False).first.click()
    await expect(names).to_have_text(['▾Alpha','▸bravo','▸Zulu'])

    await expect(page.get_by_role('button',name='Start cleanup for all versions',exact=True)).to_have_count(0)
    await expect(page.get_by_text('Clean up leaked copies',exact=True)).to_have_count(0)
    assert await expanded.evaluate("e => e.previousElementSibling.previousElementSibling.textContent.includes('Alpha') && e.nextElementSibling.textContent.includes('bravo')")
    await expect(expanded.get_by_role('button',name='Copy secret',exact=True)).to_be_visible()
    await expanded.get_by_role('button',name='Replace value',exact=True).click()
    await expect(expanded.locator('form')).to_be_visible()
    await expanded.get_by_role('button',name='Cancel',exact=True).click()
    await page.screenshot(path=str(output/f'secrets-inline-{width}.png'),full_page=True)
    assert await page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'), 'horizontal overflow'
    await alpha.click()
    await expect(expanded).to_have_count(0)
    checkbox = page.get_by_role('checkbox',name='Show deleted secrets',exact=True)
    await checkbox.check()
    await expect(names).to_have_text(['▸Alpha','▸Beta','▸bravo','▸Zulu'])
    await page.get_by_role('button',name='Beta',exact=True).click()
    await expect(expanded.get_by_role('button',name='Restore secret',exact=True)).to_be_visible()
    await expect(expanded.get_by_role('button',name='Copy secret',exact=True)).to_have_count(0)
    await checkbox.uncheck()
    await expect(names).to_have_text(['▸Alpha','▸bravo','▸Zulu'])
    await expect(expanded).to_have_count(0)
    # Closing or switching the expanded row cancels an armed deletion.
    delete = page.get_by_role('button',name='Delete secret',exact=True)
    confirm = page.get_by_role('button',name='Click again to confirm',exact=True)
    await alpha.click()
    await delete.click()
    await expect(confirm).to_be_visible()
    assert not delete_requests
    await alpha.click()
    await alpha.click()
    await expect(delete).to_be_enabled()
    await delete.click()
    await page.get_by_role('button',name='bravo',exact=True).click()
    await expect(delete).to_be_enabled()
    await expect(confirm).to_have_count(0)
    await alpha.click()
    await delete.click()
    await expect(confirm).to_be_visible()
    assert not delete_requests
    await confirm.scroll_into_view_if_needed()
    await page.screenshot(path=str(output/f'secret-delete-confirm-{width}.png'))
    # Keyboard activation follows the same second-click confirmation.
    await confirm.press('Enter')
    await asyncio.wait_for(delete_started.wait(), timeout=5)
    await expect(delete).to_be_disabled()
    assert len(delete_requests) == 1
    delete_release.set()
    await expect(page.get_by_role('alert')).to_be_visible()
    await expect(delete).to_be_enabled()
    await delete.click()
    await expect(confirm).to_be_enabled()
    assert len(delete_requests) == 1, 'failure must disarm the button'
    await confirm.click()
    await expect(names).to_have_text(['▸bravo','▸Zulu'])
    assert len(delete_requests) == 2
    assert checks == 2 and not errors, errors
    await context.close()

async def main(args):
    args.output.mkdir(parents=True,exist_ok=True)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Handler,directory=str(args.dist)))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        async with async_playwright() as p:
            browser=await p.chromium.launch()
            for width in [1100,390]: await exercise(browser,f'http://127.0.0.1:{server.server_port}',args.output,width)
            await browser.close()
    finally: server.shutdown()
    print('PASS: inventory, mobile fit, two-click delete, no first-click request, reset on close/switch/failure, keyboard confirmation, pending guard')

if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dist',type=Path,default=ROOT/'frontend/dist')
    parser.add_argument('--output',type=Path,default=ROOT/'reports/secrets-settings')
    asyncio.run(main(parser.parse_args()))
