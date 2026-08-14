#!/usr/bin/env python3
"""Check generated diagrams against the canonical layout.

Usage: check-layout.py '/tmp/diagrams/pr-*.svg'

Sessions run independently and cannot see each other's output, so consistency
across a dozen diagrams is not something to eyeball. Prints a table plus a
failure tally; exit status is informational.
"""
import xml.etree.ElementTree as ET, glob, os, re, sys

NS='{http://www.w3.org/2000/svg}'
def check(f):
    pr = re.search(r'pr-(\d+)', f).group(1)
    r = ET.parse(f).getroot()
    kids = list(r)
    out = {}
    out['viewBox'] = r.get('viewBox') == '0 0 1200 720'
    # <defs>/<title>/<desc>/<style> paint nothing, so a background rect that
    # follows them is still behind every visible element.
    NONRENDER = {NS+'defs', NS+'title', NS+'desc', NS+'style', NS+'metadata'}
    rest = [k for k in kids if k.tag not in NONRENDER]
    bg = rest[0] if rest else None
    out['bg_first'] = (bg is not None and bg.tag==NS+'rect'
                       and (bg.get('fill') or '').lower()=='#1a1b26'
                       and bg.get('width')=='1200')
    ys = lambda v: [e for e in r.iter(NS+'text') if e.get('y')==v]
    out['head_y36'] = bool(ys('36'))
    out['sub_y66'] = bool(ys('66'))
    heads = ys('36')
    txt = ''.join(heads[0].itertext()) if heads else ''
    out['head_fmt'] = txt.startswith(f'PR #{pr}') and '·' in txt
    out['rule_y96'] = any(e.get('y1')=='96' and e.get('y2')=='96' for e in r.iter(NS+'line'))
    div = [e for e in r.iter(NS+'line') if e.get('x1')=='600' and e.get('x2')=='600']
    out['divider'] = (not div) or any(e.get('stroke-dasharray') for e in div)
    # deepest drawn y
    maxy=0
    for e in r.iter():
        for a in ('y','y1','y2','cy'):
            v=e.get(a)
            if v:
                try:
                    n=float(v)
                    if n<=760: maxy=max(maxy,n)
                except ValueError: pass
        if e.tag==NS+'rect' and e.get('y') and e.get('height') and e.get('fill','').lower()!='#1a1b26':
            try: maxy=max(maxy,float(e.get('y'))+float(e.get('height')))
            except ValueError: pass
    out['fills_band'] = maxy >= 600
    ids=[e.get('id') for e in r.iter() if e.get('id')]
    out['ids_prefixed'] = all(i.startswith(f'pr{pr}') for i in ids) if ids else True
    return pr, out, round(maxy)

files = sorted(glob.glob(sys.argv[1]))
keys = ['viewBox','bg_first','head_y36','sub_y66','head_fmt','rule_y96','divider','fills_band','ids_prefixed']
print(f"{'PR':6}" + ''.join(f'{k[:9]:>11}' for k in keys) + '   maxY')
fails={k:0 for k in keys}
for f in files:
    pr,o,my = check(f)
    print(f"{pr:6}" + ''.join(f"{'ok' if o[k] else 'FAIL':>11}" for k in keys) + f"   {my}")
    for k in keys:
        if not o[k]: fails[k]+=1
print(f"\n{len(files)} file(s). failures:", {k:v for k,v in fails.items() if v} or 'none')
