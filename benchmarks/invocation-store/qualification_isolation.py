#!/usr/bin/env python3
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
"""Harness-only run-root allocation for InvocationStore qualification.

The runner owns `$QUALIFICATION_TMPDIR/runs/<group>/<nn>`. Measured tests
receive that path through environment variables and must not delete it.
Cleanup is a separate explicit step after every measured group and summary
generation. This module never inspects PASS/FAIL and never rewrites JSONL.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
from pathlib import Path

GROUP_RE = re.compile(r"^[A-Za-z0-9_]+$")
MAX_RUN = 20
RUNS_DIRNAME = "runs"

TEMP_ROOT_ENV = "LATTICE_INVOCATION_QUALIFICATION_TEMP_ROOT"
RUN_ROOT_ENV = "LATTICE_INVOCATION_QUALIFICATION_RUN_ROOT"


class IsolationError(Exception):
    """Malformed or colliding qualification run-root request."""


def require_absolute_dir(path: Path, label: str) -> Path:
    if not path.is_absolute():
        raise IsolationError(f"{label} must be an absolute path")
    if path.is_symlink():
        raise IsolationError(f"{label} must not be a symlink")
    if not path.is_dir():
        raise IsolationError(f"{label} must be an existing directory")
    return path.resolve()


def allocate_run_root(temp_root: Path, group: str, run: int) -> Path:
    temp_root = require_absolute_dir(temp_root, "qualification temp root")
    if not GROUP_RE.fullmatch(group):
        raise IsolationError(f"malformed qualification group name {group!r}")
    if not isinstance(run, int) or run < 1 or run > MAX_RUN:
        raise IsolationError(f"qualification run must be 1..{MAX_RUN}, got {run!r}")
    leaf = temp_root / RUNS_DIRNAME / group / f"{run:02d}"
    leaf.parent.mkdir(parents=True, exist_ok=True)
    try:
        leaf.mkdir()
    except FileExistsError as exc:
        raise IsolationError(f"qualification run root already exists: {leaf}") from exc
    if leaf.is_symlink():
        raise IsolationError(f"qualification run root must not be a symlink: {leaf}")
    resolved = leaf.resolve()
    if not resolved.is_relative_to(temp_root):
        raise IsolationError("qualification run root escaped the temp root")
    return resolved


def cleanup_retained_roots(temp_root: Path) -> None:
    temp_root = require_absolute_dir(temp_root, "qualification temp root")
    runs = temp_root / RUNS_DIRNAME
    if runs.exists():
        shutil.rmtree(runs)


def test_binary_from_cargo_json(raw: str) -> Path:
    found: list[Path] = []
    for line in raw.splitlines():
        if not line.strip():
            continue
        message = json.loads(line)
        target = message.get("target") or {}
        executable = message.get("executable")
        kinds = target.get("kind") or []
        if (
            executable
            and target.get("name") == "invocation_qualification"
            and "test" in kinds
        ):
            found.append(Path(executable))
    unique = list(dict.fromkeys(found))
    if len(unique) != 1:
        raise IsolationError(
            f"expected one invocation_qualification test binary, got {unique!r}"
        )
    binary = unique[0]
    if not binary.is_file():
        raise IsolationError(f"qualification test binary is missing: {binary}")
    return binary


def run_root_rel(temp_root: Path, run_root: Path) -> str:
    temp_root = require_absolute_dir(temp_root, "qualification temp root")
    run_root = Path(run_root)
    if not run_root.is_absolute():
        raise IsolationError("qualification run root must be an absolute path")
    rel = run_root.resolve().relative_to(temp_root)
    return rel.as_posix()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    allocate = sub.add_parser("allocate", help="create a unique retained run root")
    allocate.add_argument("temp_root")
    allocate.add_argument("group")
    allocate.add_argument("run", type=int)

    cleanup = sub.add_parser("cleanup", help="delete retained run roots after summary")
    cleanup.add_argument("temp_root")

    resolve_bin = sub.add_parser(
        "resolve-bin", help="print the invocation_qualification executable from cargo JSON"
    )
    resolve_bin.add_argument(
        "json_path",
        nargs="?",
        help="cargo --message-format=json output; stdin if omitted",
    )

    rel = sub.add_parser("run-root-rel", help="print run root relative to the temp root")
    rel.add_argument("temp_root")
    rel.add_argument("run_root")

    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    try:
        if args.command == "allocate":
            print(allocate_run_root(Path(args.temp_root), args.group, args.run))
        elif args.command == "cleanup":
            cleanup_retained_roots(Path(args.temp_root))
        elif args.command == "resolve-bin":
            raw = Path(args.json_path).read_text(encoding="utf-8") if args.json_path else sys.stdin.read()
            print(test_binary_from_cargo_json(raw))
        elif args.command == "run-root-rel":
            print(run_root_rel(Path(args.temp_root), Path(args.run_root)))
        else:
            raise IsolationError(f"unknown command {args.command}")
    except IsolationError as exc:
        print(f"qualification isolation failed closed: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
