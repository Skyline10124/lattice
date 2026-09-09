#!/usr/bin/env python3
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
"""Summarize and adjudicate InvocationStore qualification bundles.

Qualification contract v2. Every threshold, run count, and sample shape comes
from `qualification-contract-v2.json`; this module holds estimators, not
constants. See `docs/development/invocation-store-diagnostics.md`.

Modes:

* `benchmark BUNDLE` — run a campaign as a diagnostic. Writes `summary.json` /
  `summary.md`; gate failures are reported but do not block.
* `qualify BUNDLE`   — retain strict adjudication for qualification redesign.
  Writes summaries and exits non-zero on an enforcing failure or insufficient gate.
* `report BUNDLE`    — inspect a retained bundle without writing a byte into
  it. Well-formed data that fails a gate is reported, not raised; missing or
  malformed data still fails closed. A bundle recorded under an older contract
  or harness shape is rendered with `insufficient_evidence` rows and never has
  its recorded verdict rewritten.
* `baseline BUNDLE`  — print the family-T baseline fragment measured by a
  baseline campaign. Produces no pass/fail: a baseline campaign defines the
  reference point, it does not grade against it.
* `render-gates`     — print the gate table for the approval record.
* `check-doc FILE`   — fail when FILE's generated gate-table block has drifted
  from the manifest.
"""

from __future__ import annotations

import json
import math
import pathlib
import statistics
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
MANIFEST_PATH = HERE / "qualification-contract-v2.json"

SUMMARY_SCHEMA_VERSION = 4
GATE_TABLE_BEGIN = "<!-- BEGIN GENERATED GATE TABLE -->"
GATE_TABLE_END = "<!-- END GENERATED GATE TABLE -->"

PASS = "PASS"
FAIL = "FAIL"
UNADJUDICABLE = "UNADJUDICABLE"
INSUFFICIENT = "insufficient_evidence"

# Harness shape 1 is the retired per-step `sessions-N.git` benchmark. Records
# written before shape versioning carry no field and are shape 1 by definition.
DEFAULT_HARNESS_SHAPE_VERSION = 1


class QualificationError(Exception):
    """Incomplete or malformed qualification input. Always fail closed."""


# ---------------------------------------------------------------------------
# Loading
# ---------------------------------------------------------------------------


def load_manifest(path: pathlib.Path | None = None) -> dict:
    path = path or MANIFEST_PATH
    if not path.is_file():
        raise QualificationError(f"missing gate manifest {path.name}")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise QualificationError(f"gate manifest {path.name} is not JSON: {exc}") from exc


def load_jsonl(path: pathlib.Path) -> list[dict]:
    if not path.is_file():
        raise QualificationError(f"missing qualification file {path.name}")
    records = []
    for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not line.strip():
            continue
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError as exc:
            raise QualificationError(f"{path.name} line {line_no} is not JSON: {exc}") from exc
    if not records:
        raise QualificationError(f"{path.name} contains no records")
    return records


def load_environment(bundle: pathlib.Path) -> dict:
    path = bundle / "environment.json"
    if not path.is_file():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise QualificationError(f"environment.json is not JSON: {exc}") from exc


# ---------------------------------------------------------------------------
# Primitives
# ---------------------------------------------------------------------------


def percentile_of(values, p):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, math.ceil(len(ordered) * p) - 1)]


def distribution(values):
    ordered = sorted(values)

    def percentile(p):
        return ordered[min(len(ordered) - 1, math.ceil(len(ordered) * p) - 1)]

    return {
        "count": len(ordered),
        "min_ns": ordered[0],
        "p50_ns": percentile(0.50),
        "p90_ns": percentile(0.90),
        "p95_ns": percentile(0.95),
        "p99_ns": percentile(0.99),
        "max_ns": ordered[-1],
        "mean_ns": round(statistics.fmean(ordered)),
    }


def dig(record: dict, path: str):
    """Read a dotted field path, failing closed on an absent segment."""
    value = record
    for segment in path.split("."):
        if not isinstance(value, dict) or segment not in value:
            raise QualificationError(f"record is missing field {path}")
        value = value[segment]
    return value


def series(record: dict, metric: str) -> list:
    """Read one run's sample array, failing closed on an absent metric."""
    value = record.get(metric)
    if not isinstance(value, list) or not value:
        raise QualificationError(f"record is missing metric {metric}")
    return value


def samples(records: list[dict], metric: str) -> list:
    pooled = []
    for record in records:
        pooled.extend(series(record, metric))
    return pooled


def median_of_run_quantile(records: list[dict], metric: str, quantile: float) -> float:
    return statistics.median(
        percentile_of(series(record, metric), quantile) for record in records
    )


