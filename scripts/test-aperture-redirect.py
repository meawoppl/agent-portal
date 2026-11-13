#!/usr/bin/env python3
"""Verify old SPA links preserve observer fragments when leaving for Pages."""
import json
import asyncio
import functools
import http.server
import threading
from pathlib import Path
from playwright.async_api import async_playwright, expect

DESTINATION = "https://meawoppl.github.io/agent-portal/"
ROOT = Path(__file__).resolve().parents[1]


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith("/aperture"):
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
            page = await browser.new_page()

            async def api(route):
                data = {}
                if route.request.url.endswith("/api/auth/me"):
                    data = dict(
                        id="00000000-0000-4000-8000-000000000001",
                        email="test@example.test",
                        name="Test",
                        is_admin=False,
                    )
                elif route.request.url.endswith("/api/portal-connections"):
                    data = dict(connections=[])
                await route.fulfill(
                    content_type="application/json", body=json.dumps(data)
                )

            await page.route("**/api/**", api)
            await page.route(
                DESTINATION + "**",
                lambda route: route.fulfill(
                    body="<h1>External exhibit</h1>", content_type="text/html"
                ),
            )
            for fragment in ("", "#observer-viewer", "#observer-editor"):
                await page.goto(
                    f"http://127.0.0.1:{server.server_port}/aperture{fragment}"
                )
                await expect(page).to_have_url(DESTINATION + fragment)
                await expect(
                    page.get_by_role("heading", name="External exhibit")
                ).to_be_visible()
            await browser.close()
            print("PASS legacy SPA redirect and both observer roles")
    finally:
        server.shutdown()


if __name__ == "__main__":
    asyncio.run(main())
