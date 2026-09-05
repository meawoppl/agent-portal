# PR visualizations

Every pull request ships a visual before/after summary of itself, committed
here as `<pr-number>.svg` with the number zero-padded to six digits
(`001811.svg`). GitHub lists changed files in byte order of their paths, and
`.0` sorts ahead of `.claude/`, `.github/`, every other dotfile and every
letter, so the picture is the first thing a reviewer sees in "Files changed".
The fixed-width names keep the files in PR order. (Plain `ls` hides a
dot-directory; `ls -a` and GitHub's tree show it.)

- **Generating one**: follow the authoring spec in
  [meawoppl/visual-pr](https://github.com/meawoppl/visual-pr) (`SPEC.md`) —
  read the PR's actual diff, render the SVG in the house style, and validate
  with its `check_svg.py --style .github/visual-pr/style.json` — the same
  validator the CI check runs. The check's failure output carries the full
  recipe, including how to fetch the validator.
- **Enforcement**: the `Visual PR attached` workflow
  ([.github/workflows/visual-pr.yml](../.github/workflows/visual-pr.yml))
  wires up the reusable [meawoppl/visual-pr](https://github.com/meawoppl/visual-pr)
  action, which fails a PR that lacks its SVG or whose SVG fails validation
  (off-palette color, wrong canvas, characters the font stack would render as
  boxes) and warns on text that overruns its box or the canvas.
- **In the description**: the PR body opens with the image, pinned to the
  commit SHA — `![Visual summary](https://raw.githubusercontent.com/meawoppl/agent-portal/<sha>/.0-pr-viz/<n>.svg)`.
  The check (`body-image: first`, the v2 default) verifies it; editing the
  description re-runs the check.
- **Opting out**: label a PR `no-visual` when a diagram is genuinely noise
  (dependency bumps, typo fixes). The job skips, which satisfies the check.

Merged SVGs accumulate here as a visual history of the repo's changes; they
are small (6–10 KB of text each).
