#!/usr/bin/env python3
"""Smoke-test the built History recovery UI with deterministic API fixtures.

Requires: pip install playwright; playwright install chromium
Build frontend with trunk first, then run this from the repository root.
No real backend, native agent, credentials, or conversation is used.
"""
import asyncio
import functools
import http.server
import json
from pathlib import Path
import threading
from urllib.parse import urlparse

from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1] / "frontend" / "dist"
USER = "00000000-0000-4000-8000-000000000001"
HOST = "00000000-0000-4000-8000-000000000002"
SOURCE = "00000000-0000-4000-8000-000000000003"
RESTORED = "00000000-0000-5000-8000-000000000004"


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path in ("/history", "/dashboard") or urlparse(self.path).path.startswith("/history/"):
            self.path = "/index.html"
        super().do_GET()

    def log_message(self, *_args):
        pass


async def main():
    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0), functools.partial(Handler, directory=str(ROOT))
    )
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_port}"
    fixture = dict(owner=True, existing=None, capable=True, hosts=True, fail=True)
    posts = []
    manifest = dict(
        user_id=USER, session_id=SOURCE, session_name="Restore integration example",
        agent_type="codex", hostname="test-host", working_directory="/home/test/project",
    )

    async def api(route):
        path = urlparse(route.request.url).path
        if path == "/api/auth/me":
            data = dict(id=USER, email="history-test@example.invalid")
        elif path == "/api/portal-connections":
            data = dict(connections=[])
        elif path == "/api/history/sessions":
            data = dict(sessions=[manifest], is_admin=False, total=1)
        elif path.endswith("/manifest"):
            data = manifest
        elif path.endswith("/messages"):
            await route.fulfill(status=200, content_type="application/x-ndjson", body="")
            return
        elif path.endswith("/recovery"):
            if route.request.method == "POST":
                posts.append(route.request.post_data_json)
                if fixture["fail"]:
                    await route.fulfill(status=502, body="Saved native state is missing. Choose archived context.")
                    return
                data = dict(session_id=RESTORED)
            else:
                data = dict(can_restore=fixture["owner"], existing_session_id=fixture["existing"], transcript_available=True)
        elif path == "/api/launchers":
            data = [dict(launcher_id=HOST, launcher_name="Test computer", hostname="test-host", connected=True, running_sessions=0, version="test", capabilities=["session.history_restore_v1"] if fixture["capable"] else [])] if fixture["hosts"] else []
        else:
            data = {}
        await route.fulfill(status=200, content_type="application/json", body=json.dumps(data))

    try:
        async with async_playwright() as playwright:
            browser = await playwright.chromium.launch(headless=True)
            page = await browser.new_page(viewport=dict(width=1280, height=1000))
            await page.route("**/api/**", api)
            errors = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            await page.goto(f"{base}/history")
            try:
                await page.get_by_role("link", name=manifest["session_name"]).click()
            except Exception:
                print("Page errors:", errors, flush=True)
                print("Page text:", await page.locator("body").inner_text(), flush=True)
                raise
            await page.get_by_role("button", name="Restore from History", exact=True).click()
            await expect(page.get_by_label("Recovery method")).to_have_value("native")
            assert await page.locator(".history-restore").evaluate("el => parseFloat(getComputedStyle(el).paddingLeft) > 0"), "History stylesheet must load"
            await page.get_by_role("button", name="Restore paused session", exact=True).click()
            await expect(page.get_by_role("alert")).to_contain_text("native state is missing")
            assert posts[-1]["mode"] == "native"
            await page.get_by_label("Recovery method").select_option("context")
            directory = page.get_by_label("Working directory (on selected host)")
            await directory.fill("")
            await directory.press_sequentially("/home/test/recovered", delay=10)
            await expect(directory).to_be_focused()
            await expect(directory).to_have_value("/home/test/recovered")
            fixture["fail"] = False
            await page.get_by_role("button", name="Restore paused session", exact=True).click()
            await expect(page.get_by_role("link", name="Open restored session")).to_have_attribute("href", f"/dashboard?session={RESTORED}")
            assert posts[-1] == dict(launcher_id=HOST, working_directory="/home/test/recovered", mode="context")
            for width in [1280, 390]:
                await page.set_viewport_size(dict(width=width, height=900))
                fixture.update(existing=SOURCE)
                await page.reload()
                await expect(page.get_by_role("link", name="Open existing session")).to_have_attribute("href", f"/dashboard?session={SOURCE}")
                fixture.update(existing=None, hosts=False)
                await page.reload()
                await page.get_by_role("button", name="Restore from History", exact=True).click()
                await expect(page.get_by_role("button", name="Restore paused session", exact=True)).to_be_disabled()
                await expect(page.get_by_role("status")).to_contain_text("Reconnect test-host")
                fixture.update(hosts=True, capable=False)
                await page.reload()
                await page.get_by_role("button", name="Restore from History", exact=True).click()
                await expect(page.get_by_role("status")).to_contain_text("Update agent-portal")
                fixture.update(capable=True)
                await page.get_by_role("button", name="Refresh hosts").click()
                await expect(page.get_by_role("button", name="Restore paused session", exact=True)).to_be_enabled()
                assert await page.evaluate("document.documentElement.scrollWidth <= window.innerWidth"), "History recovery must fit mobile width"
                await page.screenshot(path=f"/tmp/portal-restore-{width}.png", full_page=True)
                fixture.update(owner=False)
                await page.reload()
                await expect(page.get_by_text("Only the archive owner can restore this conversation.")).to_be_visible()
                await expect(page.get_by_role("button", name="Restore from History", exact=True)).to_have_count(0)
                fixture.update(owner=True)
            assert not errors, errors
            await browser.close()
            print("History UI: navigation, native failure, context retry, focus, host gates, owner gate, existing-session link, desktop/mobile passed")
    finally:
        server.shutdown()


if __name__ == "__main__":
    asyncio.run(main())
