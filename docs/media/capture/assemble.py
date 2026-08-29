#!/usr/bin/env python3
"""assemble.py <src-dir> <dst-dir> a:b [c:d ...]
Concatenate frame ranges into one sequence, so a clip can skip dead air."""
import sys, os, shutil, glob
src, dst, ranges = sys.argv[1], sys.argv[2], sys.argv[3:]
frames = sorted(glob.glob(os.path.join(src, 'f*.png')))
shutil.rmtree(dst, ignore_errors=True); os.makedirs(dst)
n = 0
for r in ranges:
    a, b = (int(x) for x in r.split(':'))
    for f in frames[a:b]:
        shutil.copy(f, os.path.join(dst, f'f{n:04d}.png')); n += 1
print(f'{n} frames from {len(ranges)} range(s)')
