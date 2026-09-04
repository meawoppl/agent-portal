# PR visualizations

Every pull request ships a visual before/after summary of itself, committed
here as `<pr-number>.svg` on the PR branch. The `000-` directory prefix sorts
this path first in GitHub's review file list, so the picture is the first
thing a reviewer sees when opening "Files changed".

- **Generating one**: the `visual-pr` skill
  ([.claude/skills/visual-pr/SKILL.md](../.claude/skills/visual-pr/SKILL.md))
  reads the PR's actual diff, renders the SVG in the house style, and
  validates it with `check_svg.py` — the same validator the CI check runs.
- **Enforcement**: the `Visual PR attached` workflow
  ([.github/workflows/visual-pr.yml](../.github/workflows/visual-pr.yml))
  fails a PR that lacks its SVG or whose SVG fails validation.
- **Opting out**: label a PR `no-visual` when a diagram is genuinely noise
  (dependency bumps, typo fixes). The job skips, which satisfies the check.

Merged SVGs accumulate here as a visual history of the repo's changes; they
are small (6–10 KB of text each). One caveat on ordering: dotfile paths
(`.claude/`, `.github/`) sort before `000-` in GitHub's list, so a PR touching
those shows them first.
