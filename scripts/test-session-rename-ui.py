#!/usr/bin/env python3
"""Exercise the built dashboard rename controls using deterministic API fixtures.

Build frontend with trunk, install Python Playwright + Chromium, then run from
this checkout. No agent or real conversation is used.
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
SESSION = "00000000-0000-4000-8000-000000000002"


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path == "/dashboard":
            self.path = "/index.html"
        super().do_GET()

    def log_message(self, *_args):
        pass


async def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(ROOT)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_port}"
    session = dict(id=SESSION, user_id=USER, session_key=SESSION, session_name="Original session",
                   working_directory="/home/test/project", status="disconnected", paused=True,
                   hostname="test-host", agent_type="codex", my_role="owner",
                   last_activity="2026-01-01T00:00:00", created_at="2026-01-01T00:00:00", updated_at="2026-01-01T00:00:00")
    fixture = dict(fail=False)
    patches = []

    async def api(route):
        path = urlparse(route.request.url).path
        if route.request.method == "PATCH":
            assert path == f"/api/sessions/{SESSION}/name"
            name = route.request.post_data_json["session_name"]
            patches.append(name)
            if fixture["fail"]:
                await route.fulfill(status=502, body="Rename temporarily unavailable")
                return
            session["session_name"] = name
            data = dict(session_id=SESSION, session_name=name)
        elif path == "/api/sessions":
            data = dict(sessions=[session])
        elif path == "/api/auth/me":
            data = dict(id=USER, email="test@example.invalid", name="Test user", is_admin=False)
        elif path.endswith("/turn-metrics") or path == "/api/metrics/recent":
            data = dict(metrics=[], latest_session_metrics=[])
        elif path.endswith("/edit-stack"):
            data = dict(items=[])
        elif path.endswith("/messages"):
            data = dict(messages=[], total=0)
        elif path == "/api/launchers":
            data = []
        else:
            data = {}
        await route.fulfill(status=200, content_type="application/json", body=json.dumps(data))

    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch(headless=True)
            page = await browser.new_page(viewport=dict(width=1280, height=900))
            await page.route("**/api/**", api)
            await page.route_web_socket("**/ws/client", lambda ws: None)
            errors=[]
            page.on("pageerror", lambda e: errors.append(str(e)))
            await page.goto(f"{base}/dashboard?session={SESSION}")
            title = page.locator(".session-name-button")
            await expect(title).to_be_visible()
            # Menu and title open the same dialog. Escape/Cancel never mutate.
            await page.locator(".pill-menu-toggle").click()
            await page.get_by_role("button", name="Rename session Change display name").click()
            field = page.get_by_label("Session name", exact=True)
            await expect(field).to_be_focused()
            await field.fill("Discard me")
            await field.press("Escape")
            await expect(field).to_have_count(0)
            assert not patches
            await title.click()
            await field.fill("   ")
            await field.press("Enter")
            await expect(page.get_by_role("alert")).to_contain_text("Enter a session name")
            assert not patches
            await field.fill("")
            await field.press_sequentially("Revised session", delay=15)
            # The dashboard polls and the rail renders every 100 ms: neither
            # may replace the active input or overwrite its local draft.
            await page.wait_for_event("response", predicate=lambda r: urlparse(r.url).path == "/api/sessions", timeout=7000)
            await expect(field).to_be_focused()
            await expect(field).to_have_value("Revised session")
            fixture["fail"] = True
            await field.press("Enter")
            await expect(page.get_by_role("alert")).to_contain_text("temporarily unavailable")
            await expect(field).to_have_value("Revised session")
            fixture["fail"] = False
            await page.get_by_role("button", name="Save name", exact=True).click()
            await expect(field).to_have_count(0)
            await expect(title).to_contain_text("Revised session")
            await expect(page.locator(".pill-session-name")).to_have_text("Revised session")
            await page.goto(f"{base}/dashboard?session={SESSION}")
            await expect(title).to_contain_text("Revised session")
            # On phones the rail/name label are hidden; the Rename title action
            # must remain visible, usable by editors, and fit the viewport.
            await page.set_viewport_size(dict(width=390, height=844))
            session["my_role"] = "editor"
            await page.goto(f"{base}/dashboard?session={SESSION}")
            await expect(title).to_be_visible()
            await title.click()
            await field.fill("  Mobile name  ")
            await page.screenshot(path="/tmp/portal-session-rename-mobile.png", full_page=True, animations="disabled")
            await field.press("Enter")
            await expect(field).to_have_count(0)
            assert patches[-1] == "Mobile name"
            await expect(page.locator("#mobile-session-select")).to_contain_text("Mobile name")
            assert await page.evaluate("document.documentElement.scrollWidth <= innerWidth")
            session["my_role"] = "viewer"
            await page.goto(f"{base}/dashboard?session={SESSION}")
            await expect(title).to_have_count(0)
            await page.set_viewport_size(dict(width=1280, height=900))
            await page.locator(".pill-menu-toggle").click()
            await expect(page.get_by_role("button", name="Rename session Change display name")).to_have_count(0)
            assert not errors, errors
            await browser.close()
            print("Rename UI passed: menu/title, focus during polling, Escape, validation, failure/retry, Enter, persisted display, mobile, owner/editor/viewer")
    finally:
        server.shutdown()


if __name__ == "__main__":
    asyncio.run(main())