def within_run_drift(records: list[dict], metric: str, buckets: int) -> float:
    """Median of the last bucket over median of the first, pooled across runs.

    A per-operation cost that ADR 0005 calls decoupled from history depth must
    not rise as one run deepens its own ledger. Pooling across runs averages
    host noise out; a monotone rise survives it.
    """
    length = len(series(records[0], metric))
    if length < buckets * 2:
        raise QualificationError(
            f"{metric} needs at least {buckets * 2} samples for a {buckets}-bucket drift"
        )
    size = length // buckets
    first = [s for record in records for s in series(record, metric)[:size]]
    last = [s for record in records for s in series(record, metric)[(buckets - 1) * size:]]
    head = statistics.median(first)
    if head <= 0:
        raise QualificationError(f"{metric} first-bucket median is not positive")
    return statistics.median(last) / head


def stall_clusters(record: dict, metric: str, threshold_ns: int) -> list[list[int]]:
    """Maximal runs of consecutive sample indices above the stall threshold."""
    clusters: list[list[int]] = []
    for index, value in enumerate(series(record, metric)):
        if value <= threshold_ns:
            continue
        if clusters and clusters[-1][-1] == index - 1:
            clusters[-1].append(index)
        else:
            clusters.append([index])
    return clusters


def aligned_finalize_components(record: dict, gate: dict) -> dict[str, list]:
    """Pin, terminal, and residual aligned to full_terminal_finalize_ns.

    Residual is derived, never recorded: full[i] - pin[i] - terminal[i].
    A negative residual means the timings cannot close; it is never clamped
    to zero. Lengths must match full exactly. dispatch_started_ns is not a
    component of this span.
    """
    full_name = gate["metric"]
    full = series(record, full_name)
    components = {full_name: full}
    for name in gate["in_span_components"]:
        samples = series(record, name)
        if len(samples) != len(full):
            raise QualificationError(
                f"{name} length {len(samples)} does not match {full_name} length {len(full)}"
            )
        components[name] = samples
    residual_name = gate["residual_component"]
    residual = []
    for index, total in enumerate(full):
        spent = sum(components[name][index] for name in gate["in_span_components"])
        if total < spent:
            joined = " + ".join(gate["in_span_components"])
            raise QualificationError(
                f"{full_name}[{index}]={total} is less than {joined}={spent}"
            )
        residual.append(total - spent)
    components[residual_name] = residual
    return components

def attribute_cluster(record: dict, cluster: list[int], gate: dict) -> dict:
    """Classify one stall cluster by in-span component excess.

    Error model (attribution estimator revision 2)
    ----------------------------------------------
    `full_terminal_finalize_ns[i]` is the span from the durable candidate
    pin through terminal evidence. `dispatch_started_ns` is timed *before*
    that span and is not a component of it. `append_fsync_barrier_max_ns`
    is the run-wide maximum of a different append path (reserve_batch) and
    is not time-aligned to sample i, so it cannot explain a specific
    cluster.

    Partition of sample i, fail closed, never clamp::

        residual[i] = full[i] - pin[i] - terminal[i]
        require full[i] >= pin[i] + terminal[i] for every i in the run
        require len(full) == len(pin) == len(terminal)

    Each component's positive excess is against that run's own p50 (the
    same order-statistic median family T uses). Cluster excesses sum those
    per-sample positive excesses inside the cluster.

    Share of C = excess_C / sum(excesses). C contributes substantially iff
    share_C >= 1/4. 1/4 is the largest dyadic unit fraction at which two
    components can both be substantial without either needing a majority.
    Majority (>1/2) is structurally unusable here: pin and terminal are a
    two-way split of the instrumented span, so a shared stall with small
    residual always gives one side a slight majority. Two substantial
    components together hold at least half the excess. A leftover under
    1/4 (a few percent of residual bleed on a multi-second stall) cannot
    mint a second contributor.

    Attributed (shared / multi-stage) iff at least two components are
    substantial. A single substantial component — including residual — is
    implementation-side.

    False positive (implementation regression called shared): a 97% / 3%
    split has one substantial component and fails. False negative (shared
    stall called ours): a host/device stall inside the finalize span lifts
    pin and terminal together, each typically well above 1/4, so the
    cluster is attributed. Residual majority is fail-closed: residual is
    draft/revalidation/classify plus uninstrumented work, and if it is the
    only substantial component we cannot claim a shared two-stage stall.
    Zero-unattributed still holds. The 100 ms cluster threshold, family B
    maxima, and FinalizeBudget are unchanged.
    """
    components = aligned_finalize_components(record, gate)
    full = components[gate["metric"]]
    names = list(gate["in_span_components"]) + [gate["residual_component"]]
    medians = {name: percentile_of(components[name], 0.5) for name in names}
    excesses = {
        name: sum(max(0, components[name][index] - medians[name]) for index in cluster)
        for name in names
    }
    total = sum(excesses.values())
    shares = {name: (excesses[name] / total if total else 0.0) for name in names}
    floor = gate["substantial_share"]
    substantial = [name for name in names if shares[name] >= floor]
    if len(substantial) == 1:
        dominant = substantial[0]
    elif not substantial and total:
        dominant = max(names, key=lambda name: shares[name])
    else:
        dominant = None
    contributing = [name for name in names if excesses[name] > 0]
    peak = max(full[index] for index in cluster)
    return {
        "run": record.get("run"),
        "indices": cluster,
        "length": len(cluster),
        "peak_ns": peak,
        "excess_ns": peak - percentile_of(full, 0.5),
        "component_excess_ns": excesses,
        "component_share": shares,
        "substantial_components": substantial,
        "dominant_component": dominant,
        "contributing_components": contributing,
        "attributed": len(substantial) >= gate["min_contributing_components"],
    }


