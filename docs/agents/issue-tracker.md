# Issue Tracker: Codeberg

Issues and PRDs for this repository live in Codeberg Issues at
`AstRin/LATTICE`. Use the connected Forgejo tools for all operations, with
`owner: AstRin` and `repo: LATTICE`.

## Conventions

- Create an issue with `forgejo_create_issue`.
- Read an issue with `forgejo_get_issue_by_index`; read its discussion with `forgejo_list_issue_comments`.
- List issues with `forgejo_list_repo_issues`, applying state and label filters as needed.
- Comment with `forgejo_create_issue_comment`.
- Apply labels with `forgejo_add_issue_labels`; remove labels with `forgejo_remove_issue_labels`.
- Close an issue with `forgejo_issue_state_change`, adding an explanatory comment first when appropriate.

## Pull Requests As A Triage Surface

External pull requests are not a triage surface. The `triage` skill processes
Codeberg issues only.

## Skill Vocabulary

When a skill says "publish to the issue tracker", create a Codeberg issue in
`AstRin/LATTICE`. When it says "fetch the relevant ticket", fetch the Codeberg
issue by index together with its comments and labels.
