#!/usr/bin/env python3
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
"""Contract tests for the qualification contract v2 summarizer.

The manifest remains the sole metric definition while its enforcement is
suspended. These tests exercise the estimators, strict input validation, and
the distinction between diagnostic benchmarking and strict qualification.
"""

from __future__ import annotations

import copy
import json
import pathlib
import tempfile
import unittest

import summarize_qualification as sq

HERE = pathlib.Path(__file__).resolve().parent
MS = 1_000_000


def repository_block():
    return {
        "append_count": 1,
        "repository_lock_count": 1,
        "repository_lock_wait_ns": 1,
        "repository_lock_wait_max_ns": 1,
        "append_fsync_barrier_ns": 1,
        "append_fsync_barrier_max_ns": 1,
    }


def local_record(run):
    return {
        "harness": "local-ledger",
        "harness_shape_version": 2,
        "run": run,
        "invocations": 1280,
        "per_writer_completed": [340] * 8,
        "per_writer_wall_ns": [1] * 8,
        "reserve_batch_ns": [MS] * 160,
        "dispatch_started_ns": [MS] * 1280,
        "terminal_evidence_publish_ns": [MS] * 1280,
        "wall_ns": MS,
        "repository": repository_block(),
        "writer_wall_max_to_min_ratio_milli": 1000,
        "max_consecutive_wait": 7,
        "unexpected_indeterminate_count": 0,
        "cas_retry_count": 0,
        "cas_retry_mode": "structural_zero_under_exclusive_repository_flock",
        "fixture_retention": "runner_owned",
    }


def finalize_record(run, *, full=None, terminal=None, pin=None, dispatch=None,
                    revalidation=None, barrier_max_ns=1):
    repository = repository_block()
    repository["append_fsync_barrier_max_ns"] = barrier_max_ns
    return {
        "harness": "result-finalize",
        "harness_shape_version": 2,
        "run": run,
        "invocations": 160,
        "clean_absent_count": 60,
        "uncertain_count": 60,
        "durable_count": 40,
        "session_repository_create_count": 1,
        "invocation_repository_create_count": 1,
        "timed_finalize_repository_create_count": 0,
        "full_terminal_finalize_ns": list(full or [30 * MS] * 160),
        "terminal_evidence_publish_ns": list(terminal or [14 * MS] * 160),
        "session_journal_pin_ns": list(pin or [14 * MS] * 160),
        "dispatch_started_ns": list(dispatch or [14 * MS] * 160),
        "reserve_batch_ns": [14 * MS] * 20,
        "result_draft_publish_ns": [3 * MS] * 40,
        "uncertain_draft_revalidation_ns": list(revalidation or [2 * MS] * 60),
        "wall_ns": 12 * 1_000_000_000,
        "repository": repository,
        "unexpected_indeterminate_count": 0,
        "cas_retry_count": 0,
        "cas_retry_mode": "structural_zero_under_exclusive_repository_flock",
        "fixture_retention": "runner_owned",
    }


def cold_record(run):
    return {
        "harness": "cold-reopen",
        "harness_shape_version": 2,
        "run": run,
        "invocations": 1280,
        "commits": 2561,
        "checkpoint_plans_after_restart_writes": 1224,
        "writer_handle_lifetime": "one_open_per_write",
        "cold_read_with_checkpoint_ns": [20 * MS] * 20,
        "cold_read_without_checkpoint_ns": [200 * MS] * 20,
        "repeated_project_facts_ns": [5_000] * 20,
        "wall_ns": 80 * 1_000_000_000,
        "repository": repository_block(),
        "cas_retry_count": 0,
        "cas_retry_mode": "structural_zero_under_exclusive_repository_flock",
        "fixture_retention": "runner_owned",
    }


def write_jsonl(path: pathlib.Path, records):
    path.write_text(
        "".join(json.dumps(record, separators=(",", ":")) + "\n" for record in records),
        encoding="utf-8",
    )