# ---------------------------------------------------------------------------
# Input validation
# ---------------------------------------------------------------------------


def require_run_count(records: list[dict], name: str, expected: int, strict: bool) -> str | None:
    if len(records) == expected:
        return None
    message = f"{name} requires exactly {expected} runs, got {len(records)}"
    if strict:
        raise QualificationError(message)
    return message


def require_sample_shape(
    records: list[dict], name: str, expected: dict[str, int], strict: bool
) -> list[str]:
    """Validate the recorded sample shape.

    A new campaign fails closed. `report` collects the differences instead, so
    a bundle recorded before a metric existed stays readable and the gates
    that need the missing metric become `insufficient_evidence`.
    """
    problems: list[str] = []

    def problem(message: str) -> None:
        if strict:
            raise QualificationError(message)
        problems.append(message)

    for index, record in enumerate(records, start=1):
        if not isinstance(record, dict):
            raise QualificationError(f"{name} run {index} is not an object")
        for key, count in expected.items():
            if key not in record:
                problem(f"{name} run {index} is missing {key}")
                continue
            series = record[key]
            if not isinstance(series, list):
                problem(f"{name} run {index} {key} is not an array")
                continue
            if len(series) != count:
                problem(f"{name} run {index} {key} has {len(series)} samples, expected {count}")
                continue
            if any(
                not isinstance(sample, (int, float)) or isinstance(sample, bool)
                for sample in series
            ):
                problem(f"{name} run {index} {key} contains a non-numeric sample")
    return problems


def require_cas_mode(records: list[dict], name: str, expected: str) -> None:
    modes = sorted({record.get("cas_retry_mode") for record in records})
    if modes != [expected]:
        raise QualificationError(f"{name} cas_retry_mode {modes!r} is not [{expected}]")


def observed_shape_version(groups: dict[str, list[dict]]) -> int | str:
    observed = {
        record.get("harness_shape_version", DEFAULT_HARNESS_SHAPE_VERSION)
        for records in groups.values()
        for record in records
    }
    if len(observed) != 1:
        raise QualificationError(f"records disagree on harness_shape_version: {sorted(observed)}")
    return observed.pop()


# ---------------------------------------------------------------------------
# Gate evaluation
# ---------------------------------------------------------------------------


def fmt_ns(value, reference) -> str:
    if value is None:
        return "-"
    scale = reference if reference is not None else value
    if scale >= 1_000_000_000:
        return f"{value / 1_000_000_000:.3f} s"
    if scale >= 1_000:
        return f"{value / 1_000_000:.3f} ms"
    return f"{value:.0f} ns"


def _row(gate, measured, limit, verdict, note=None):
    row = {
        "id": gate["id"],
        "family": gate["family"],
        "group": gate["group"],
        "estimator": gate["estimator"],
        "measured": measured,
        "limit": limit,
        "verdict": verdict,
        "basis": gate.get("basis", ""),
    }
    if note:
        row["note"] = note
    return row


BASELINE_PROVENANCE_FIELDS = (
    "source_sha",
    "harness_shape_version",
    "qualification_contract_version",
    "bundle_identity",
    "bundle_path",
)


def load_baseline_provenance(manifest: dict) -> dict:
    return dict(manifest.get("baseline_provenance") or {})


def _provenance_value_present(value) -> bool:
    return value is not None and value != ""


def family_t_baseline_state(manifest: dict) -> tuple[str, str | None]:
    """Classify the installed family-T baseline.

    `none` — no values and no provenance (the intended pre-baseline state).
    `usable` — values plus complete provenance bound to this contract/shape.
    `invalid` — values without provenance, or provenance bound to a stale shape.
    """
    provenance = load_baseline_provenance(manifest)
    t_gates = [gate for gate in manifest["gates"] if gate["estimator"] == "median_of_run_quantile"]
    has_values = any(gate.get("baseline_ns") is not None for gate in t_gates)
    fields = [provenance.get(name) for name in BASELINE_PROVENANCE_FIELDS]
    complete = all(_provenance_value_present(value) for value in fields)
    if not has_values and not any(_provenance_value_present(value) for value in fields):
        return (
            "none",
            "no shape baseline established; run `baseline` on an authorised baseline campaign",
        )
    if has_values and not complete:
        return (
            "invalid",
            "baseline values are present without complete (source SHA, harness shape) provenance",
        )
    if not complete:
        return "invalid", "baseline provenance is incomplete"
    if provenance["harness_shape_version"] != manifest["required_harness_shape_version"]:
        return "invalid", "baseline harness shape is stale relative to the required shape"
    if provenance["qualification_contract_version"] != manifest["qualification_contract_version"]:
        return "invalid", "baseline contract version is stale"
    if not has_values:
        return "invalid", "baseline provenance is present without baseline values"
    return "usable", None


