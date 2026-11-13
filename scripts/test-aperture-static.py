#!/usr/bin/env python3
"""Check the standalone exhibit at a project-site prefix, locally or deployed."""
import argparse
import asyncio
import functools
import http.server
import threading
from pathlib import Path
from urllib.parse import urlparse
from playwright.async_api import async_playwright, expect

ROOT = Path(__file__).resolve().parents[1]


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if not self.path.startswith("/agent-portal/"):
            self.send_error(404)
            return
        self.path = self.path[len("/agent-portal") :]
        super().do_GET()

    def log_message(self, *_):
        pass


async def exercise(base):
    async with async_playwright() as p:
        browser = await p.chromium.launch()
        page = await browser.new_page(viewport=dict(width=1440, height=1000))
        errors, failed, api = [], [], []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.on(
            "response",
            lambda r: failed.append((r.status, r.url)) if r.status >= 400 else None,
        )
        page.on("request", lambda r: api.append(r.url) if "/api/" in r.url else None)
        await page.goto(base)
        await expect(
            page.get_by_role("button", name="Observe your replacement")
        ).to_be_visible()
        await page.get_by_role("button", name="Enable audio").click()
        await expect(page.get_by_role("button", name="Audio on")).to_be_visible()
        await page.get_by_role("button", name="Observe your replacement").click()
        for index in range(8):
            await page.locator(".ap-chamber-tab").nth(index).click()
            await expect(page.locator(".ap-chamber-tab").nth(index)).to_have_attribute(
                "aria-current", "step"
            )
            if index == 2:
                await page.locator(".ap-terminal-command").click()
                frame = page.frame_locator(".ap-sample-site")
                await frame.locator("#rotate").click()
                await expect(frame.locator("#status")).to_contain_text("90")
                assert (
                    await page.locator(".ap-sample-site").get_attribute("sandbox")
                    == "allow-scripts"
                )
            if index in (1, 3, 4, 5, 6, 7):
                await page.wait_for_function(
                    'document.querySelector("video").readyState >= 1'
                )
        # Media/document payloads must be real responses rather than SPA fallbacks.
        for file, mime in [
            ("evidence/cake.stl", ""),
            ("evidence/justletgo-review.pdf", "application/pdf"),
            ("voice/welcome.mp3", "audio/"),
        ]:
            response = await page.request.get(base + "aperture-assets/" + file)
            assert response.ok and len(await response.body()) > 100
            assert "text/html" not in response.headers.get("content-type", "")
            if mime:
                assert mime in response.headers.get("content-type", "")
        await page.goto(base + "aperture-assets/trailer/index.html")
        await expect(page.locator("#begin")).to_be_enabled(timeout=60000)
        await page.locator("#begin").click()
        await page.wait_for_function('document.querySelector("audio").currentTime > 0')
        await page.get_by_role("link", name="Exhibit ↗").click()
        await expect(page.locator(".aperture")).to_be_visible()
        await page.goto(base + "#observer-viewer")
        await page.reload()
        await expect(page.locator(".ap-observer-banner")).to_contain_text("viewer")
        assert not errors, errors
        assert not failed, failed
        assert not api, api
        await browser.close()
        print(
            "PASS: sectors, narration, evidence, film, sandbox, observer links; no Portal API"
        )


if __name__ == "__main__":
    args = argparse.ArgumentParser()
    args.add_argument("--base")
    options = args.parse_args()
    if options.base:
        asyncio.run(exercise(options.base.rstrip("/") + "/"))
    else:
        server = http.server.ThreadingHTTPServer(
            ("127.0.0.1", 0),
            functools.partial(Handler, directory=str(ROOT / "aperture-frontend/dist")),
        )
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            asyncio.run(
                exercise(f"http://127.0.0.1:{server.server_port}/agent-portal/")
            )
        finally:
            server.shutdown()