def baselined_manifest(manifest=None):
    """A manifest whose family-T baselines are established.

    Without a baseline every family-T gate is UNADJUDICABLE by design, so the
    passing path can only be exercised against an established reference.
    """
    manifest = copy.deepcopy(manifest or sq.load_manifest())
    baselines = {
        "T-finalize-full-p50": 30 * MS,
        "T-finalize-full-p90": 30 * MS,
        "T-finalize-terminal-p50": 14 * MS,
        "T-finalize-journal-pin-p50": 14 * MS,
        "T-finalize-dispatch-p50": 14 * MS,
        "T-finalize-reserve-batch-p50": 14 * MS,
        "T-finalize-draft-publish-p50": 3 * MS,
        "T-finalize-revalidation-p50": 2 * MS,
        "T-cold-read-with-checkpoint-p50": 20 * MS,
    }
    for gate in manifest["gates"]:
        if gate["id"] in baselines:
            gate["baseline_ns"] = baselines[gate["id"]]
    manifest["baseline_provenance"] = {
        "source_sha": "0123456789abcdef0123456789abcdef01234567",
        "harness_shape_version": manifest["required_harness_shape_version"],
        "qualification_contract_version": manifest["qualification_contract_version"],
        "bundle_identity": "test-baseline-bundle",
        "bundle_path": "target/invocation-store-bundles/test-baseline-bundle",
    }
    return manifest


def stripped_baseline_manifest():
    """Live manifest with family T unbound.

    After a shape baseline is installed, the pre-baseline fail-closed path
    still has to be testable."""
    manifest = copy.deepcopy(sq.load_manifest())
    for gate in manifest["gates"]:
        if "baseline_ns" in gate:
            gate["baseline_ns"] = None
    manifest["baseline_provenance"] = {name: None for name in sq.BASELINE_PROVENANCE_FIELDS}
    return manifest

def write_bundle(root: pathlib.Path, *, finalize=None, local=None, cold=None,
                 environment=True, contract_version=2):
    manifest = sq.load_manifest()
    runs = {name: spec["runs"] for name, spec in manifest["groups"].items()}
    write_jsonl(root / "local-ledger.jsonl",
                local if local is not None else [local_record(i) for i in range(1, runs["local-ledger"] + 1)])
    write_jsonl(root / "result-finalize.jsonl",
                finalize if finalize is not None
                else [finalize_record(i) for i in range(1, runs["result-finalize"] + 1)])
    if cold is not False:
        write_jsonl(root / "cold-reopen.jsonl",
                    cold if cold is not None else [cold_record(i) for i in range(1, runs["cold-reopen"] + 1)])
    if environment:
        (root / "environment.json").write_text(
            json.dumps({
                "schema_version": 5,
                "qualification_contract_version": contract_version,
                "harness_shape_version": 2,
                "git_sha": "0123456789abcdef0123456789abcdef01234567",
            }),
            encoding="utf-8",
        )


def verdicts(summary):
    return {row["id"]: row["verdict"] for row in summary["gates"]}