def bundle_identity_and_path(bundle: pathlib.Path) -> tuple[str, str]:
    identity = bundle.name
    local_bundles = HERE.parents[1] / "target" / "invocation-store-bundles"
    try:
        if bundle.resolve().parent == local_bundles.resolve():
            return identity, f"target/invocation-store-bundles/{identity}"
    except OSError:
        pass
    return identity, str(bundle)


def evaluate_gate(
    gate: dict,
    records: list[dict],
    shape_ok: bool,
    *,
    mode: str = "qualify",
    baseline_state: tuple[str, str | None] = ("none", None),
) -> dict:
    estimator = gate["estimator"]
    baseline_kind, baseline_note = baseline_state

    if estimator == "exact_every_run":
        expected = gate["equals"]
        observed = [dig(record, gate["field"]) for record in records]
        bad = [value for value in observed if value != expected]
        return _row(
            gate,
            f"{observed[0]!r}" + (f" (+{len(bad)} mismatched)" if bad else ""),
            f"{expected!r} in every run",
            PASS if not bad else FAIL,
        )

    if estimator == "exact_every_run_tuple":
        expected = list(gate["equals"])
        observed = [[dig(record, field) for field in gate["fields"]] for record in records]
        bad = [value for value in observed if value != expected]
        return _row(
            gate,
            "/".join(str(part) for part in observed[0]),
            "/".join(str(part) for part in expected),
            PASS if not bad else FAIL,
        )

    if estimator == "sum_over_runs":
        total = sum(dig(record, gate["field"]) for record in records)
        return _row(gate, str(total), str(gate["equals"]), PASS if total == gate["equals"] else FAIL)

    if estimator == "median_of_run_quantile":
        measured = median_of_run_quantile(records, gate["metric"], gate["quantile"])
        ceiling = gate["absolute_max_ns"]
        if measured > ceiling:
            return _row(gate, fmt_ns(measured, ceiling), f"<= {fmt_ns(ceiling, ceiling)} absolute", FAIL)
        if mode == "baseline":
            return _row(
                gate,
                fmt_ns(measured, ceiling),
                f"baseline +{gate['tolerance_pct']}% (absolute <= {fmt_ns(ceiling, ceiling)})",
                UNADJUDICABLE,
                "baseline campaign defines family T; it does not grade it",
            )
        if baseline_kind != "usable":
            return _row(
                gate,
                fmt_ns(measured, ceiling),
                f"baseline +{gate['tolerance_pct']}% (absolute <= {fmt_ns(ceiling, ceiling)})",
                UNADJUDICABLE if shape_ok else INSUFFICIENT,
                baseline_note,
            )
        if not shape_ok:
            return _row(
                gate,
                fmt_ns(measured, ceiling),
                f"baseline +{gate['tolerance_pct']}%",
                INSUFFICIENT,
                "baseline is bound to a different harness shape",
            )
        baseline = gate["baseline_ns"]
        limit = baseline * (1 + gate["tolerance_pct"] / 100)
        return _row(
            gate,
            fmt_ns(measured, ceiling),
            f"<= {fmt_ns(limit, ceiling)} (baseline {fmt_ns(baseline, ceiling)} +{gate['tolerance_pct']}%)",
            PASS if measured <= limit else FAIL,
        )

    if estimator == "ratio_of_median_of_run_quantile":
        numerator = median_of_run_quantile(records, gate["numerator"], gate["quantile"])
        denominator = median_of_run_quantile(records, gate["denominator"], gate["quantile"])
        if denominator <= 0:
            raise QualificationError(f"{gate['id']} denominator median is not positive")
        ratio = numerator / denominator
        return _row(
            gate,
            f"{ratio:.2f}x",
            f">= {gate['minimum_ratio']:.2f}x",
            PASS if ratio >= gate["minimum_ratio"] else FAIL,
        )

    if estimator == "within_run_drift":
        ratio = within_run_drift(records, gate["metric"], gate["buckets"])
        return _row(
            gate,
            f"{ratio:.2f}x",
            f"<= {gate['maximum_ratio']:.2f}x",
            PASS if ratio <= gate["maximum_ratio"] else FAIL,
        )

    if estimator == "max_over_samples":
        measured = max(samples(records, gate["metric"]))
        limit = gate["maximum_ns"]
        return _row(gate, fmt_ns(measured, limit), f"<= {fmt_ns(limit, limit)}",
                    PASS if measured <= limit else FAIL)

    if estimator in {"max_of_run_field", "median_of_run_field"}:
        values = [dig(record, gate["field"]) for record in records]
        measured = max(values) if estimator == "max_of_run_field" else statistics.median(values)
        if "maximum_ns" in gate:
            limit = gate["maximum_ns"]
            return _row(gate, fmt_ns(measured, limit), f"<= {fmt_ns(limit, limit)}",
                        PASS if measured <= limit else FAIL)
        limit = gate["maximum"]
        scale = gate.get("scale", 1)
        return _row(gate, f"{measured / scale:g}", f"<= {limit / scale:g}",
                    PASS if measured <= limit else FAIL)

    if estimator == "stall_attribution":
        details = [
            attribute_cluster(record, cluster, gate)
            for record in records
            for cluster in stall_clusters(record, gate["metric"], gate["stall_threshold_ns"])
        ]
        unattributed = [detail for detail in details if not detail["attributed"]]
        row = _row(
            gate,
            f"{len(unattributed)} unattributed of {len(details)} clusters",
            f"<= {gate['max_unattributed_clusters']} unattributed",
            PASS if len(unattributed) <= gate["max_unattributed_clusters"] else FAIL,
        )
        row["clusters"] = details
        return row

    raise QualificationError(f"unknown estimator {estimator}")


