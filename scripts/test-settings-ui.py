#!/usr/bin/env python3
"""Exercise settings reorganization against the built WASM app and API fixtures.

Run after `cd frontend && trunk build` with Python Playwright and Chromium.
No real sessions are mutated.
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
USER = "00000000-0000-4000-8000-000000000001"
SESSION = "00000000-0000-4000-8000-000000000002"
SECOND = "00000000-0000-4000-8000-000000000003"


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path == "/settings":
            self.path = "/index.html"
        super().do_GET()

    def log_message(self, *_args):
        pass


async def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(ROOT / "frontend/dist")))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_port}"
    session = dict(id=SESSION, user_id=USER, session_key=SESSION, session_name="Build site",
                   working_directory="/home/test/site", status="active", paused=False,
                   hostname="test-host", agent_type="codex", my_role="owner",
                   last_activity="2026-10-21T00:00:00", created_at="2026-10-21T00:00:00", updated_at="2026-10-21T00:00:00")
    other = dict(session, id=SECOND, session_key=SECOND, session_name="No forward", working_directory="/home/test/other")
    forward = dict(session_id=SESSION, session_name="Build site", port=8080, url="https://example.invalid/", public=False,
                   connections=[dict(stream_id=SECOND, port=8080, transport="binary", opened_at="2026-10-21T00:00:00Z")])
    state = dict(fail_patch=False, fail_fetch=False)
    requests = []

    async def api(route):
        path = urlparse(route.request.url).path
        requests.append((route.request.method, path))
        if route.request.method == "PATCH":
            assert path == f"/api/sessions/{SESSION}/forwards/public"
            if state["fail_patch"]:
                await route.fulfill(status=500, body="Failed")
                return
            forward["public"] = route.request.post_data_json["public"]
            await route.fulfill(status=204)
            return
        if path == "/api/auth/me":
            data = dict(id=USER, email="test@example.invalid", name="Test user", is_admin=False)
        elif path == "/api/sessions":
            data = dict(sessions=[session, other])
        elif path == "/api/forwards":
            if state["fail_fetch"]:
                await route.fulfill(status=500, body="Unavailable")
                return
            data = dict(forwards=[forward])
        elif path == "/api/settings/identities":
            data = dict(identities=[])
        elif path == "/api/portal-connections":
            data = dict(connections=[])
        else:
            data = {}
        await route.fulfill(content_type="application/json", body=json.dumps(data))

    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch(headless=True)
            for width in (1440, 390):
                forward["public"] = False
                state.update(fail_patch=False, fail_fetch=False)
                page = await browser.new_page(viewport=dict(width=width, height=1000))
                errors = []
                page.on("pageerror", lambda e: errors.append(str(e)))
                await page.route("**/api/**", api)
                await page.goto(f"{base}/settings")
                tabs = page.locator(".settings-tabs")
                for removed in ("Sounds", "Forwarding", "Health Timer"):
                    await expect(tabs.get_by_role("button", name=removed, exact=True)).to_have_count(0)
                await tabs.get_by_role("button", name="Sessions").click()
                await expect(page.get_by_role("columnheader", name="Forwards", exact=True)).to_be_visible()
                row = page.locator(".session-row").filter(has_text="/home/test/site")
                await expect(row.locator(".forwarding-url")).to_have_attribute("href", forward["url"])
                await expect(page.locator(".session-row").filter(has_text="/home/test/other").locator(".session-forwards")).to_have_text("—")
                toggle = row.get_by_role("checkbox", name="Public forward for Build site")
                await toggle.check()
                await expect(toggle).to_be_checked()
                await expect(row.locator(".forwarding-badge")).to_have_text("Public")
                state["fail_patch"] = True
                await toggle.uncheck()
                await expect(row.get_by_role("alert")).to_contain_text("Could not change")
                await expect(toggle).to_be_checked()
                await row.locator("summary").click()
                await expect(row.locator(".forwarding-connections li")).to_contain_text("binary")
                state["fail_fetch"] = True
                await page.get_by_role("button", name="Refresh sessions and forwards").click()
                await expect(page.get_by_role("alert").filter(has_text="Could not refresh")).to_be_visible()
                state.update(fail_patch=False, fail_fetch=False)
                await page.get_by_role("button", name="Refresh sessions and forwards").click()
                await expect(page.get_by_role("alert")).to_have_count(0)
                await row.locator(".session-forwards").scroll_into_view_if_needed()
                await page.screenshot(path=str(ROOT / f"builds/settings-sessions-{width}.png"))
                await tabs.get_by_role("button", name="Appearance", exact=True).click()
                await expect(page.get_by_role("heading", name="Health timer", exact=True)).to_be_visible()
                interval = page.get_by_label("Reminder interval")
                await interval.fill("45")
                await interval.press("Tab")
                await page.get_by_label("Reminder message").fill("Stretch and rest your eyes")
                timer = page.locator(".health-settings-section")
                await timer.get_by_role("button", name="Disabled").click()
                await expect(timer.get_by_role("button", name="Enabled")).to_have_attribute("aria-pressed", "true")
                await page.reload()
                await page.locator(".settings-tabs").get_by_role("button", name="Appearance", exact=True).click()
                await expect(page.get_by_label("Reminder interval")).to_have_value("45")
                await expect(page.get_by_label("Reminder message")).to_have_value("Stretch and rest your eyes")
                await expect(page.locator(".health-settings-section button")).to_have_attribute("aria-pressed", "true")
                await page.locator(".health-settings-section").scroll_into_view_if_needed()
                await page.screenshot(path=str(ROOT / f"builds/settings-appearance-{width}.png"))
                assert not errors, errors
                await page.close()
            await browser.close()
        assert not any(path == "/api/settings/sound" for _, path in requests)
        print("PASS: settings navigation, forward controls/errors, connection details, and persisted health timer at desktop/phone widths")
    finally:
        server.shutdown()


if __name__ == "__main__":
    (ROOT / "builds").mkdir(exist_ok=True)
    asyncio.run(main())
