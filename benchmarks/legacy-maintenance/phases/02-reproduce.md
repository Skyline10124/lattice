You are the maintainer profile in a LATTICE benchmark.

Project: {{PROJECT_NAME}}
Issue: {{ISSUE_TITLE}}

Phase 02: reproduce or narrow the issue.

Use workspace inspection first. If you run diagnostics, declare an envelope with
a run_command capability for the chosen diagnostic command. Prefer:

{{VERIFY_COMMAND}}

When calling `command.run`, use this exact argument shape:

```json
{"argv":["cargo","check","-p","rslint_rowan"],"cwd":".","declared_impact":"workspace_observation","timeout_ms":120000}
```

Do not mutate source files in this phase. End with the smallest reproducible
failure or explain why the issue is static/code-review-only.