# ---------------------------------------------------------------------------
# Diagnostics (recorded, never a verdict)
# ---------------------------------------------------------------------------


def evaluate_diagnostic(spec: dict, records: list[dict], bundle: pathlib.Path) -> dict:
    estimator = spec["estimator"]
    out = {"id": spec["id"], "group": spec["group"], "estimator": estimator,
           "note": spec.get("note", "")}

    if estimator == "run_incidence":
        per_run = [percentile_of(series(record, spec["metric"]), 0.99) for record in records]
        out["metric"] = spec["metric"]
        out["threshold_ns"] = spec["threshold_ns"]
        out["run_count"] = len(per_run)
        out["exceeding_runs"] = sum(value > spec["threshold_ns"] for value in per_run)
        out["per_run_p99_ns"] = per_run
    elif estimator == "pooled_distribution":
        shared = set.intersection(
            *[
                {k for k, v in record.items() if k.endswith("_ns") and isinstance(v, list) and v}
                for record in records
            ]
        )
        out["distributions"] = {
            key: distribution(samples(records, key)) for key in sorted(shared)
        }
    elif estimator == "stall_clusters":
        clusters = []
        for record in records:
            metric_samples = series(record, spec["metric"])
            for cluster in stall_clusters(record, spec["metric"], spec["threshold_ns"]):
                clusters.append({
                    "run": record.get("run"),
                    "indices": cluster,
                    "length": len(cluster),
                    "peak_ns": max(metric_samples[index] for index in cluster),
                })
        out["threshold_ns"] = spec["threshold_ns"]
        out["cluster_count"] = len(clusters)
        out["sample_count"] = sum(cluster["length"] for cluster in clusters)
        out["clusters"] = clusters
    elif estimator == "timed_untimed_split":
        out["runs"] = timed_untimed_split(bundle, records)
    else:
        raise QualificationError(f"unknown diagnostic estimator {estimator}")
    return out


def timed_untimed_split(bundle: pathlib.Path, records: list[dict]) -> list[dict] | str:
    """Process duration minus harness wall, per run.

    A host stall that lands in fixture setup instead of the timed loop must be
    visible in the record rather than silently scoring the run clean.
    """
    path = bundle / "run-conditions.jsonl"
    if not path.is_file():
        return "run-conditions.jsonl absent"
    phases: dict[int, dict[str, int]] = {}
    for row in load_jsonl(path):
        if row.get("group") != "result_finalize_stress":
            continue
        phases.setdefault(row["run"], {})[row["phase"]] = row["at_unix_ns"]
    split = []
    for record in records:
        run = record.get("run")
        marks = phases.get(run, {})
        if "before" not in marks or "after" not in marks:
            continue
        process_ns = marks["after"] - marks["before"]
        split.append({
            "run": run,
            "process_ns": process_ns,
            "harness_wall_ns": record["wall_ns"],
            "untimed_ns": process_ns - record["wall_ns"],
        })
    return split or "no matching run-conditions rows"


# ---------------------------------------------------------------------------
# Summary assembly
# ---------------------------------------------------------------------------


def summarize_group(records: list[dict], name: str, strict: bool = True) -> dict:
    present = [
        {
            key
            for key, value in record.items()
            if key.endswith("_ns") and isinstance(value, list) and value
        }
        for record in records
    ]
    union = sorted(set().union(*present))
    if strict:
        keys = union
        for record in records:
            for key in keys:
                if key not in record or not isinstance(record[key], list) or not record[key]:
                    raise QualificationError(
                        f"{name} run is missing a non-empty timing array {key}"
                    )
    else:
        # A retained bundle recorded before a metric existed keeps the metrics
        # every one of its runs carries; the rest become insufficient evidence.
        keys = sorted(set.intersection(*present)) if present else []
    return {
        "runs": len(records),
        "wall": distribution([record["wall_ns"] for record in records]),
        "operations": {key: distribution(samples(records, key)) for key in keys},
        "operations_per_run_p50": {
            key: distribution([percentile_of(record[key], 0.50) for record in records])
            for key in keys
        },
        "operations_per_run_p90": {
            key: distribution([percentile_of(record[key], 0.90) for record in records])
            for key in keys
        },
        "operations_per_run_p99": {
            key: distribution([percentile_of(record[key], 0.99) for record in records])
            for key in keys
        },
        "repository_lock_wait_max": distribution(
            [dig(record, "repository.repository_lock_wait_max_ns") for record in records]
        ),
        "append_fsync_barrier_max": distribution(
            [dig(record, "repository.append_fsync_barrier_max_ns") for record in records]
        ),
    }


