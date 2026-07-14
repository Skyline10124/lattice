#!/usr/bin/env bash
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
#
# Static contract checks for version-controlled Upper image inputs.
# Does not replace the black-box probe; only guards pin/lock shape.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_REF_FILE="${ROOT}/base-reference.txt"
CONTAINERFILE="${ROOT}/Containerfile"
PROBE="${ROOT}/probe.sh"
LOCK="${ROOT}/lock.json"

fail() {
  echo "verify-inputs: FAIL: $*" >&2
  exit 1
}

pass() {
  echo "verify-inputs: $*"
}

[[ -f "$BASE_REF_FILE" ]] || fail "missing base-reference.txt"
[[ -f "$CONTAINERFILE" ]] || fail "missing Containerfile"
[[ -f "$PROBE" ]] || fail "missing probe.sh"
[[ -x "$PROBE" ]] || fail "probe.sh must be executable"

BASE_REF="$(tr -d '[:space:]' <"$BASE_REF_FILE")"
[[ -n "$BASE_REF" ]] || fail "base-reference.txt is empty"

# Expect name:tag@sha256:<64-hex>
if [[ ! "$BASE_REF" =~ ^[a-z0-9._/-]+:[a-zA-Z0-9._-]+@sha256:[0-9a-f]{64}$ ]]; then
  fail "base-reference must be name:tag@sha256:<64-lowercase-hex>, got: ${BASE_REF}"
fi
pass "base-reference pin ok (${BASE_REF})"

if ! grep -q "FROM \${DEBIAN_BASE}" "$CONTAINERFILE"; then
  fail "Containerfile must FROM \${DEBIAN_BASE}"
fi

if ! grep -Fq "ARG DEBIAN_BASE=${BASE_REF}" "$CONTAINERFILE"; then
  fail "Containerfile DEBIAN_BASE default must equal base-reference.txt"
fi
pass "Containerfile DEBIAN_BASE matches base-reference.txt"

if grep -Eiq '^[[:space:]]*ENTRYPOINT[[:space:]]' "$CONTAINERFILE"; then
  fail "Containerfile must not declare ENTRYPOINT"
fi
pass "no ENTRYPOINT"

if ! grep -Eq 'org\.opencontainers\.image\.source' "$CONTAINERFILE"; then
  fail "Containerfile must set org.opencontainers.image.source"
fi
if ! grep -Eq 'org\.opencontainers\.image\.revision' "$CONTAINERFILE"; then
  fail "Containerfile must set org.opencontainers.image.revision"
fi
if ! grep -Eq 'org\.opencontainers\.image\.version' "$CONTAINERFILE"; then
  fail "Containerfile must set org.opencontainers.image.version"
fi
pass "OCI source/revision/version labels present"

# Lock is optional until maintainer checkpoint. When present, enforce minimal schema.
if [[ -f "$LOCK" ]]; then
  command -v python3 >/dev/null || fail "python3 required to validate lock.json"
  python3 - "$LOCK" <<'PY'
import json, re, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as f:
    data = json.load(f)
if set(data.keys()) != {"schema_version", "reference"}:
    raise SystemExit(f"lock keys must be exactly schema_version+reference, got {sorted(data)}")
if data.get("schema_version") != 1:
    raise SystemExit(f"schema_version must be 1, got {data.get('schema_version')!r}")
ref = data.get("reference")
pat = re.compile(r"^codeberg\.org/astrin/lattice-upper@sha256:[0-9a-f]{64}$")
if not isinstance(ref, str) or not pat.match(ref):
    raise SystemExit(f"reference must be codeberg.org/astrin/lattice-upper@sha256:<64-hex>, got {ref!r}")
print("verify-inputs: lock.json schema ok")
PY
else
  pass "lock.json absent (expected until maintainer checkpoint)"
fi

pass "all static input checks passed"
