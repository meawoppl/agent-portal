#!/usr/bin/env python3
"""composite.py <desk-dir> <phone-dir> <out-dir>
Lay the desktop and phone captures side by side on one canvas, same timeline."""
import sys, os, glob, shutil
from PIL import Image, ImageDraw

desk_dir, phone_dir, out_dir = sys.argv[1:4]
W, H, BG = 1800, 1180, (13, 14, 20)
desks = sorted(glob.glob(os.path.join(desk_dir, 'f*.png')))
phones = sorted(glob.glob(os.path.join(phone_dir, 'f*.png')))
shutil.rmtree(out_dir, ignore_errors=True); os.makedirs(out_dir)

def rounded(im, r):
    mask = Image.new('L', im.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, im.size[0] - 1, im.size[1] - 1], r, fill=255)
    out = Image.new('RGBA', im.size, (0, 0, 0, 0))
    out.paste(im, (0, 0), mask)
    return out

for i, (d, p) in enumerate(zip(desks, phones)):
    canvas = Image.new('RGB', (W, H), BG)
    dim = Image.open(d).convert('RGB')
    dim = dim.resize((1240, round(1240 * dim.size[1] / dim.size[0])), Image.LANCZOS)
    pim = Image.open(p).convert('RGB')
    pim = pim.resize((470, round(470 * pim.size[1] / pim.size[0])), Image.LANCZOS)
    canvas.paste(rounded(dim, 14), (30, (H - dim.size[1]) // 2), rounded(dim, 14))
    canvas.paste(rounded(pim, 26), (1300, (H - pim.size[1]) // 2), rounded(pim, 26))
    canvas.save(os.path.join(out_dir, f'f{i:04d}.png'))
print(f'{min(len(desks), len(phones))} composited frames')
