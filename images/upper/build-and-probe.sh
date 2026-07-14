#!/usr/bin/env bash
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
#
# Build the official Upper image locally and run the black-box capability probe
# with ENTRYPOINT cleared. Never pushes to a registry.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${ROOT}/../.." && pwd)"

IMAGE_NAME="${UPPER_IMAGE_NAME:-lattice-upper:local}"
OCI_SOURCE="${OCI_SOURCE:-https://codeberg.org/AstRin/LATTICE}"
OCI_REVISION="${OCI_REVISION:-$(git -C "$REPO_ROOT" rev-parse HEAD)}"
OCI_VERSION="${OCI_VERSION:-dev-$(git -C "$REPO_ROOT" rev-parse --short HEAD)}"
PLATFORM="${UPPER_IMAGE_PLATFORM:-linux/amd64}"
BASE_REF="$(tr -d '[:space:]' <"${ROOT}/base-reference.txt")"

ENGINE="${CONTAINER_ENGINE:-}"
if [[ -z "$ENGINE" ]]; then
  if command -v docker >/dev/null 2>&1; then
    ENGINE=docker
  elif command -v podman >/dev/null 2>&1; then
    ENGINE=podman
  else
    echo "build-and-probe: need docker or podman" >&2
    exit 1
  fi
fi

"${ROOT}/verify-inputs.sh"

echo "build-and-probe: building ${IMAGE_NAME} (${PLATFORM}) from ${BASE_REF}"
"$ENGINE" build \
  --platform "$PLATFORM" \
  --build-arg "DEBIAN_BASE=${BASE_REF}" \
  --build-arg "OCI_SOURCE=${OCI_SOURCE}" \
  --build-arg "OCI_REVISION=${OCI_REVISION}" \
  --build-arg "OCI_VERSION=${OCI_VERSION}" \
  -f "${ROOT}/Containerfile" \
  -t "$IMAGE_NAME" \
  "$ROOT"

echo "build-and-probe: assert image Entrypoint is empty"
ENTRYPOINT_JSON="$("$ENGINE" image inspect --format '{{json .Config.Entrypoint}}' "$IMAGE_NAME")"
case "$ENTRYPOINT_JSON" in
  null|'[]'|'')
    ;;
  *)
    echo "build-and-probe: image Entrypoint must be empty, got: ${ENTRYPOINT_JSON}" >&2
    exit 1
    ;;
esac

echo "build-and-probe: running capability probe (entrypoint cleared)"
"$ENGINE" run --rm \
  --platform "$PLATFORM" \
  --entrypoint "" \
  -v "${ROOT}/probe.sh:/probe.sh:ro" \
  "$IMAGE_NAME" \
  /bin/sh /probe.sh

echo "build-and-probe: PASS (${IMAGE_NAME})"