class ManifestIsTheAuthority(unittest.TestCase):
    def test_manifest_loads_and_declares_its_versions(self):
        manifest = sq.load_manifest()
        self.assertEqual(manifest["qualification_contract_version"], 2)
        self.assertEqual(manifest["required_harness_shape_version"], 2)
        self.assertEqual(manifest["attribution_estimator_revision"], 2)
        a_gate = next(g for g in manifest["gates"] if g["id"] == "A-finalize-stall-attribution")
        self.assertEqual(a_gate["attribution_estimator_revision"], 2)
        self.assertEqual(a_gate["substantial_share"], 0.25)

    def test_manifest_marks_the_installed_baseline_diagnostic_only(self):
        manifest = sq.load_manifest()
        self.assertEqual(manifest["enforcement_status"], "diagnostic_only")
        self.assertEqual(manifest["follow_up_issue"], 153)
        provenance = manifest["baseline_provenance"]
        self.assertTrue(provenance["source_sha"])
        self.assertEqual(
            provenance["harness_shape_version"],
            manifest["required_harness_shape_version"],
        )
        self.assertEqual(
            provenance["qualification_contract_version"],
            manifest["qualification_contract_version"],
        )
        self.assertTrue(provenance["bundle_identity"])
        self.assertTrue(provenance["bundle_path"].startswith("target/"))
        relative_t = [
            gate for gate in manifest["gates"]
            if gate["family"] == "T" and "baseline_ns" in gate
        ]
        self.assertTrue(relative_t)
        self.assertTrue(all(gate["baseline_ns"] is not None for gate in relative_t))
    def test_every_gate_id_is_unique(self):
        ids = [gate["id"] for gate in sq.load_manifest()["gates"]]
        self.assertEqual(len(ids), len(set(ids)))

    def test_every_gate_states_a_family_and_a_basis(self):
        for gate in sq.load_manifest()["gates"]:
            self.assertIn(gate["family"], {"D", "T", "S", "B", "F", "A"}, gate["id"])
            self.assertTrue(gate.get("basis"), gate["id"])

    def test_budget_coupling_rule_holds_in_the_manifest(self):
        """lock_wait_gate + 2 x full_finalize_gate + backoff_cap <= FinalizeBudget."""
        manifest = sq.load_manifest()
        gates = {gate["id"]: gate for gate in manifest["gates"]}
        chain = (
            gates["B-local-lock-wait-max"]["maximum_ns"]
            + 2 * gates["B-finalize-full-max"]["maximum_ns"]
            + manifest["backoff_cap_ns"]
        )
        self.assertLessEqual(chain, manifest["finalize_budget_ns"])

    def test_v1_run_incidence_is_a_diagnostic_not_a_gate(self):
        manifest = sq.load_manifest()
        self.assertNotIn(
            "run_incidence", {gate["estimator"] for gate in manifest["gates"]}
        )
        self.assertIn(
            "run_incidence", {spec["estimator"] for spec in manifest["diagnostics"]}
        )

    def test_no_k_baseline_survives_from_contract_v1(self):
        self.assertFalse(hasattr(sq, "RUN_INCIDENCE_GATES"))
        self.assertFalse(hasattr(sq, "STILL_EFFECTIVE_APPROVED_BUNDLES"))
        self.assertNotIn(
            "max_exceeding_runs", json.dumps(sq.load_manifest()["gates"])
        )

    def test_changing_a_manifest_limit_changes_the_verdict(self):
        manifest = baselined_manifest()
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            self.assertEqual(sq.run(root, "qualify", write=False, manifest=manifest), 0)
            for gate in manifest["gates"]:
                if gate["id"] == "B-finalize-full-max":
                    gate["maximum_ns"] = 1
            self.assertEqual(sq.run(root, "qualify", write=False, manifest=manifest), 1)

    def test_benchmark_mode_preserves_diagnostics_without_blocking_on_a_gate(self):
        manifest = baselined_manifest()
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            for gate in manifest["gates"]:
                if gate["id"] == "B-finalize-full-max":
                    gate["maximum_ns"] = 1
            self.assertEqual(sq.run(root, "qualify", write=False, manifest=manifest), 1)
            self.assertEqual(sq.run(root, "benchmark", write=False, manifest=manifest), 0)


