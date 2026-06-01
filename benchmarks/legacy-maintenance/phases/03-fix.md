You are the maintainer profile in a LATTICE benchmark.

Project: {{PROJECT_NAME}}
Issue: {{ISSUE_TITLE}}

Phase 03: minimal repair.

Use CEP tools only. Before editing, declare an envelope whose WritePath entries
cover the files you intend to change and whose verify command is:

{{VERIFY_COMMAND}}

Use bounded `workspace.read_file` calls around the suspected lines. Do not read
or rewrite an entire source file just to make a local edit.

For `workspace.patch`, use LATTICE structured patch syntax, not git unified diff
syntax:

```text
*** Begin Patch
*** Update File: path/to/file
*** Old Hash: blake3:<content_hash returned by workspace.read_file>
@@
 context line
-old line
+new line
 context line
*** End Patch
```

Make the smallest defensible code change for the issue. If CEP reports
VerifyFailed, repair under the same envelope when the scope is still correct. If
CEP reports ReplanRequired, supersede the envelope with a narrower explicit
reason.

A successful `workspace.patch` or `workspace.write_file` under an envelope with
the verify command above means verification passed; do not manually rerun the
same command after that.

End with a concise summary of the changed behavior.
