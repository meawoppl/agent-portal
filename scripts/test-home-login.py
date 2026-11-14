#!/usr/bin/env python3
"""Check installed-app startup against built WASM with authenticated API fixtures.

Run after `cd frontend && trunk build` with Python Playwright, Chromium and
WebKit installed. All API/socket traffic is intercepted; no real login is used.
"""
import asyncio
import functools
import http.server
import json
import threading
from pathlib import Path
from urllib.parse import urlparse

from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
USER = "00000000-0000-4000-8000-000000000001"


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path in ("/dashboard", "/demo"):
            self.path = "/index.html"
        try:
            super().do_GET()
        except (BrokenPipeError, ConnectionResetError):
            pass  # Reopening/navigating can cancel an in-flight static asset.

    def log_message(self, *_):
        pass


async def exercise(browser, device, base, name):
    context = await browser.new_context(**device, service_workers="block")
    identity_status = 200
    identity_ready = asyncio.Event()
    auth_actions, errors = [], []

    async def api(route):
        path = urlparse(route.request.url).path
        if path == "/api/auth/me":
            await identity_ready.wait()
            if identity_status != 200:
                await route.fulfill(status=identity_status, body="Account unavailable")
                return
            data = dict(id=USER, email="test@example.invalid", name="Test", is_admin=False)
        elif path.startswith("/api/auth/"):
            auth_actions.append(path)
            await route.fulfill(status=500, body="Unexpected login/logout")
            return
        elif path == "/api/config":
            data = dict(app_title="Agent Portal", server_version="test", protocol_version=1,
                        auth_providers=["google", "github"], splash_text="Agent Portal")
        elif path == "/api/portal-connections":
            data = dict(connections=[])
        elif path in ("/api/sessions", "/api/session-attention", "/api/agent/sessions"):
            data = dict(sessions=[])
        elif path == "/api/launchers":
            data = []
        elif path == "/api/metrics/recent":
            data = dict(metrics=[], latest_by_session=[])
        elif path == "/api/push/vapid-key":
            await route.fulfill(status=404, body="Not configured")
            return
        else:
            data = {}
        await route.fulfill(content_type="application/json", body=json.dumps(data))

    async def ws(route):
        def receive(raw):
            if json.loads(raw)["type"] == "Hello":
                route.send(json.dumps(dict(type="HelloAck", protocol_version=1, success=True)))
        route.on_message(receive)

    await context.route("**/api/**", api)
    await context.route_web_socket("**/ws/**", ws)
    # Avoid third-party assets while exercising the local app.
    await context.route("https://cdn.jsdelivr.net/**", lambda route: route.abort())
    page = await context.new_page()
    page.on("pageerror", lambda error: errors.append(str(error)))

    async def expect_dashboard(target):
        await expect(target).to_have_url(base + "/dashboard", timeout=30000)
        await expect(target.get_by_role("button", name="Logout", exact=True)).to_be_visible()
        await expect(target.locator(".login-button")).to_have_count(0)

    # Same entry point as an already-installed Android app; wait for identity
    # before choosing a destination, without flashing a sign-in prompt.
    await page.goto(base + "/")
    await expect(page.get_by_text("Loading account…", exact=True)).to_be_visible(timeout=30000)
    await expect(page.locator(".login-button")).to_have_count(0)
    identity_ready.set()
    await expect_dashboard(page)

    # Reopening the app and opening another window must reuse the valid login.
    await page.goto(base + "/")
    await expect_dashboard(page)
    reopened = await context.new_page()
    await reopened.goto(base + "/")
    await expect_dashboard(reopened)
    await reopened.close()

    # Replace the startup entry, so Back cannot bounce through it forever.
    await page.goto(base + "/demo")
    await page.goto(base + "/")
    await expect_dashboard(page)
    await page.go_back()
    await expect(page).to_have_url(base + "/demo")

    # An actually signed-out browser still gets the normal provider buttons.
    identity_status = 401
    await page.goto(base + "/")
    await expect(page.get_by_role("button", name="Sign in with Google")).to_be_visible()
    await expect(page.get_by_role("button", name="Sign in with GitHub")).to_be_visible()
    await expect(page).to_have_url(base + "/")

    # A transient identity failure is not a logout. Retry restores the route.
    identity_status = 503
    await page.reload()
    await expect(page.get_by_role("alert")).to_contain_text("Cannot load account")
    await expect(page.locator(".login-button")).to_have_count(0)
    identity_status = 200
    await page.get_by_role("button", name="Retry", exact=True).click()
    await expect_dashboard(page)
    assert not auth_actions, auth_actions
    assert not errors, errors
    await context.close()
    print(f"PASS {name}: existing login, reopen, Back, signed-out login, outage/retry")


async def main():
    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0),
        functools.partial(Handler, directory=str(ROOT / "frontend/dist")),
    )
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        async with async_playwright() as p:
            for engine, device, name in (
                (p.chromium, p.devices["Pixel 7"], "Android Chromium"),
                (p.webkit, p.devices["iPhone 13"], "iPhone WebKit"),
            ):
                browser = await engine.launch()
                try:
                    await exercise(browser, device, f"http://127.0.0.1:{server.server_port}", name)
                finally:
                    await browser.close()
    finally:
        server.shutdown()


if __name__ == "__main__":
    asyncio.run(main())