class VersionGating(unittest.TestCase):
    def test_qualify_fails_closed_without_a_recorded_contract_version(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, environment=False)
            self.assertEqual(sq.run(root, "qualify", write=False), 2)

    def test_benchmark_fails_closed_without_a_recorded_contract_version(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, environment=False)
            self.assertEqual(sq.run(root, "benchmark", write=False), 2)

    def test_qualify_fails_closed_on_a_foreign_contract_version(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, contract_version=99)
            self.assertEqual(sq.run(root, "qualify", write=False), 2)

    def test_qualify_fails_closed_on_a_shape_mismatch(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            records = [finalize_record(i) for i in range(1, 21)]
            for record in records:
                record["harness_shape_version"] = 1
            write_bundle(root, finalize=records)
            self.assertEqual(sq.run(root, "qualify", write=False), 2)

    def test_records_disagreeing_on_shape_fail_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            records = [finalize_record(i) for i in range(1, 21)]
            records[7]["harness_shape_version"] = 1
            write_bundle(root, finalize=records)
            self.assertEqual(sq.run(root, "report", write=False), 2)

    def test_family_t_is_unadjudicable_without_a_baseline(self):
        manifest = stripped_baseline_manifest()
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            summary = sq.build_summary(root, "qualify", manifest)
            relative = verdicts(summary)["T-finalize-full-p50"]
            self.assertEqual(relative, sq.UNADJUDICABLE)
            self.assertEqual(sq.run(root, "qualify", write=False, manifest=manifest), 1)

    def test_an_absolute_ceiling_still_binds_without_a_baseline(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(
                root,
                finalize=[finalize_record(i, full=[500 * MS] * 160) for i in range(1, 21)],
            )
            summary = sq.build_summary(root, "report")
            self.assertEqual(verdicts(summary)["T-finalize-full-p50"], sq.FAIL)

    def test_qualify_fails_closed_when_baseline_values_lack_provenance(self):
        manifest = copy.deepcopy(sq.load_manifest())
        manifest["baseline_provenance"] = {name: None for name in sq.BASELINE_PROVENANCE_FIELDS}
        for gate in manifest["gates"]:
            if gate["estimator"] == "median_of_run_quantile":
                gate["baseline_ns"] = 30 * MS
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            summary = sq.build_summary(root, "qualify", manifest)
            self.assertEqual(verdicts(summary)["T-finalize-full-p50"], sq.UNADJUDICABLE)
            note = next(row["note"] for row in summary["gates"] if row["id"] == "T-finalize-full-p50")
            self.assertIn("provenance", note)
            self.assertEqual(sq.run(root, "qualify", write=False, manifest=manifest), 1)

    def test_stale_baseline_shape_is_unadjudicable(self):
        manifest = baselined_manifest()
        manifest["baseline_provenance"]["harness_shape_version"] = 1
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            summary = sq.build_summary(root, "qualify", manifest)
            self.assertEqual(verdicts(summary)["T-finalize-full-p50"], sq.UNADJUDICABLE)
            note = next(row["note"] for row in summary["gates"] if row["id"] == "T-finalize-full-p50")
            self.assertIn("stale", note)
            self.assertEqual(sq.run(root, "qualify", write=False, manifest=manifest), 1)

    def test_local_bundle_provenance_uses_the_ignored_target_path(self):
        bundle = (
            HERE.parents[1]
            / "target"
            / "invocation-store-bundles"
            / "test-baseline-bundle"
        )
        self.assertEqual(
            sq.bundle_identity_and_path(bundle),
            (
                "test-baseline-bundle",
                "target/invocation-store-bundles/test-baseline-bundle",
            ),
        )

    def test_baseline_fragment_includes_provenance(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            summary = sq.build_summary(root, "baseline")
            fragment = json.loads(sq.render_baseline(summary, sq.load_manifest()))
            provenance = fragment["provenance"]
            self.assertEqual(provenance["source_sha"], "0123456789abcdef0123456789abcdef01234567")
            self.assertEqual(provenance["qualification_contract_version"], 2)
            self.assertEqual(provenance["harness_shape_version"], 2)
            self.assertEqual(provenance["bundle_identity"], root.name)
            self.assertTrue(provenance["bundle_path"])
            self.assertIn("T-finalize-full-p50", fragment["gates"])
            self.assertIn("baseline_ns", fragment["gates"]["T-finalize-full-p50"])
    def test_baseline_mode_does_not_grade_family_t_against_installed_values(self):
        manifest = baselined_manifest()
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            summary = sq.build_summary(root, "baseline", manifest)
            self.assertEqual(verdicts(summary)["T-finalize-full-p50"], sq.UNADJUDICABLE)
            self.assertEqual(sq.run(root, "baseline", write=False, manifest=manifest), 0)

    def test_baseline_fails_closed_without_source_sha(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            env = json.loads((root / "environment.json").read_text(encoding="utf-8"))
            del env["git_sha"]
            (root / "environment.json").write_text(json.dumps(env), encoding="utf-8")
            with self.assertRaises(sq.QualificationError) as raised:
                sq.build_summary(root, "baseline")
            self.assertIn("git_sha", str(raised.exception))
            self.assertEqual(sq.run(root, "baseline", write=False), 2)


class TypicalCostEstimator(unittest.TestCase):
    def test_median_of_run_p50_at_exactly_the_limit_passes(self):
        manifest = baselined_manifest()
        gate = next(g for g in manifest["gates"] if g["id"] == "T-finalize-full-p50")
        limit = int(gate["baseline_ns"] * (1 + gate["tolerance_pct"] / 100))
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=[finalize_record(i, full=[limit] * 160)
                                         for i in range(1, 21)])
            summary = sq.build_summary(root, "qualify", manifest)
            self.assertEqual(verdicts(summary)["T-finalize-full-p50"], sq.PASS)

    def test_one_nanosecond_over_the_limit_fails(self):
        manifest = baselined_manifest()
        gate = next(g for g in manifest["gates"] if g["id"] == "T-finalize-full-p50")
        over = int(gate["baseline_ns"] * (1 + gate["tolerance_pct"] / 100)) + 1
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=[finalize_record(i, full=[over] * 160)
                                         for i in range(1, 21)])
            summary = sq.build_summary(root, "qualify", manifest)
            self.assertEqual(verdicts(summary)["T-finalize-full-p50"], sq.FAIL)

    def test_a_single_stall_run_cannot_move_the_typical_gate(self):
        """The estimator the whole redesign exists for: one stalled run in
        twenty must not decide a latency verdict."""
        manifest = baselined_manifest()
        records = [finalize_record(i) for i in range(1, 21)]
        records[3]["full_terminal_finalize_ns"] = [4_000 * MS] * 160
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            summary = sq.build_summary(root, "report", manifest)
            self.assertEqual(verdicts(summary)["T-finalize-full-p50"], sq.PASS)


class ScalingInvariance(unittest.TestCase):
    def test_a_monotone_within_run_rise_fails(self):
        """Modelled on `d474236`'s revalidation profile: 1.01 ms rising to
        2.57 ms across one run, with no host stall involved."""
        rising = [int((1.0 + 1.6 * index / 59) * MS) for index in range(60)]
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=[finalize_record(i, revalidation=rising)
                                         for i in range(1, 21)])
            summary = sq.build_summary(root, "report")
            self.assertEqual(verdicts(summary)["S-finalize-revalidation-drift"], sq.FAIL)

    def test_a_flat_series_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            summary = sq.build_summary(root, "report")
            self.assertEqual(verdicts(summary)["S-finalize-revalidation-drift"], sq.PASS)

    def test_drift_ignores_an_isolated_tail_spike(self):
        spiky = [2 * MS] * 60
        spiky[59] = 4_000 * MS
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=[finalize_record(i, revalidation=spiky)
                                         for i in range(1, 21)])
            summary = sq.build_summary(root, "report")
            self.assertEqual(verdicts(summary)["S-finalize-revalidation-drift"], sq.PASS)


