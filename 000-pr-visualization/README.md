# PR visualizations

Every pull request ships a visual before/after summary of itself, committed
here as `<pr-number>.svg` on the PR branch. The `000-` directory prefix sorts
this path first in GitHub's review file list, so the picture is the first
thing a reviewer sees when opening "Files changed".

- **Generating one**: follow the authoring spec in
  [meawoppl/visual-pr](https://github.com/meawoppl/visual-pr) (`SPEC.md`) —
  read the PR's actual diff, render the SVG in the house style, and validate
  with its `check_svg.py` against
  [.github/visual-pr/style.json](../.github/visual-pr/style.json) — the same
  validator the CI check runs. The check's failure output carries the full
  recipe.
- **Enforcement**: the `Visual PR attached` workflow
  ([.github/workflows/visual-pr.yml](../.github/workflows/visual-pr.yml))
  wires up the reusable [meawoppl/visual-pr](https://github.com/meawoppl/visual-pr)
  action, which fails a PR that lacks its SVG or whose SVG fails validation —
  and on failure emits the full authoring spec, this repo's style JSON
  ([.github/visual-pr/style.json](../.github/visual-pr/style.json)), and the
  exact local check to run.
- **Opting out**: label a PR `no-visual` when a diagram is genuinely noise
  (dependency bumps, typo fixes). The job skips, which satisfies the check.

Merged SVGs accumulate here as a visual history of the repo's changes; they
are small (6–10 KB of text each). One caveat on ordering: dotfile paths
(`.claude/`, `.github/`) sort before `000-` in GitHub's list, so a PR touching
those shows them first.
