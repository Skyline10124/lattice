#!/bin/sh
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
#
# Black-box capability probe for the official Upper OCI image.
# Must be executed inside the final image with ENTRYPOINT cleared.
# Success may only come from the final rootfs itself — no registry
# credentials, project source mounts, or network package installs.

set -eu

echo "upper-image probe: checking required tools"
for tool in sh git base64 mkdir stat dd cat wc printf cmp; do
  command -v "$tool" >/dev/null
done

echo "upper-image probe: workspace + base64 binary write"
mkdir -p /workspace /tmp/lattice-probe
printf '%s' 'hello-lattice-probe' | base64 | base64 -d >/workspace/input.bin
test "$(cat /workspace/input.bin)" = "hello-lattice-probe"
# chunked write path also used by baseline transfer helpers
printf '%s' 'chunk-a' | base64 | base64 -d >/tmp/lattice-probe/chunk.bin
printf '%s' 'chunk-b' | base64 | base64 -d >>/tmp/lattice-probe/chunk.bin
test "$(cat /tmp/lattice-probe/chunk.bin)" = "chunk-achunk-b"

echo "upper-image probe: git pack ingest / ref / checkout"
export GIT_AUTHOR_NAME=probe
export GIT_AUTHOR_EMAIL=probe@lattice.local
export GIT_COMMITTER_NAME=probe
export GIT_COMMITTER_EMAIL=probe@lattice.local

git init --initial-branch=main /tmp/lattice-probe/source >/dev/null
printf '%s\n' 'baseline' >/tmp/lattice-probe/source/README
git -C /tmp/lattice-probe/source add README
git -C /tmp/lattice-probe/source \
  -c user.name=probe \
  -c user.email=probe@lattice.local \
  commit -m baseline >/dev/null
EXPECTED_OID=$(git -C /tmp/lattice-probe/source rev-parse HEAD)
git -C /tmp/lattice-probe/source rev-list --objects --all \
  | git -C /tmp/lattice-probe/source pack-objects --stdout \
  >/tmp/lattice-probe/baseline.pack

git init --initial-branch=main /workspace >/dev/null
git -C /workspace index-pack --stdin </tmp/lattice-probe/baseline.pack >/dev/null
git -C /workspace update-ref refs/heads/lattice-upper "$EXPECTED_OID"
git -C /workspace symbolic-ref HEAD refs/heads/lattice-upper
git -C /workspace -c advice.detachedHead=false checkout -f "$EXPECTED_OID" >/dev/null
test "$(git -C /workspace rev-parse HEAD)" = "$EXPECTED_OID"
test -f /workspace/README
test "$(cat /workspace/README)" = "baseline"

echo "upper-image probe: bundle / stat / dd / base64 round-trip"
git -C /workspace bundle create /tmp/lattice-probe/candidate.bundle HEAD >/dev/null
SIZE=$(stat -c %s /tmp/lattice-probe/candidate.bundle)
test "$SIZE" -gt 0
dd if=/tmp/lattice-probe/candidate.bundle bs=65536 2>/dev/null \
  | base64 >/tmp/lattice-probe/export.b64
base64 -d </tmp/lattice-probe/export.b64 >/tmp/lattice-probe/roundtrip.bundle
cmp /tmp/lattice-probe/candidate.bundle /tmp/lattice-probe/roundtrip.bundle

echo "upper-image capability probe: PASS"
