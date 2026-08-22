#!/usr/bin/env python3
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
"""Contract tests for qualification run-root isolation."""

from __future__ import annotations

import json
import pathlib
import tempfile
import unittest

import qualification_isolation as qi
import summarize_qualification as sq
import test_summarize_qualification as tsq


class AllocateTests(unittest.TestCase):
    def test_run_n_remains_while_run_n_plus_one_is_allocated(self):
        with tempfile.TemporaryDirectory() as raw:
            temp_root = pathlib.Path(raw).resolve()
            first = qi.allocate_run_root(temp_root, "result_finalize_stress", 1)
            (first / "sessions.git").mkdir()
            second = qi.allocate_run_root(temp_root, "result_finalize_stress", 2)
            self.assertTrue((first / "sessions.git").is_dir())
            self.assertTrue(second.is_dir())
            self.assertNotEqual(first, second)
            self.assertEqual(
                qi.run_root_rel(temp_root, first),
                "runs/result_finalize_stress/01",
            )

    def test_reused_run_root_fails_closed(self):
        with tempfile.TemporaryDirectory() as raw:
            temp_root = pathlib.Path(raw).resolve()
            first = qi.allocate_run_root(temp_root, "result_finalize_stress", 1)
            (first / "marker").write_text("keep", encoding="utf-8")
            with self.assertRaises(qi.IsolationError) as caught:
                qi.allocate_run_root(temp_root, "result_finalize_stress", 1)
            self.assertIn("already exists", str(caught.exception))
            self.assertEqual((first / "marker").read_text(encoding="utf-8"), "keep")

    def test_malformed_group_and_relative_temp_root_fail_closed(self):
        with tempfile.TemporaryDirectory() as raw:
            temp_root = pathlib.Path(raw).resolve()
            with self.assertRaises(qi.IsolationError):
                qi.allocate_run_root(temp_root, "../escape", 1)
            with self.assertRaises(qi.IsolationError):
                qi.allocate_run_root(pathlib.Path("relative"), "result_finalize_stress", 1)
            with self.assertRaises(qi.IsolationError):
                qi.allocate_run_root(temp_root, "result_finalize_stress", 0)
            with self.assertRaises(qi.IsolationError):
                qi.allocate_run_root(temp_root, "result_finalize_stress", 21)

    def test_cleanup_is_explicit_and_does_not_touch_the_bundle(self):
        with tempfile.TemporaryDirectory() as raw:
            temp_root = pathlib.Path(raw).resolve()
            bundle = temp_root / "bundle"
            bundle.mkdir()
            tsq.write_bundle(bundle)
            first = qi.allocate_run_root(temp_root, "result_finalize_stress", 1)
            (first / "sessions.git").mkdir()
            self.assertTrue(first.exists())
            pass_status = sq.run(bundle, "qualify")
            self.assertEqual(pass_status, 0)
            self.assertTrue(first.exists(), "qualify PASS must not delete run roots")
            fail_status = sq.run(bundle, "qualify", manifest=tsq.stripped_baseline_manifest())
            self.assertEqual(fail_status, 1)
            self.assertTrue(first.exists(), "qualify FAIL must not delete run roots")
            qi.cleanup_retained_roots(temp_root)
            self.assertFalse((temp_root / "runs").exists())
            self.assertTrue((bundle / "result-finalize.jsonl").is_file())
            report = sq.run(bundle, "report")
            self.assertEqual(report, 0)

class CargoJsonTests(unittest.TestCase):
    def test_resolve_bin_rejects_two_distinct_executables(self):
        payload = "\n".join(
            [
                json.dumps(
                    {
                        "executable": "/tmp/invocation_qualification-abc",
                        "target": {
                            "name": "invocation_qualification",
                            "kind": ["test"],
                        },
                    }
                ),
                json.dumps(
                    {
                        "executable": "/tmp/invocation_qualification-def",
                        "target": {
                            "name": "invocation_qualification",
                            "kind": ["test"],
                        },
                    }
                ),
            ]
        )
        with self.assertRaises(qi.IsolationError) as caught:
            qi.test_binary_from_cargo_json(payload)
        self.assertIn("expected one", str(caught.exception))

    def test_resolve_bin_dedupes_repeated_cargo_artifacts(self):
        with tempfile.TemporaryDirectory() as raw:
            binary = pathlib.Path(raw) / "invocation_qualification-abc"
            binary.write_text("", encoding="utf-8")
            line = json.dumps(
                {
                    "executable": str(binary),
                    "target": {
                        "name": "invocation_qualification",
                        "kind": ["test"],
                    },
                }
            )
            resolved = qi.test_binary_from_cargo_json(line + "\n" + line + "\n")
            self.assertEqual(resolved, binary)


if __name__ == "__main__":
    unittest.main()
