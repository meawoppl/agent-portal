#!/usr/bin/env bash
# encode.sh <frames-dir> <out-basename> [fps] [width]
set -euo pipefail
DIR="$1"; OUT="$2"; FPS="${3:-12}"; W="${4:-900}"
ffmpeg -y -loglevel error -framerate "$FPS" -i "$DIR/f%04d.png" \
  -vf "scale=$W:-2:flags=lanczos" -plays 0 -f apng "$OUT.png"
ffmpeg -y -loglevel error -framerate "$FPS" -i "$DIR/f%04d.png" \
  -vf "scale=$W:-2:flags=lanczos" -c:v libwebp_anim -lossless 0 -q:v 62 -loop 0 "$OUT.webp"
ls -lh "$OUT.png" "$OUT.webp" | awk '{print $9, $5}'
