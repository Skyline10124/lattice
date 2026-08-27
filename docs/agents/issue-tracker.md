# Issue Tracker: GitHub

Issues and PRDs for this repository live in GitHub Issues at
`Skyline10124/lattice`. Use the connected GitHub tools when they have private-repository access; otherwise use the authenticated `gh` CLI.

## Conventions

- Create an issue with `github_create_issue` or `gh issue create`.
- Read an issue with `github_get_issue` or `gh issue view`; include its comments and labels when gathering requirements.
- List issues with `github_list_issues` or `gh issue list`, applying state and label filters as needed.
- Comment with `github_add_issue_comment` or `gh issue comment`.
- Apply labels or change state with `github_update_issue` or `gh issue edit` / `gh issue close`.
- Use `owner: Skyline10124` and `repo: lattice` with GitHub tools.

## Pull Requests As A Triage Surface

External pull requests are not an issue-triage surface. The `triage` skill processes GitHub issues only; use GitHub pull-request tools for PR work.

## Migrated Issue Numbers

The Codeberg tracker was migrated on 2026-07-24. Codeberg issue #1 reuses its original GitHub issue #23; Codeberg issues #2–#50 map to GitHub issues #24–#72 respectively (`github = codeberg + 22`). Active `docs/plans/` and `docs/specs/` filenames and references use GitHub numbers. Root `AGENTS.md` and all crate-local `crates/*/AGENTS.md` files must always use GitHub issue numbers and are explicitly excluded from the historical exemption surfaces. `FINDINGS.md`, labeled `FIX.md` historical headings, and commit messages may still mention Codeberg numbers — translate those with this mapping instead of treating them as same-number GitHub references.

## Skill Vocabulary

When a skill says "publish to the issue tracker", create a GitHub issue in `Skyline10124/lattice`. When it says "fetch the relevant ticket", fetch the mapped GitHub issue together with its comments and labels.
