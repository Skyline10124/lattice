#!/usr/bin/env bash
# scripts/p27-real-dogfood.sh — manual real-LLM dogfood checklist
#
# MANUAL CHECKLIST — NOT CALLED FROM CI OR scripts/release-gate-v1.3.sh
#
# This script exercises the full CEP stack (declare + workspace.patch +
# command.run + verify) with a real LLM provider. It complements the offline
# fake-LLM e2e test without becoming part of the release gate.
#
# Required env:
#   ANTHROPIC_API_KEY   (or one of the alternatives below)
#
# Alternative providers:
#   OPENAI_API_KEY + P27_MODEL=openai:<model>
#   OPENAI_COMPATIBLE_API_KEY + OPENAI_COMPATIBLE_BASE_URL + P27_MODEL=compat:<model>
#
# Optional:
#   P27_MODEL=sonnet           (default; can also use "haiku", "opus", "compat:…")
#   P27_DOGFOOD_KEEP=1         (keep temp workspace; default: delete on exit)
#   P27_DOGFOOD_REPORT=<path>  (default: build/tmp/p27-dogfood/report.md)
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MODEL="${P27_MODEL:-sonnet}"
BASE="${ROOT}/build/tmp/p27-dogfood"
REPORT="${P27_DOGFOOD_REPORT:-${BASE}/report.md}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
RUN_DIR="${BASE}/${RUN_ID}"

require_one_provider() {
  if [[ -z "${ANTHROPIC_API_KEY:-}" ]] && \
     [[ -z "${OPENAI_API_KEY:-}" ]] && \
     [[ -z "${OPENAI_COMPATIBLE_API_KEY:-}" ]]; then
    echo "FAIL: set at least one of ANTHROPIC_API_KEY, OPENAI_API_KEY, or" >&2
    echo "      OPENAI_COMPATIBLE_API_KEY + OPENAI_COMPATIBLE_BASE_URL" >&2
    exit 2
  fi
}

echo "================================================================"
echo " P27 Layer-2 real-LLM dogfood checklist"
echo " $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
echo " model: $MODEL"
echo " report: $REPORT"
echo "================================================================"
echo ""
echo "MANUAL CHECKLIST — not CI, not in release gate."
echo "Verify each scenario manually after runs complete."
echo ""

require_one_provider

# ── Build CLI ─────────────────────────────────────────────────────────────────
echo "==> Building lattice CLI..."
cargo build -p lattice-cli --quiet
LATTICE="${ROOT}/target/debug/lattice"
echo "    binary: $LATTICE"
echo ""

# ── Workspace setup ───────────────────────────────────────────────────────────
mkdir -p "${RUN_DIR}"
WS="${RUN_DIR}/workspace"
mkdir -p "${WS}/.lattice/agents/default"

cat >"${WS}/.lattice/agents/default/agent.toml" <<EOF
schema_version = 1

[agent]
name = "p27-dogfood"
model = "${MODEL}"

[tools]
enabled = [
  "envelope.declare",
  "workspace.read_file",
  "workspace.grep",
  "workspace.list_directory",
  "workspace.find_files",
  "workspace.write_file",
  "workspace.patch",
  "command.run",
]

[cep]
mode = "enforce"
EOF

mkdir -p "${WS}/src"
cat >"${WS}/src/counter.rs" <<'EOF'
pub fn count(items: &[u32]) -> usize {
    items.len()
}
EOF
cat >"${WS}/Cargo.toml" <<'EOF'
[package]
name = "p27-dogfood-fixture"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "counter"
path = "src/counter.rs"

[workspace]
EOF

: >"${REPORT}"
log() { echo "$*" | tee -a "${REPORT}"; }

log "# P27 Layer-2 real-LLM dogfood report"
log ""
log "- run_id: ${RUN_ID}"
log "- model: ${MODEL}"
log "- workspace: ${WS}"
log "- binary: ${LATTICE}"
log ""

# ── Scenario 1: CEP declare + read + patch ────────────────────────────────────
log "## Scenario 1: declare + workspace.patch (CEP mutation path)"
log ""
log "Prompt: Read src/counter.rs, declare a mutation envelope, patch it to add a"
log "  doc comment, and verify via cargo check."
log ""

JSONL1="${RUN_DIR}/s1.jsonl"
STDERR1="${RUN_DIR}/s1.stderr"

set +e
(
  cd "${WS}"
  "${LATTICE}" --json run \
    'Use CEP tools. envelope.declare with intent "p27 patch counter", WritePath ["src/counter.rs"], verify command "cargo check --manifest-path Cargo.toml", expected_exit_code 0, on_failure retain_diff. Then workspace.patch to add a /// doc comment above the count function. Final message: P27_SCENARIO_1_DONE' \
    -m "${MODEL}" \
    >"${JSONL1}" 2>"${STDERR1}"
)
S1_EXIT=$?
set -e

log "- exit: ${S1_EXIT}"
log ""
log "### Manual checklist"
log "- [ ] JSONL contains cep_envelope_declared event"
log "- [ ] JSONL contains turn_committed event (not turn_failed)"
log "- [ ] src/counter.rs has a doc comment"
log "- [ ] cep_audit.db exists with ≥2 rows"
log ""

# ── Scenario 2: CEP declare + command.run ─────────────────────────────────────
log "## Scenario 2: declare + command.run (command enforcement path)"
log ""
JSONL2="${RUN_DIR}/s2.jsonl"
STDERR2="${RUN_DIR}/s2.stderr"

set +e
(
  cd "${WS}"
  "${LATTICE}" --json run \
    'Use CEP tools. envelope.declare with intent "p27 run echo", RunCommand ["echo p27-ok"], verify command "echo p27-ok", expected_exit_code 0, on_failure retain_diff. Then command.run "echo p27-ok". Final message: P27_SCENARIO_2_DONE' \
    -m "${MODEL}" \
    >"${JSONL2}" 2>"${STDERR2}"
)
S2_EXIT=$?
set -e

log "- exit: ${S2_EXIT}"
log ""
log "### Manual checklist"
log "- [ ] JSONL contains cep_envelope_declared event"
log "- [ ] JSONL contains tool_call_finished for command.run with ok=true"
log "- [ ] No cep_verify_failed events (verify passed)"
log "- [ ] turn_committed present"
log ""

# ── Summary ───────────────────────────────────────────────────────────────────
log "## Summary"
log ""
log "| Scenario | Exit | Notes |"
log "|----------|------|-------|"
log "| 1: declare+patch | ${S1_EXIT} | inspect ${JSONL1} |"
log "| 2: declare+command.run | ${S2_EXIT} | inspect ${JSONL2} |"
log ""
log "Inspect JSONL files with:"
log "  jq -c 'select(.type | test(\"^(cep_|tool_call_)\"))' ${JSONL1}"
log "  jq -c 'select(.type | test(\"^(cep_|tool_call_)\"))' ${JSONL2}"
log ""

if [[ "${P27_DOGFOOD_KEEP:-}" == "1" ]]; then
  log "- workspace retained: ${RUN_DIR}"
  echo "Retained: ${RUN_DIR}"
else
  log "- workspace deleted (set P27_DOGFOOD_KEEP=1 to keep)"
  rm -rf "${RUN_DIR}"
fi

echo ""
echo "Report: ${REPORT}"
echo ""
echo "MANUAL — verify checklist items above before signing off Layer-2 dogfood."