class StallAttribution(unittest.TestCase):
    @staticmethod
    def shared_stall(indices):
        full = [30 * MS] * 160
        terminal = [14 * MS] * 160
        pin = [14 * MS] * 160
        dispatch = [14 * MS] * 160
        for index in indices:
            full[index] = 3_000 * MS
            terminal[index] = 1_400 * MS
            pin[index] = 1_400 * MS
            dispatch[index] = 900 * MS
        return full, terminal, pin, dispatch

    def test_a_shared_durability_stall_is_attributed(self):
        full, terminal, pin, dispatch = self.shared_stall([56, 57])
        records = [finalize_record(i) for i in range(1, 21)]
        records[6] = finalize_record(
            7, full=full, terminal=terminal, pin=pin, dispatch=dispatch,
            barrier_max_ns=1_500 * MS,
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            summary = sq.build_summary(root, "report")
            row = next(r for r in summary["gates"] if r["id"] == "A-finalize-stall-attribution")
            self.assertEqual(row["verdict"], sq.PASS)
            self.assertEqual(len(row["clusters"]), 1)
            self.assertEqual(row["clusters"][0]["length"], 2)

    def test_a_single_stage_multisecond_sample_fails_at_count_one(self):
        """Only terminal publish moves: that is our code, and one sample is enough."""
        full = [30 * MS] * 160
        terminal = [14 * MS] * 160
        full[42] = 3_000 * MS
        terminal[42] = 2_900 * MS
        records = [finalize_record(i) for i in range(1, 21)]
        records[2] = finalize_record(3, full=full, terminal=terminal, barrier_max_ns=1)
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            summary = sq.build_summary(root, "report")
            row = next(r for r in summary["gates"] if r["id"] == "A-finalize-stall-attribution")
            self.assertEqual(row["verdict"], sq.FAIL)
            self.assertEqual(row["clusters"][0]["dominant_component"], "terminal_evidence_publish_ns")

    def test_co_elevated_stages_with_a_flat_barrier_still_fails(self):
        """In-span pin+terminal excess is attributed even when the run-wide barrier is flat.
        Kept as the shared-stall fixture; the barrier is out of span and unused."""
        full, terminal, pin, dispatch = self.shared_stall([10, 11])
        records = [finalize_record(i) for i in range(1, 21)]
        records[0] = finalize_record(
            1, full=full, terminal=terminal, pin=pin, dispatch=dispatch, barrier_max_ns=MS
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            summary = sq.build_summary(root, "report")
            row = next(r for r in summary["gates"] if r["id"] == "A-finalize-stall-attribution")
            self.assertEqual(row["verdict"], sq.PASS)

    def test_attribution_does_not_weaken_when_most_runs_stall(self):
        """The campaign-median form of this criterion degraded here; the
        self-referential form must not."""
        full, terminal, pin, dispatch = self.shared_stall([20, 21])
        records = []
        for index in range(1, 21):
            if index <= 15:
                records.append(finalize_record(
                    index, full=full, terminal=terminal, pin=pin, dispatch=dispatch,
                    barrier_max_ns=1_500 * MS,
                ))
            else:
                records.append(finalize_record(index))
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            summary = sq.build_summary(root, "report")
            row = next(r for r in summary["gates"] if r["id"] == "A-finalize-stall-attribution")
            self.assertEqual(row["verdict"], sq.PASS)
            self.assertEqual(len(row["clusters"]), 15)

    @staticmethod
    def sample_159_shape():
        """Approximate run-2 / sample-159 shape from campaign 742cfd20.

        Quiet samples sit at that run's medians so the stall is a mild
        composite: pin, terminal, and residual each about 2×, none 5×.
        """
        index = 159
        full = [52_176_000] * 160
        pin = [27_388_000] * 160
        terminal = [17_570_000] * 160
        full[index] = 107_600_000
        pin[index] = 52_300_000
        terminal[index] = 36_400_000
        return index, full, pin, terminal

    def _a_row(self, **finalize_kwargs):
        records = [finalize_record(i) for i in range(1, 21)]
        records[1] = finalize_record(2, **finalize_kwargs)
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            summary = sq.build_summary(root, "report")
        return next(r for r in summary["gates"] if r["id"] == "A-finalize-stall-attribution")

    def test_multi_component_excess_is_attributed(self):
        index, full, pin, terminal = self.sample_159_shape()
        row = self._a_row(full=full, pin=pin, terminal=terminal, barrier_max_ns=1)
        self.assertEqual(row["verdict"], sq.PASS)
        self.assertEqual(len(row["clusters"]), 1)
        cluster = row["clusters"][0]
        self.assertEqual(cluster["indices"], [index])
        self.assertTrue(cluster["attributed"])
        self.assertIsNone(cluster["dominant_component"])
        self.assertGreaterEqual(len(cluster["contributing_components"]), 2)

    def test_single_component_dominance_fails(self):
        quiet_full, quiet_pin, quiet_term, quiet_res = 30 * MS, 14 * MS, 14 * MS, 2 * MS
        extra = 80 * MS
        cases = (
            ("pin-only", extra, 0, 0, "session_journal_pin_ns"),
            ("terminal-only", 0, extra, 0, "terminal_evidence_publish_ns"),
            ("residual-only", 0, 0, extra, "residual_ns"),
        )
        for name, pin_extra, term_extra, res_extra, dominant in cases:
            with self.subTest(name=name):
                full = [quiet_full] * 160
                pin = [quiet_pin] * 160
                terminal = [quiet_term] * 160
                full[40] = quiet_full + pin_extra + term_extra + res_extra
                pin[40] = quiet_pin + pin_extra
                terminal[40] = quiet_term + term_extra
                self.assertEqual(full[40], pin[40] + terminal[40] + quiet_res + res_extra)
                row = self._a_row(full=full, pin=pin, terminal=terminal)
                self.assertEqual(row["verdict"], sq.FAIL, name)
                cluster = row["clusters"][0]
                self.assertFalse(cluster["attributed"], name)
                self.assertEqual(cluster["dominant_component"], dominant)

    def test_malformed_component_evidence_fails_closed(self):
        gate = next(g for g in sq.load_manifest()["gates"] if g["id"] == "A-finalize-stall-attribution")
        index, full, pin, terminal = self.sample_159_shape()

        def record():
            return finalize_record(2, full=full, pin=pin, terminal=terminal)

        short = record()
        short["session_journal_pin_ns"] = pin[:-1]
        missing = record()
        del missing["session_journal_pin_ns"]
        broken_identity = record()
        broken_identity["full_terminal_finalize_ns"][index] = 50 * MS
        broken_identity["session_journal_pin_ns"][index] = 40 * MS
        broken_identity["terminal_evidence_publish_ns"][index] = 40 * MS
        cases = (
            ("length-mismatch", short),
            ("missing-pin", missing),
            ("full-lt-pin-plus-terminal", broken_identity),
        )
        for name, rec in cases:
            with self.subTest(name=name):
                with self.assertRaises(sq.QualificationError):
                    sq.attribute_cluster(rec, [index], gate)

    def test_out_of_span_evidence_does_not_affect_attribution(self):
        full, terminal, pin, dispatch = self.shared_stall([10, 11])
        loud_dispatch = list(dispatch)
        for index in (10, 11):
            loud_dispatch[index] = 10_000 * MS
        quiet = self._a_row(
            full=full, terminal=terminal, pin=pin, dispatch=dispatch, barrier_max_ns=1,
        )
        loud = self._a_row(
            full=full, terminal=terminal, pin=pin, dispatch=loud_dispatch,
            barrier_max_ns=10_000 * MS,
        )
        self.assertEqual(quiet["verdict"], loud["verdict"])
        self.assertEqual(quiet["clusters"][0]["attributed"], loud["clusters"][0]["attributed"])
        self.assertEqual(
            quiet["clusters"][0]["contributing_components"],
            loud["clusters"][0]["contributing_components"],
        )

class InputShape(unittest.TestCase):
    def test_wrong_run_count_fails_qualify_and_is_noted_by_report(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=[finalize_record(i) for i in range(1, 20)])
            self.assertEqual(sq.run(root, "qualify", write=False), 2)
            summary = sq.build_summary(root, "report")
            self.assertTrue(any("requires exactly 20 runs" in note
                                for note in summary["run_count_notes"]))

    def test_short_series_fails_qualify(self):
        records = [finalize_record(i) for i in range(1, 21)]
        records[3]["full_terminal_finalize_ns"] = [MS] * 159
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            self.assertEqual(sq.run(root, "qualify", write=False), 2)

    def test_non_numeric_sample_fails_qualify(self):
        records = [finalize_record(i) for i in range(1, 21)]
        records[0]["full_terminal_finalize_ns"][3] = "slow"
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            self.assertEqual(sq.run(root, "qualify", write=False), 2)

    def test_missing_metric_fails_qualify_and_degrades_report(self):
        records = [finalize_record(i) for i in range(1, 21)]
        for record in records:
            del record["session_journal_pin_ns"]
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            self.assertEqual(sq.run(root, "qualify", write=False), 2)
            summary = sq.build_summary(root, "report")
            self.assertEqual(
                verdicts(summary)["T-finalize-journal-pin-p50"], sq.INSUFFICIENT
            )

    def test_missing_result_finalize_fails_closed_in_both_modes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_jsonl(root / "local-ledger.jsonl", [local_record(i) for i in range(1, 11)])
            self.assertEqual(sq.run(root, "report", write=False), 2)
            self.assertEqual(sq.run(root, "qualify", write=False), 2)

    def test_report_allows_a_historical_bundle_without_cold_reopen(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, cold=False)
            self.assertEqual(sq.run(root, "report", write=False), 0)
            self.assertEqual(sq.run(root, "qualify", write=False), 2)

    def test_malformed_jsonl_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "result-finalize.jsonl"
            path.write_text("{not json\n", encoding="utf-8")
            with self.assertRaises(sq.QualificationError):
                sq.load_jsonl(path)

    def test_empty_jsonl_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "result-finalize.jsonl"
            path.write_text("\n", encoding="utf-8")
            with self.assertRaises(sq.QualificationError):
                sq.load_jsonl(path)

    def test_foreign_cas_mode_fails_closed(self):
        records = [finalize_record(i) for i in range(1, 21)]
        records[5]["cas_retry_mode"] = "retry_on_conflict"
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root, finalize=records)
            self.assertEqual(sq.run(root, "report", write=False), 2)


class EvidenceImmutability(unittest.TestCase):
    def test_report_writes_nothing_into_the_bundle(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            before = {path.name: path.read_bytes() for path in sorted(root.iterdir())}
            self.assertEqual(sq.main(["report", str(root)]), 0)
            after = {path.name: path.read_bytes() for path in sorted(root.iterdir())}
            self.assertEqual(before, after)

    def test_baseline_mode_writes_nothing_and_grades_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            before = {path.name: path.read_bytes() for path in sorted(root.iterdir())}
            self.assertEqual(sq.main(["baseline", str(root)]), 0)
            after = {path.name: path.read_bytes() for path in sorted(root.iterdir())}
            self.assertEqual(before, after)

    def test_qualify_writes_the_summaries(self):
        manifest = baselined_manifest()
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            self.assertEqual(sq.run(root, "qualify", manifest=manifest), 0)
            self.assertTrue((root / "summary.json").is_file())
            self.assertTrue((root / "summary.md").is_file())
            summary = json.loads((root / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(summary["schema_version"], sq.SUMMARY_SCHEMA_VERSION)
            self.assertEqual(summary["qualification_contract_version"], 2)

    def test_benchmark_writes_a_failing_verdict_without_blocking(self):
        manifest = baselined_manifest()
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            write_bundle(root)
            for gate in manifest["gates"]:
                if gate["id"] == "B-finalize-full-max":
                    gate["maximum_ns"] = 1
            self.assertEqual(sq.run(root, "benchmark", manifest=manifest), 0)
            summary = json.loads((root / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(verdicts(summary)["B-finalize-full-max"], sq.FAIL)
            self.assertTrue((root / "summary.md").is_file())


class DocumentRendering(unittest.TestCase):
    def test_render_gates_lists_every_gate_and_run_count(self):
        manifest = sq.load_manifest()
        table = sq.render_gate_table(manifest)
        for gate in manifest["gates"]:
            self.assertIn(gate["id"], table)
        for name, spec in manifest["groups"].items():
            self.assertIn(f"`{name}`: {spec['runs']} runs", table)

    def test_check_doc_accepts_a_current_block_and_rejects_a_stale_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            good = pathlib.Path(tmp) / "good.md"
            good.write_text(
                f"intro\n\n{sq.GATE_TABLE_BEGIN}\n{sq.render_gate_table()}\n"
                f"{sq.GATE_TABLE_END}\n\noutro\n",
                encoding="utf-8",
            )
            self.assertEqual(sq.check_doc(good), 0)
            stale = pathlib.Path(tmp) / "stale.md"
            stale.write_text(
                f"{sq.GATE_TABLE_BEGIN}\nold table\n{sq.GATE_TABLE_END}\n",
                encoding="utf-8",
            )
            self.assertEqual(sq.check_doc(stale), 1)
            missing = pathlib.Path(tmp) / "missing.md"
            missing.write_text("no block here\n", encoding="utf-8")
            self.assertEqual(sq.check_doc(missing), 2)

    def test_the_approval_record_gate_table_is_current(self):
        record = HERE / "../../docs/plans/2026-08-21-issue139a-qualification-contract-v2.md"
        self.assertEqual(sq.check_doc(record.resolve()), 0)


class Cli(unittest.TestCase):
    def test_unknown_mode_is_a_usage_error(self):
        self.assertEqual(sq.main(["approve", "."]), 2)
        self.assertEqual(sq.main([]), 2)

    def test_render_gates_via_cli(self):
        self.assertEqual(sq.main(["render-gates"]), 0)


if __name__ == "__main__":
    unittest.main()
