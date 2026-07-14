# Official Upper OCI image

Package identity: `codeberg.org/astrin/lattice-upper`

Runtime identity (after maintainer checkpoint): the immutable digest reference
in `lock.json`:

```json
{
  "schema_version": 1,
  "reference": "codeberg.org/astrin/lattice-upper@sha256:<64-hex>"
}
```

Until the first successful publish + anonymous verify, `lock.json` **must not
exist**. There is no placeholder digest.

## What this image contains

Pinned Debian bookworm-slim base (`base-reference.txt`), plus:

- `git`
- `ca-certificates`
- POSIX `/bin/sh` (`dash`)
- baseline transfer tools from `coreutils` (`base64`, `mkdir`, `stat`, `dd`,
  `cat`, `wc`, …)

No Python/Node/Rust toolchains, no LATTICE binary, no `ENTRYPOINT`, no runtime
package install.

## Local build + black-box probe

```sh
images/upper/verify-inputs.sh
images/upper/build-and-probe.sh
```

`build-and-probe.sh` builds `linux/amd64`, runs `probe.sh` with entrypoint
cleared, and never logs into or pushes a registry.

## CI

`.github/workflows/upper-image.yml`:

| Event | Behavior |
|---|---|
| pull_request / push (paths under `images/upper/**`) | build + probe only, read-only permissions, no registry login/push |
| `workflow_dispatch` with `source_revision` + `version` | publish candidate, verify manifest, anonymous probe, optional version promotion |

Publish requires repository secrets:

- `CODEBERG_PACKAGE_USER`
- `CODEBERG_PACKAGE_TOKEN` (package write only)

## Maintainer checkpoint (first release)

1. Checkout the exact revision to publish.
2. Run:

   ```sh
   CODEBERG_USER=... CODEBERG_TOKEN=... \
     images/upper/publish.sh \
       --source-revision <full-sha> \
       --version <explicit-version> \
       --promote
   ```

3. Confirm anonymous pull of `codeberg.org/astrin/lattice-upper@sha256:…` and
   probe PASS in the script output / `build/tmp/upper-publish-<sha>/provenance.json`.
4. Copy `build/tmp/upper-publish-<sha>/lock.suggested.json` to
   `images/upper/lock.json` in a normal reviewable commit.
5. Never create or promote a `latest` tag as the runtime identity.

`publish.sh` does **not** commit the lock. Failure at any stage (build, probe,
push, manifest check, anonymous verify, version re-read) aborts without
suggesting a promotable lock.

## Relation to later work

- Issue #30 embeds/parses `lock.json` as the only runtime image input.
- Real libkrun/KVM validation is #33/#34; this directory proves OCI capability
  and public fetchability of the product rootfs.
