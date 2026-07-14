#!/usr/bin/env bash
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
#
# Maintainer publish path for codeberg.org/astrin/lattice-upper.
#
# Stages:
#   1. Confirm checkout matches source_revision
#   2. Build + probe (linux/amd64)
#   3. Push candidate tag unique to the revision
#   4. Inspect manifest (amd64-only, sha256 digests)
#   5. Anonymous pull + probe by manifest digest
#   6. Optional: promote explicit version tag (never latest)
#   7. Emit provenance JSON + suggested lock.json body
#
# This script never commits lock.json. Maintainer checkpoint is a separate
# reviewed repository change after anonymous verification succeeds.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${ROOT}/../.." && pwd)"

PACKAGE="codeberg.org/astrin/lattice-upper"
REGISTRY_HOST="codeberg.org"
PLATFORM="linux/amd64"
PROMOTE=0
SOURCE_REVISION=""
VERSION=""

usage() {
  cat <<'EOF'
Usage: images/upper/publish.sh --source-revision <full-sha> --version <version> [--promote]

Environment:
  CONTAINER_ENGINE   docker|podman (auto-detected)
  CODEBERG_USER      registry username (required for push)
  CODEBERG_TOKEN     package write token (required for push)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source-revision)
      SOURCE_REVISION="${2:-}"
      shift 2
      ;;
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --promote)
      PROMOTE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[[ -n "$SOURCE_REVISION" ]] || { echo "missing --source-revision" >&2; exit 2; }
[[ -n "$VERSION" ]] || { echo "missing --version" >&2; exit 2; }
[[ "$VERSION" != "latest" ]] || { echo "version must not be 'latest'" >&2; exit 2; }
[[ "$SOURCE_REVISION" =~ ^[0-9a-f]{40}$ ]] || {
  echo "source-revision must be a full 40-char lowercase hex SHA" >&2
  exit 2
}

HEAD_SHA="$(git -C "$REPO_ROOT" rev-parse HEAD)"
if [[ "$HEAD_SHA" != "$SOURCE_REVISION" ]]; then
  echo "checkout HEAD (${HEAD_SHA}) != source-revision (${SOURCE_REVISION})" >&2
  exit 1
fi

ENGINE="${CONTAINER_ENGINE:-}"
if [[ -z "$ENGINE" ]]; then
  if command -v docker >/dev/null 2>&1; then
    ENGINE=docker
  elif command -v podman >/dev/null 2>&1; then
    ENGINE=podman
  else
    echo "need docker or podman" >&2
    exit 1
  fi
fi

: "${CODEBERG_USER:?CODEBERG_USER required}"
: "${CODEBERG_TOKEN:?CODEBERG_TOKEN required}"

BASE_REF="$(tr -d '[:space:]' <"${ROOT}/base-reference.txt")"
CANDIDATE_TAG="candidate-${SOURCE_REVISION}"
LOCAL_TAG="lattice-upper:publish-${SOURCE_REVISION}"
REMOTE_CANDIDATE="${PACKAGE}:${CANDIDATE_TAG}"
ARTIFACT_DIR="${REPO_ROOT}/build/tmp/upper-publish-${SOURCE_REVISION}"
mkdir -p "$ARTIFACT_DIR"

export UPPER_IMAGE_NAME="$LOCAL_TAG"
export OCI_SOURCE="https://codeberg.org/AstRin/LATTICE"
export OCI_REVISION="$SOURCE_REVISION"
export OCI_VERSION="$VERSION"
export UPPER_IMAGE_PLATFORM="$PLATFORM"
export CONTAINER_ENGINE="$ENGINE"

echo "publish: build + probe"
"${ROOT}/build-and-probe.sh"

echo "publish: registry login"
echo "$CODEBERG_TOKEN" | "$ENGINE" login "$REGISTRY_HOST" -u "$CODEBERG_USER" --password-stdin

echo "publish: tag + push candidate ${REMOTE_CANDIDATE}"
"$ENGINE" tag "$LOCAL_TAG" "$REMOTE_CANDIDATE"
"$ENGINE" push "$REMOTE_CANDIDATE"

echo "publish: inspect candidate manifest"
INSPECT_JSON="${ARTIFACT_DIR}/candidate-inspect.json"
if "$ENGINE" buildx imagetools inspect "$REMOTE_CANDIDATE" --raw >"${ARTIFACT_DIR}/manifest.raw" 2>/dev/null; then
  :
fi
# Prefer crane-less inspection via docker/podman image inspect after pull by tag.
"$ENGINE" pull --platform "$PLATFORM" "$REMOTE_CANDIDATE"
"$ENGINE" image inspect "$REMOTE_CANDIDATE" >"$INSPECT_JSON"

# Resolve digests with imagetools when available; fall back to RepoDigests.
PLATFORM_DIGEST=""
MANIFEST_DIGEST=""
if "$ENGINE" buildx imagetools inspect "$REMOTE_CANDIDATE" >"${ARTIFACT_DIR}/imagetools.txt" 2>/dev/null; then
  MANIFEST_DIGEST="$(
    sed -n 's/^Digest:[[:space:]]*//p' "${ARTIFACT_DIR}/imagetools.txt" | head -n1 | tr -d '[:space:]'
  )"
  PLATFORM_DIGEST="$(
    awk '
      /Platform:[[:space:]]*linux\/amd64/ { want=1; next }
      want && /Digest:[[:space:]]*/ {
        sub(/^.*Digest:[[:space:]]*/, "", $0)
        gsub(/[[:space:]]/, "", $0)
        print
        exit
      }
    ' "${ARTIFACT_DIR}/imagetools.txt"
  )"
fi

if [[ -z "$MANIFEST_DIGEST" ]]; then
  MANIFEST_DIGEST="$(
    python3 - "$INSPECT_JSON" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
