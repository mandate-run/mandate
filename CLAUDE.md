# Git

Conventional commit subject, 50 characters max. Body: up to 3 bullets, 50 characters each, not capitalized. Run `git config core.hooksPath .githooks` once per clone; the commit-msg hook enforces this.

# Discussion

Before working, read open issues with `gh issue list` and the files in `docs/decisions/`. Discuss in the issue; the outcome goes to `docs/spec.md` when it changes behavior, otherwise to `docs/decisions/`.

# Pull requests

One branch and one pull request per issue, branch `N-slug`. The body starts with `Closes #N`, or `Part of #N` when the issue stays open for evidence. Rebase merge only, when CI is green. A PR that touches `docs/spec.md` or `docs/decisions/` waits for the other person's approval.
