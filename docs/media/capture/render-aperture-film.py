"""Render one full Aperture film with cached scenes and parallel browser capture.

Usage: python render-aperture-film.py URL OUT_DIR ASSET_DIR MASTER_DIR [--workers 8]
MASTER_DIR contains each named evidence clip as <id>.mp4 and soundtrack.wav.
Requires Playwright/Chrome and ffmpeg. Cache reuse never substitutes old inputs.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import json
import math
from pathlib import Path
import subprocess
from time import perf_counter
from playwright.sync_api import sync_playwright

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("url")
parser.add_argument("out", type=Path)
parser.add_argument("assets", type=Path)
parser.add_argument("masters", type=Path)
parser.add_argument("--workers", type=int, default=8)
parser.add_argument("--fps", type=int, default=30)
parser.add_argument(
    "--encoder", choices=["h264_nvenc", "libx264"], default="h264_nvenc"
)
parser.add_argument("--capture-only", action="store_true")
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
cache = args.out / "scene-cache"
cache.mkdir(exist_ok=True)
url = args.url + ("&" if "?" in args.url else "?") + "capture&external-clips"


def encode(quality):
    if args.encoder == "h264_nvenc":
        return [
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p4",
            "-rc",
            "vbr",
            "-cq",
            str(quality),
            "-b:v",
            "0",
        ]
    return ["-c:v", "libx264", "-preset", "fast", "-crf", str(quality), "-threads", "3"]


def ffmpeg(params):
    subprocess.run(["ffmpeg", "-v", "error", "-y", *map(str, params)], check=True)


def open_page(playwright):
    browser = playwright.chromium.launch(channel="chrome", args=["--disable-gpu-vsync"])
    page = browser.new_page(viewport={"width": 1920, "height": 1080})
    page.goto(url, wait_until="domcontentloaded")
    page.wait_for_function("window.ready === true", timeout=60000)
    page.evaluate("document.fonts.ready")
    return browser, page


with sync_playwright() as p:
    browser, page = open_page(p)
    timeline = page.evaluate("window.FILM")
    browser.close()
(args.out / "timeline.json").write_text(json.dumps(timeline, indent=2))
# Render content, fonts and captions affect base frames; footage is composited separately.
hash_input = Path(__file__).read_bytes()
for pattern in [
    "trailer/*.html",
    "trailer/*.css",
    "trailer/*.js",
    "lines.json",
    "art/*.svg",
    "fonts/*.woff2",
]:
    for f in sorted(args.assets.glob(pattern)):
        hash_input += str(f.relative_to(args.assets)).encode() + f.read_bytes()
source_hash = hashlib.sha256(hash_input).hexdigest()


def capture_scene(item):
    index, scene = item
    start = math.floor(scene["start"] * args.fps)
    end = math.floor(scene["end"] * args.fps)
    if index == len(timeline["scenes"]) - 1:
        end = math.ceil(scene["end"] * args.fps)
    key = hashlib.sha256(
        json.dumps([source_hash, start, end, args.fps, args.encoder]).encode()
    ).hexdigest()[:18]
    result = cache / f"{index:02d}-{scene['id']}-{key}.mp4"
    if result.exists():
        return index, result, 0
    began = perf_counter()
    with sync_playwright() as p:
        browser, page = open_page(p)
        temp = result.with_name(result.stem + ".partial.mp4")
        process = subprocess.Popen(
            [
                "ffmpeg",
                "-v",
                "error",
                "-y",
                "-f",
                "image2pipe",
                "-framerate",
                str(args.fps),
                "-c:v",
                "mjpeg",
                "-i",
                "-",
                *encode(16),
                "-pix_fmt",
                "yuv420p",
                str(temp),
            ],
            stdin=subprocess.PIPE,
        )
        try:
            for frame in range(start, end):
                page.evaluate("(t)=>window.seek(t)", frame / args.fps)
                process.stdin.write(page.screenshot(type="jpeg", quality=92))
            process.stdin.close()
            if process.wait() != 0:
                raise RuntimeError(f"Encoder failed in {scene['id']}")
            temp.replace(result)
        except BaseException:
            process.kill()
            process.wait()
            temp.unlink(missing_ok=True)
            raise
        finally:
            browser.close()
    return index, result, perf_counter() - began


parts = {}
started = perf_counter()
with ThreadPoolExecutor(max_workers=args.workers) as pool:
    futures = [
        pool.submit(capture_scene, item) for item in enumerate(timeline["scenes"])
    ]
    for future in as_completed(futures):
        index, file, seconds = future.result()
        parts[index] = file
        print(
            f"{len(parts)}/{len(futures)} scenes: {file.name} ({seconds:.1f}s, zero means cached)",
            flush=True,
        )
if args.capture_only:
    raise SystemExit(0)
concat = args.out / "scenes.txt"
concat.write_text("".join(f"file '{parts[i].resolve()}'\n" for i in sorted(parts)))
base = args.out / "base.mp4"
ffmpeg(["-f", "concat", "-safe", "0", "-i", concat, "-c", "copy", base])
inputs = ["-i", base]
filters = []
previous = "0:v"
for i, clip in enumerate(timeline["clips"], 1):
    inputs += ["-i", args.masters / f"{clip['id']}.mp4"]
    duration = clip["end"] - clip["start"]
    rate = clip.get("rate", 1)
    h = clip["h"] - clip.get("cropBottom", 0)
    filters.append(
        f"[{i}:v]setpts=(PTS-STARTPTS)/{rate},fps={args.fps},scale={clip['w']}:{clip['h']},crop={clip['w']}:{h}:0:0,tpad=stop_mode=clone:stop_duration={duration},trim=duration={duration},format=yuva420p,fade=t=in:st=0:d=0.3:alpha=1,fade=t=out:st={duration - 0.3}:d=0.3:alpha=1,setpts=PTS-STARTPTS+{clip['start']}/TB[c{i}];[{previous}][c{i}]overlay=x={clip['x']}:y={clip['y']}:eof_action=pass:enable='between(t,{clip['start']},{clip['end']})'[v{i}]"
    )
    previous = f"v{i}"
sound_index = len(timeline["clips"]) + 1
inputs += ["-i", args.masters / "soundtrack.wav"]
output = args.out / "aperture-film-v4.mp4"
ffmpeg(
    [
        *inputs,
        "-filter_complex_threads",
        "6",
        "-filter_complex",
        ";".join(filters),
        "-map",
        f"[{previous}]",
        "-map",
        f"{sound_index}:a",
        *encode(19),
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "160k",
        "-movflags",
        "+faststart",
        "-t",
        timeline["total"],
        output,
    ]
)
ffmpeg(
    [
        "-ss",
        timeline["total"] - 7,
        "-i",
        output,
        "-frames:v",
        "1",
        "-q:v",
        "2",
        args.out / "poster.jpg",
    ]
)
(args.out / "render-report.json").write_text(
    json.dumps(
        {
            "seconds": perf_counter() - started,
            "frames_per_second": args.fps,
            "workers": args.workers,
            "encoder": args.encoder,
            "source_hash": source_hash,
            "output": str(output),
        },
        indent=2,
    )
)
print(f"Full film: {output} ({perf_counter() - started:.1f}s)", flush=True)