def build_summary(bundle: pathlib.Path, mode: str, manifest: dict | None = None) -> dict:
    if mode not in {"benchmark", "qualify", "report", "baseline"}:
        raise QualificationError(f"unknown mode {mode}")
    manifest = manifest or load_manifest()
    strict = mode != "report"

    groups: dict[str, list[dict]] = {}
    run_count_notes: list[str] = []
    evidence_notes: list[str] = []
    for name, spec in manifest["groups"].items():
        path = bundle / spec["file"]
        if not path.is_file():
            # `report` must still open a historical bundle recorded before a
            # group existed; a group required in report mode never may.
            if strict or spec["required_in_report"]:
                raise QualificationError(f"missing qualification file {spec['file']}")
            run_count_notes.append(f"{name} absent from this bundle")
            continue
        records = load_jsonl(path)
        note = require_run_count(records, name, spec["runs"], strict)
        if note:
            run_count_notes.append(note)
        evidence_notes.extend(
            require_sample_shape(records, name, spec["sample_counts"], strict)
        )
        require_cas_mode(records, name, manifest["cas_retry_mode"])
        groups[name] = records

    if not groups:
        raise QualificationError("bundle contains no qualification group")
    shape = observed_shape_version(groups)
    shape_ok = shape == manifest["required_harness_shape_version"]
    environment = load_environment(bundle)
    recorded_contract = environment.get("qualification_contract_version")
    contract_ok = recorded_contract == manifest["qualification_contract_version"]
    if strict and not contract_ok:
        raise QualificationError(
            f"environment.json qualification_contract_version {recorded_contract!r} is not "
            f"{manifest['qualification_contract_version']}"
        )
    if strict and not shape_ok:
        raise QualificationError(
            f"records report harness_shape_version {shape!r}, contract requires "
            f"{manifest['required_harness_shape_version']}"
        )

    baseline_state = family_t_baseline_state(manifest)
    rows = []
    for gate in manifest["gates"]:
        records = groups.get(gate["group"])
        if records is None:
            rows.append(_row(gate, "-", "-", INSUFFICIENT, "group absent from this bundle"))
            continue
        try:
            rows.append(
                evaluate_gate(
                    gate,
                    records,
                    shape_ok=shape_ok and contract_ok,
                    mode=mode,
                    baseline_state=baseline_state,
                )
            )
        except QualificationError as exc:
            if strict:
                raise
            rows.append(_row(gate, "-", "-", INSUFFICIENT, str(exc)))

    diagnostics = []
    for spec in manifest["diagnostics"]:
        records = groups.get(spec["group"])
        if records is None:
            continue
        try:
            diagnostics.append(evaluate_diagnostic(spec, records, bundle))
        except QualificationError as exc:
            if strict:
                raise
            diagnostics.append(
                {"id": spec["id"], "group": spec["group"], "estimator": spec["estimator"],
                 "status": INSUFFICIENT, "note": str(exc)}
            )

    identity, bundle_path = bundle_identity_and_path(bundle)
    summary = {
        "schema_version": SUMMARY_SCHEMA_VERSION,
        "qualification_contract_version": manifest["qualification_contract_version"],
        "required_harness_shape_version": manifest["required_harness_shape_version"],
        "observed_harness_shape_version": shape,
        "source_sha": environment.get("git_sha"),
        "bundle_identity": identity,
        "bundle_path": bundle_path,
        "recorded_contract_version": recorded_contract,
        "completed_at_unix_ns": time.time_ns(),
        "run_count_notes": run_count_notes,
        "evidence_notes": evidence_notes,
        "gates": rows,
        "diagnostics": diagnostics,
        "groups": {
            name: summarize_group(records, name, strict) for name, records in groups.items()
        },
        "budget": budget_chain(manifest, groups, strict),
    }
    if mode == "baseline" and not _provenance_value_present(summary.get("source_sha")):
        raise QualificationError(
            "environment.json is missing git_sha; a baseline cannot bind (source SHA, harness shape)"
        )
    return summary


