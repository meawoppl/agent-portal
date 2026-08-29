#!/usr/bin/env python3
"""trim.py <src-frames-dir> <dst-dir> <start> <end> [--dedupe-hold N]
Copies frames [start, end) and optionally collapses runs of identical frames
to at most N repeats, so idle stretches don't eat the clip budget."""
import sys, os, shutil, hashlib, glob

src, dst, start, end = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
hold = None
if '--dedupe-hold' in sys.argv:
    hold = int(sys.argv[sys.argv.index('--dedupe-hold') + 1])
frames = sorted(glob.glob(os.path.join(src, 'f*.png')))[start:end]
shutil.rmtree(dst, ignore_errors=True)
os.makedirs(dst)
out, last, run = 0, None, 0
for f in frames:
    h = hashlib.md5(open(f, 'rb').read()).hexdigest()
    if h == last:
        run += 1
        if hold is not None and run > hold:
            continue
    else:
        run = 0
    last = h
    shutil.copy(f, os.path.join(dst, f'f{out:04d}.png'))
    out += 1
print(f'{len(frames)} -> {out} frames')
