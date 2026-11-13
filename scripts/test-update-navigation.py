#!/usr/bin/env python3
"""Exercise session selection across a server-version reconnect (synthetic APIs)."""
import asyncio, functools, http.server, json, re, threading
from pathlib import Path
from urllib.parse import urlparse
from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
USER, A, B = [f"00000000-0000-4000-8000-{i:012d}" for i in range(1, 4)]


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path == "/dashboard":
            self.path = "/index.html"
        super().do_GET()

    def log_message(self, *_):
        pass


async def main():
    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0),
        functools.partial(Handler, directory=str(ROOT / "frontend/dist")),
    )
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch()
            page = await browser.new_page(viewport=dict(width=1440, height=900))
            version = "2.15.1906"
            sockets = []
            errors = []
            offline = False
            handshake = asyncio.Event()
            handshake.set()
            waiting = asyncio.Event()
            page.on("pageerror", lambda e: errors.append(str(e)))

            async def api(route):
                path = urlparse(route.request.url).path
                if offline:
                    await route.fulfill(status=503, body="Restarting")
                    return
                if path == "/api/config":
                    data = dict(
                        app_title="Portal", server_version=version, protocol_version=1
                    )
                elif path == "/api/auth/me":
                    data = dict(
                        id=USER, email="test@example.test", name="Test", is_admin=False
                    )
                elif path == "/api/sessions":
                    data = dict(
                        sessions=[
                            dict(
                                id=id,
                                user_id=USER,
                                session_key=id,
                                session_name=name,
                                working_directory="/fixture",
                                status="active",
                                paused=False,
                                hostname="fixture",
                                agent_type="codex",
                                my_role="owner",
                                last_activity="2026-11-01T00:00:00",
                                created_at="2026-11-01T00:00:00",
                                updated_at="2026-11-01T00:00:00",
                            )
                            for id, name in [(A, "First"), (B, "Second")]
                        ]
                    )
                elif path == "/api/portal-connections":
                    data = dict(connections=[])
                elif path == "/api/launchers":
                    data = []
                elif path.endswith("/messages"):
                    data = dict(messages=[], total=0)
                elif path.endswith("/edit-stack"):
                    data = dict(items=[])
                else:
                    data = {}
                await route.fulfill(
                    content_type="application/json", body=json.dumps(data)
                )

            async def ws(route):
                sockets.append(route)

                async def receive(raw):
                    if json.loads(raw).get("type") == "Hello":
                        waiting.set()
                        await handshake.wait()
                        route.send(
                            json.dumps(
                                dict(type="HelloAck", protocol_version=1, success=True)
                            )
                        )

                route.on_message(lambda raw: asyncio.create_task(receive(raw)))

            await page.route("**/api/**", api)
            await page.route_web_socket("**/ws/**", ws)
            await page.goto(
                f"http://127.0.0.1:{server.server_port}/dashboard?session={A}"
            )
            pill = lambda id: page.locator(f'.session-pill[data-session-key="0/{id}"]')
            await expect(pill(A)).to_have_class(
                re.compile(r"\bfocused\b"), timeout=30000
            )
            await pill(B).click()
            await expect(pill(B)).to_have_class(re.compile(r"\bfocused\b"))
            version = "2.15.1907"
            for socket in list(sockets):
                await socket.close()
            await expect(page.locator(".update-available-banner")).to_be_visible(
                timeout=20000
            )
            offline = True
            for socket in list(sockets):
                try:
                    await socket.close()
                except Exception:
                    pass
            await pill(A).click()
            await expect(pill(A)).to_have_class(re.compile(r"\bfocused\b"))
            await pill(B).click()
            await expect(pill(B)).to_have_class(re.compile(r"\bfocused\b"))
            await expect(page.locator(".server-shutdown-banner")).to_be_visible(
                timeout=12000
            )
            await page.screenshot(path="/tmp/update-navigation-before.png")
            await pill(A).click()
            await expect(pill(A)).to_have_class(re.compile(r"\bfocused\b"))
            await pill(B).click()
            await expect(pill(B)).to_have_class(re.compile(r"\bfocused\b"))
            for position in ("top", "bottom", "left", "right"):
                await page.locator(".dashboard-body").evaluate(
                    "(e,p) => e.className = `dashboard-body rail-${p}`", position
                )
                for theme in ("tokyo", "apple", "windows"):
                    await page.evaluate(
                        "(t) => document.documentElement.dataset.ui=t", theme
                    )
                    await pill(A).click()
                    await expect(pill(A)).to_have_class(re.compile(r"\bfocused\b"))
                    await pill(B).click()
                    await expect(pill(B)).to_have_class(re.compile(r"\bfocused\b"))
            # Config is reachable before the socket handshake is ready. The
            # reconnecting state must persist until the acknowledgement arrives.
            handshake.clear()
            waiting.clear()
            offline = False
            await asyncio.wait_for(waiting.wait(), timeout=15)
            await expect(page.locator(".server-shutdown-banner")).to_be_visible()
            await pill(A).click()
            await expect(pill(A)).to_have_class(re.compile(r"\bfocused\b"))
            handshake.set()
            await expect(page.locator(".server-shutdown-banner")).to_have_count(
                0, timeout=15000
            )
            await expect(page.locator(".update-available-banner")).to_be_visible()
            await pill(B).click()
            await expect(pill(B)).to_have_class(re.compile(r"\bfocused\b"))
            assert not errors, errors
            print("PASS update reconnect navigation")
            await browser.close()
    finally:
        server.shutdown()


if __name__ == "__main__":
    asyncio.run(main())