for entry in data:
    for d in entry.get("RepoDigests") or []:
        if "@" in d:
            print(d.split("@", 1)[1])
            raise SystemExit(0)
raise SystemExit("no RepoDigests found")
PY
  )"
fi

[[ "$MANIFEST_DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || {
  echo "manifest digest must be sha256:<64-hex>, got: ${MANIFEST_DIGEST}" >&2
  exit 1
}

if [[ -z "$PLATFORM_DIGEST" ]]; then
  PLATFORM_DIGEST="$MANIFEST_DIGEST"
fi
[[ "$PLATFORM_DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || {
  echo "platform digest must be sha256:<64-hex>, got: ${PLATFORM_DIGEST}" >&2
  exit 1
}

DIGEST_REF="${PACKAGE}@${MANIFEST_DIGEST}"
echo "publish: manifest digest ${MANIFEST_DIGEST}"
echo "publish: platform digest ${PLATFORM_DIGEST}"

echo "publish: clear registry credentials and local image cache for anonymous verify"
"$ENGINE" logout "$REGISTRY_HOST" >/dev/null 2>&1 || true
# Drop host-side auth for this registry when docker config is present.
if [[ -f "${HOME}/.docker/config.json" ]]; then
  python3 - "$REGISTRY_HOST" <<'PY' || true
import json, os, sys
host = sys.argv[1]
path = os.path.expanduser("~/.docker/config.json")
with open(path, encoding="utf-8") as f:
    cfg = json.load(f)
auths = cfg.get("auths") or {}
changed = False
for key in list(auths):
    if key == host or key.endswith("/" + host) or key.startswith(host):
        del auths[key]
        changed = True
if changed:
    cfg["auths"] = auths
    with open(path, "w", encoding="utf-8") as f:
        json.dump(cfg, f, indent=2)
        f.write("\n")
PY
fi
# Force a registry fetch: remove local tags/digests so pull cannot be a no-op cache hit.
"$ENGINE" rmi "$LOCAL_TAG" >/dev/null 2>&1 || true
"$ENGINE" rmi "$REMOTE_CANDIDATE" >/dev/null 2>&1 || true
"$ENGINE" rmi "$DIGEST_REF" >/dev/null 2>&1 || true

echo "publish: anonymous pull + probe ${DIGEST_REF}"
"$ENGINE" pull --platform "$PLATFORM" "$DIGEST_REF"
"$ENGINE" run --rm \
  --platform "$PLATFORM" \
  --entrypoint "" \
  -v "${ROOT}/probe.sh:/probe.sh:ro" \
  "$DIGEST_REF" \
  /bin/sh /probe.sh
ANON_OK=true

if [[ "$PROMOTE" -eq 1 ]]; then
  echo "publish: promote version tag ${PACKAGE}:${VERSION}"
  echo "$CODEBERG_TOKEN" | "$ENGINE" login "$REGISTRY_HOST" -u "$CODEBERG_USER" --password-stdin
  VERSION_REF="${PACKAGE}:${VERSION}"
  "$ENGINE" tag "$DIGEST_REF" "$VERSION_REF"
  "$ENGINE" push "$VERSION_REF"
  # Re-read version tag and require same digest.
  "$ENGINE" pull --platform "$PLATFORM" "$VERSION_REF"
  PROMOTED_DIGEST="$(
    "$ENGINE" image inspect "$VERSION_REF" | python3 -c '
import json,sys
data=json.load(sys.stdin)
for entry in data:
  for d in entry.get("RepoDigests") or []:
    if "@" in d:
      print(d.split("@",1)[1]); raise SystemExit
raise SystemExit("no digest")
'
  )"
  if [[ "$PROMOTED_DIGEST" != "$MANIFEST_DIGEST" ]]; then
    echo "promoted version tag resolved to ${PROMOTED_DIGEST}, expected ${MANIFEST_DIGEST}" >&2
    exit 1
  fi
  "$ENGINE" logout "$REGISTRY_HOST" >/dev/null 2>&1 || true
fi

PROVENANCE="${ARTIFACT_DIR}/provenance.json"
python3 - "$PROVENANCE" <<PY
import json
prov = {
    "source_revision": "${SOURCE_REVISION}",
    "version": "${VERSION}",
    "base_reference": "${BASE_REF}",
    "manifest_digest": "${MANIFEST_DIGEST}",
    "platforms": {"linux/amd64": "${PLATFORM_DIGEST}"},
    "capability_artifacts": ["probe.sh PASS"],
    "anonymous_pull_verified": ${ANON_OK},
}
with open("${PROVENANCE}", "w", encoding="utf-8") as f:
    json.dump(prov, f, indent=2)
    f.write("\n")
print(json.dumps(prov, indent=2))
PY

LOCK_SUGGESTED="${ARTIFACT_DIR}/lock.suggested.json"
python3 - "$LOCK_SUGGESTED" <<PY
import json
lock = {
    "schema_version": 1,
    "reference": "${DIGEST_REF}",
}
with open("${LOCK_SUGGESTED}", "w", encoding="utf-8") as f:
    json.dump(lock, f, indent=2)
    f.write("\n")
print("suggested lock.json:")
print(json.dumps(lock, indent=2))
PY

echo
echo "publish: SUCCESS"
echo "  candidate:   ${REMOTE_CANDIDATE}"
echo "  digest ref:  ${DIGEST_REF}"
echo "  provenance:  ${PROVENANCE}"
echo "  lock draft:  ${LOCK_SUGGESTED}"
echo "Maintainer checkpoint: review provenance, then commit images/upper/lock.json"
echo "from the lock draft. Do not use latest. Publish job does not write the repo."