def budget_chain(manifest: dict, groups: dict[str, list[dict]], strict: bool = True) -> dict:
    """The measured worst-case finalize chain against the FinalizeBudget.

    One worst contended acquisition, one complete publication, one capped
    backoff, one retried publication.
    """
    finalize = groups.get("result-finalize")
    local = groups.get("local-ledger")
    if not finalize or not local:
        return {"status": INSUFFICIENT}
    try:
        lock_wait = max(
            dig(record, "repository.repository_lock_wait_max_ns") for record in local
        )
        full_max = max(samples(finalize, "full_terminal_finalize_ns"))
    except QualificationError as exc:
        if strict:
            raise
        return {"status": INSUFFICIENT, "note": str(exc)}
    chain = lock_wait + 2 * full_max + manifest["backoff_cap_ns"]
    budget = manifest["finalize_budget_ns"]
    return {
        "worst_lock_wait_ns": lock_wait,
        "worst_full_finalize_ns": full_max,
        "backoff_cap_ns": manifest["backoff_cap_ns"],
        "chain_ns": chain,
        "finalize_budget_ns": budget,
        "headroom": budget / chain if chain else None,
        "fits": chain <= budget,
    }


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------


def render_gate_table(manifest: dict | None = None) -> str:
    manifest = manifest or load_manifest()
    lines = [
        f"Contract v{manifest['qualification_contract_version']}, "
        f"harness shape {manifest['required_harness_shape_version']}, "
        f"attribution estimator revision {manifest['attribution_estimator_revision']}, "
        f"FinalizeBudget {manifest['finalize_budget_ns'] / 1_000_000_000:.0f} s.",
        "",
        "| Gate | Family | Group | Estimator | Limit |",
        "|---|:---:|---|---|---|",
    ]
    for gate in manifest["gates"]:
        lines.append(
            f"| `{gate['id']}` | {gate['family']} | {gate['group']} | "
            f"{gate['estimator']} | {gate_limit_text(gate)} |"
        )
    lines += ["", "Run counts:"]
    for name, spec in manifest["groups"].items():
        lines.append(f"- `{name}`: {spec['runs']} runs — {spec['runs_basis']}")
    return "\n".join(lines)


def gate_limit_text(gate: dict) -> str:
    if "equals" in gate:
        return f"= `{gate['equals']!r}`"
    if gate["estimator"] == "median_of_run_quantile":
        ceiling = fmt_ns(gate["absolute_max_ns"], gate["absolute_max_ns"])
        return f"baseline +{gate['tolerance_pct']}%, absolute <= {ceiling}"
    if gate["estimator"] == "ratio_of_median_of_run_quantile":
        return f">= {gate['minimum_ratio']:.2f}x"
    if gate["estimator"] == "within_run_drift":
        return f"<= {gate['maximum_ratio']:.2f}x"
    if "maximum_ns" in gate:
        return f"<= {fmt_ns(gate['maximum_ns'], gate['maximum_ns'])}"
    if "maximum" in gate:
        scale = gate.get("scale", 1)
        return f"<= {gate['maximum'] / scale:g}"
    if gate["estimator"] == "stall_attribution":
        return f"<= {gate['max_unattributed_clusters']} unattributed clusters"
    return "-"


def render_summary_md(summary: dict) -> str:
    lines = [
        "# InvocationStore qualification summary",
        "",
        f"- Qualification contract: v{summary['qualification_contract_version']}",
        f"- Harness shape: observed {summary['observed_harness_shape_version']}, "
        f"required {summary['required_harness_shape_version']}",
        f"- Source SHA: {summary.get('source_sha') or 'unrecorded'}",
        f"- Bundle: {summary.get('bundle_identity')} ({summary.get('bundle_path')})",
    ]
    for name, group in summary["groups"].items():
        lines.append(f"- `{name}` runs: {group['runs']}")
    for note in summary["run_count_notes"]:
        lines.append(f"- NOTE: {note}")
    budget = summary["budget"]
    if budget.get("fits") is not None:
        lines.append(
            f"- Budget chain: {budget['chain_ns'] / 1_000_000_000:.2f} s against "
            f"{budget['finalize_budget_ns'] / 1_000_000_000:.0f} s "
            f"({budget['headroom']:.2f}x headroom)"
        )
    lines += ["", "## Gates", "", "| Gate | Family | Measured | Limit | Verdict |", "|---|:---:|---:|---|:---:|"]
    for row in summary["gates"]:
        lines.append(
            f"| `{row['id']}` | {row['family']} | {row['measured']} | {row['limit']} | {row['verdict']} |"
        )
    lines += ["", "## Diagnostics (recorded, never a verdict)", ""]
    for diagnostic in summary["diagnostics"]:
        if diagnostic["estimator"] == "run_incidence":
            lines.append(
                f"- `{diagnostic['id']}`: {diagnostic['exceeding_runs']}/{diagnostic['run_count']} runs "
                f"exceed {diagnostic['threshold_ns'] / 1_000_000:.0f} ms (no K, no verdict)"
            )
        elif diagnostic["estimator"] == "stall_clusters":
            lines.append(
                f"- `{diagnostic['id']}`: {diagnostic['cluster_count']} clusters covering "
                f"{diagnostic['sample_count']} samples"
            )
        else:
            lines.append(f"- `{diagnostic['id']}`: see summary.json")
    lines += [
        "",
        "Thresholds and run counts come from `qualification-contract-v2.json`; this file "
        "restates none of them.",
        "",
    ]
    return "\n".join(lines)


