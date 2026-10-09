"""Mix original facility music and voice against the browser-exported film clock.

Usage: python mix-aperture-score.py ASSET_DIR TIMELINE_JSON OUTPUT.mp3
Requires numpy, scipy, soundfile and ffmpeg. Exactly one full-length soundtrack.
"""

import json
from pathlib import Path
import subprocess
import sys
import numpy as np
import soundfile as sf
from scipy.signal import butter, sosfilt

assets, timeline_file, output = map(Path, sys.argv[1:4])
timeline = json.loads(timeline_file.read_text())
lines = {
    line["id"]: line
    for line in json.loads((assets / "lines.json").read_text())["lines"]
}
sr = 44100
length = int(np.ceil(timeline["total"] * sr))
# Float32 keeps the long-film mix small enough for ordinary development hosts.
bed = np.zeros((length, 2), dtype=np.float32)
voice = np.zeros(length, dtype=np.float32)
rng = np.random.default_rng(42)


def add(target, start, sound):
    at = max(0, int(start * sr))
    count = min(len(sound), len(target) - at)
    if count > 0:
        target[at : at + count] += sound[:count]


def hz(midi):
    return 440 * 2 ** ((midi - 69) / 12)


# Original restrained synth bed; dialogue remains the foreground.
chords = [
    (57, 60, 64),
    (53, 57, 60),
    (48, 55, 64),
    (55, 59, 62),
    (57, 60, 64),
    (53, 57, 60),
    (50, 57, 65),
    (52, 56, 59),
]
for i, start in enumerate(np.arange(3, timeline["total"], 6)):
    t = np.arange(int(7.5 * sr)) / sr
    pad = sum(
        np.sin(2 * np.pi * hz(note) * t + 0.012 * np.sin(2 * np.pi * 0.2 * t))
        for note in chords[i % len(chords)]
    )
    pad += 0.4 * np.sin(2 * np.pi * hz(chords[i % 8][0] - 12) * t)
    env = np.minimum(t / 1.5, 1) * np.minimum((7.5 - t) / 1.8, 1)
    add(
        bed,
        start,
        np.stack((pad, np.roll(pad, 270)), axis=1).astype(np.float32)
        * env[:, None]
        * 0.035,
    )
for start in np.arange(8, timeline["total"] - 5, 60 / 96):
    t = np.arange(int(0.28 * sr)) / sr
    pulse = (
        np.sin(2 * np.pi * (45 + 70 * np.exp(-t * 30)) * t) * np.exp(-t * 14) * 0.035
    )
    add(bed, start, np.stack((pulse, pulse), axis=1).astype(np.float32))
for scene in timeline["scenes"][1:]:
    t = np.arange(int(0.7 * sr)) / sr
    noise = sosfilt(butter(2, 2200, fs=sr, output="sos"), rng.standard_normal(len(t)))
    sweep = (
        noise * 0.035 + np.sin(2 * np.pi * (160 * t + 300 * t * t)) * 0.02
    ) * np.sin(np.pi * t / 0.7) ** 2
    add(bed, scene["start"], np.stack((sweep, sweep), axis=1).astype(np.float32))
for key, start in timeline["cues"]:
    raw = subprocess.check_output(
        [
            "ffmpeg",
            "-v",
            "error",
            "-i",
            str(assets / lines[key]["file"]),
            "-f",
            "f32le",
            "-ac",
            "1",
            "-ar",
            str(sr),
            "-",
        ]
    )
    samples = np.frombuffer(raw, dtype=np.float32)
    add(voice, start, samples)
    a = max(0, int((start - 0.15) * sr))
    b = min(length, int((start + len(samples) / sr + 0.2) * sr))
    bed[a:b] *= 0.4
mix = bed + voice[:, None] * 0.94
fade = int(3 * sr)
mix[-fade:] *= np.linspace(1, 0, fade)[:, None]
peak = float(np.max(np.abs(mix)))
if peak > 0.95:
    mix *= 0.95 / peak
output.parent.mkdir(parents=True, exist_ok=True)
sf.write(output.with_suffix(".wav"), mix, sr)
subprocess.run(
    [
        "ffmpeg",
        "-v",
        "error",
        "-y",
        "-i",
        str(output.with_suffix(".wav")),
        "-b:a",
        "160k",
        str(output),
    ],
    check=True,
)
print(
    json.dumps(
        {
            "duration": timeline["total"],
            "lines": len(timeline["cues"]),
            "output": str(output),
        }
    )
)