def render_baseline(summary: dict, manifest: dict) -> str:
    """The family-T baseline fragment measured by a baseline campaign."""
    gates = {}
    for gate in manifest["gates"]:
        if gate["estimator"] != "median_of_run_quantile":
            continue
        row = next(row for row in summary["gates"] if row["id"] == gate["id"])
        group = summary["groups"].get(gate["group"])
        if not group:
            continue
        key = f"operations_per_run_p{int(gate['quantile'] * 100)}"
        gates[gate["id"]] = {
            "baseline_ns": group[key][gate["metric"]]["p50_ns"],
            "measured_row": row["measured"],
        }
    fragment = {
        "provenance": {
            "source_sha": summary.get("source_sha"),
            "qualification_contract_version": summary["qualification_contract_version"],
            "harness_shape_version": summary["observed_harness_shape_version"],
            "bundle_identity": summary.get("bundle_identity"),
            "bundle_path": summary.get("bundle_path"),
        },
        "gates": gates,
    }
    missing = [
        name
        for name in BASELINE_PROVENANCE_FIELDS
        if not _provenance_value_present(fragment["provenance"].get(name))
    ]
    if missing:
        raise QualificationError(
            f"baseline fragment is missing provenance fields: {missing}"
        )
    return json.dumps(fragment, indent=2, sort_keys=True)


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------


def enforcing_failures(summary: dict) -> tuple[list[str], list[str]]:
    failed = [row["id"] for row in summary["gates"] if row["verdict"] == FAIL]
    blocked = [
        row["id"] for row in summary["gates"] if row["verdict"] in {UNADJUDICABLE, INSUFFICIENT}
    ]
    return failed, blocked


def write_bundle_summary(bundle: pathlib.Path, summary: dict) -> None:
    with (bundle / "summary.json").open("w", encoding="utf-8") as handle:
        json.dump(summary, handle, indent=2, sort_keys=True)
        handle.write("\n")
    (bundle / "summary.md").write_text(render_summary_md(summary), encoding="utf-8")


def run(
    bundle: pathlib.Path,
    mode: str,
    write: bool | None = None,
    manifest: dict | None = None,
) -> int:
    """Return the process exit code for `mode`.

    `benchmark` and `qualify` write `summary.json` / `summary.md`. `report` and
    `baseline` are read-only so inspecting a retained bundle cannot rewrite its bytes.
    Benchmark mode remains strict about input shape but does not turn diagnostic gate
    verdicts into a blocking process exit.
    """
    if write is None:
        write = mode in {"benchmark", "qualify"}
    try:
        manifest = manifest or load_manifest()
        summary = build_summary(bundle, mode, manifest)
    except QualificationError as exc:
        print(f"qualification input failed closed: {exc}", file=sys.stderr)
        return 2
    if write:
        write_bundle_summary(bundle, summary)
    if mode == "baseline":
        try:
            print(render_baseline(summary, manifest))
        except QualificationError as exc:
            print(f"qualification input failed closed: {exc}", file=sys.stderr)
            return 2
        return 0
    for row in summary["gates"]:
        print(f"{row['id']}: {row['measured']} vs {row['limit']}: {row['verdict']}")
    failed, blocked = enforcing_failures(summary)
    if mode == "qualify" and (failed or blocked):
        if failed:
            print("enforcing gate failed:", "; ".join(failed), file=sys.stderr)
        if blocked:
            print("enforcing gate not adjudicable:", "; ".join(blocked), file=sys.stderr)
        return 1
    return 0


def check_doc(path: pathlib.Path) -> int:
    if not path.is_file():
        print(f"missing document {path}", file=sys.stderr)
        return 2
    text = path.read_text(encoding="utf-8")
    if GATE_TABLE_BEGIN not in text or GATE_TABLE_END not in text:
        print(f"{path.name} has no generated gate-table block", file=sys.stderr)
        return 2
    embedded = text.split(GATE_TABLE_BEGIN, 1)[1].split(GATE_TABLE_END, 1)[0].strip()
    expected = render_gate_table().strip()
    if embedded != expected:
        print(f"{path.name} gate table has drifted from the manifest", file=sys.stderr)
        return 1
    return 0


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) == 1 and args[0] == "render-gates":
        print(render_gate_table())
        return 0
    if len(args) == 2 and args[0] == "check-doc":
        return check_doc(pathlib.Path(args[1]))
    if len(args) == 2 and args[0] in {"benchmark", "qualify", "report", "baseline"}:
        return run(pathlib.Path(args[1]), args[0])
    print(
        "usage: summarize_qualification.py benchmark|qualify|report|baseline BUNDLE_DIR\n"
        "       summarize_qualification.py render-gates\n"
        "       summarize_qualification.py check-doc DOC",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main())
