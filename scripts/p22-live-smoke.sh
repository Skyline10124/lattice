#!/usr/bin/env bash
# Live smoke for the production CLI turn path with an OpenAI-compatible model.
#
# Required env (values must NOT be logged):
#   OPENAI_COMPATIBLE_API_KEY
#   OPENAI_COMPATIBLE_BASE_URL
#
# Optional:
#   P22_SMOKE_MODEL   (default: compat:deepseek-v4-pro)
#   P22_SMOKE_BIN     (default: build lattice-cli, run target/debug/lattice)
#   P22_KEEP_SMOKE_HOME (default: unset — delete temp workspace on exit)
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MODEL="${P22_SMOKE_MODEL:-compat:deepseek-v4-pro}"
AGENT_NAME="p22-smoke"
# Use a non-world-writable path (lattice-cli rejects /tmp for agent profiles).
SMOKE_BASE="${ROOT}/build/tmp/p22-smoke"
mkdir -p "${SMOKE_BASE}"
SMOKE_HOME="$(mktemp -d "${SMOKE_BASE}/run-XXXXXX")"
JSONL="${SMOKE_HOME}/events.jsonl"
AUDIT_DB="${SMOKE_HOME}/.lattice/cep_audit.db"

cleanup() {
  if [[ "${P22_KEEP_SMOKE_HOME:-}" != "1" ]]; then
    rm -rf "$SMOKE_HOME"
  else
    echo "Keeping smoke workspace: ${SMOKE_HOME}"
  fi
}
trap cleanup EXIT

require_env() {
  if [[ -z "${!1:-}" ]]; then
    echo "FAIL: $1 is not set (live smoke cannot run)" >&2
    exit 2
  fi
  echo "OK: $1 is set"
}

echo "== CEP live smoke preflight =="
require_env OPENAI_COMPATIBLE_API_KEY
require_env OPENAI_COMPATIBLE_BASE_URL

echo "Model target: ${MODEL}"
echo "Workspace: ${SMOKE_HOME}"

mkdir -p "${SMOKE_HOME}/smoke" "${SMOKE_HOME}/.lattice/agents/${AGENT_NAME}"
printf 'version: 1\n' >"${SMOKE_HOME}/smoke/fixture.txt"

cat >"${SMOKE_HOME}/.lattice/agents/${AGENT_NAME}/agent.toml" <<EOF
schema_version = 1

[agent]
name = "${AGENT_NAME}"
model = "${MODEL}"

[prompt]
system = """
You are executing a CEP live smoke test. Use tools only; keep assistant text minimal.

Required sequence (do not skip or reorder):
1. envelope.declare — intent "CEP live smoke"; WritePath smoke/**; RunCommand allowlist must include echo with args P22_SMOKE_OK; verify command: echo verify-ok (diagnostic allowlist only; never use test/true/[); expected_exit_code 0; on_failure retain_diff.
2. workspace.read_file — path smoke/fixture.txt
3. workspace.grep — pattern "version" under smoke (use path smoke or smoke/fixture.txt)
4. command.run — argv ["echo", "P22_SMOKE_OK"] (diagnostic)
5. workspace.write_file — path smoke/out.txt content "ok\n" (creates the file verify expects)
6. Final assistant message must be exactly: P22_SMOKE_COMPLETE

Never write outside smoke/. Do not use web.fetch.
"""

[tools]
enabled = [
  "envelope.declare",
  "workspace.read_file",
  "workspace.grep",
  "workspace.list_directory",
  "workspace.write_file",
  "workspace.patch",
  "command.run",
]
EOF
# heredoc above is unquoted so \${MODEL} is expanded; prompt/system text has no shell expansions.

PROMPT='Execute the CEP smoke sequence from your system prompt now.'

if [[ -n "${P22_SMOKE_BIN:-}" ]]; then
  # shellcheck disable=SC2206
  LATTICE=(${P22_SMOKE_BIN})
else
  cargo build -p lattice-cli --quiet
  LATTICE=("${ROOT}/target/debug/lattice")
fi

echo "== CEP live smoke: resolve =="
"${LATTICE[@]}" resolve "${MODEL}" --verbose

echo "== CEP live smoke: session send (JSONL) =="
set +e
(
  cd "${SMOKE_HOME}"
  HOME="${SMOKE_HOME}" XDG_CONFIG_HOME="${SMOKE_HOME}/.config" \
    "${LATTICE[@]}" --json session new --agent "${AGENT_NAME}" >"${SMOKE_HOME}/session.json" 2>"${SMOKE_HOME}/run.stderr"
)
RUN_EXIT=$?
SESSION_ID=""
if [[ ${RUN_EXIT} -eq 0 ]]; then
  SESSION_ID="$(jq -r '.session_id // empty' "${SMOKE_HOME}/session.json" 2>>"${SMOKE_HOME}/run.stderr")"
  if [[ -z "${SESSION_ID}" ]]; then
    echo "FAIL: session new did not return a session_id" >>"${SMOKE_HOME}/run.stderr"
    RUN_EXIT=2
  fi
fi
if [[ ${RUN_EXIT} -eq 0 ]]; then
  (
    cd "${SMOKE_HOME}"
    HOME="${SMOKE_HOME}" XDG_CONFIG_HOME="${SMOKE_HOME}/.config" \
      "${LATTICE[@]}" --json session send "${SESSION_ID}" "${PROMPT}" >"${JSONL}" 2>>"${SMOKE_HOME}/run.stderr"
  )
  RUN_EXIT=$?
fi
set -e

echo "lattice session send exit code: ${RUN_EXIT}"
echo "--- stderr (last 40 lines) ---"
tail -n 40 "${SMOKE_HOME}/run.stderr" || true

verify_jsonl() {
  local fail=0
  jq_check() {
    # JSONL: slurp newline-delimited TurnEvent objects into one array.
    if ! jq -es "$1" "${JSONL}" >/dev/null 2>&1; then
      echo "FAIL jsonl: $2" >&2
      fail=1
    else
      echo "OK jsonl: $2"
    fi
  }

  jq_check 'any(.[]; .type == "cep_envelope_declared")' 'cep_envelope_declared'
  jq_check 'any(.[]; .type == "cep_call_started")' 'cep_call_started'
  jq_check 'any(.[]; .type == "cep_call_finished")' 'cep_call_finished'
  jq_check 'any(.[]; .type == "tool_call_finished" and .name == "envelope.declare" and .ok == true)' 'envelope.declare ok'
  jq_check 'any(.[]; .type == "tool_call_finished" and .name == "workspace.read_file" and .ok == true)' 'workspace.read_file ok'
  jq_check 'any(.[]; .type == "tool_call_finished" and (.name == "workspace.grep" or .name == "workspace.list_directory") and .ok == true)' 'observation tool ok'
  jq_check 'any(.[]; .type == "tool_call_finished" and .name == "command.run" and .ok == true)' 'command.run ok'
  jq_check 'any(.[]; .type == "tool_call_finished" and .name == "workspace.write_file" and .ok == true)' 'workspace.write_file ok'
  jq_check 'any(.[]; .type == "turn_committed")' 'turn_committed'
  jq_check 'all(.[]; .type != "cep_verify_failed")' 'no cep_verify_failed on happy path'

  # CEP guard verdict may appear for bridge/policy paths; not required on every native call.
  if jq -es 'any(.[]; .type == "cep_guard_verdict")' "${JSONL}" >/dev/null 2>&1; then
    echo "OK jsonl: cep_guard_verdict present (optional)"
  else
    echo "NOTE jsonl: cep_guard_verdict not seen (acceptable for all-native path)"
  fi

  if [[ -f "${SMOKE_HOME}/smoke/out.txt" ]] && grep -q '^ok$' "${SMOKE_HOME}/smoke/out.txt"; then
    echo "OK workspace: smoke/out.txt written"
  else
    echo "FAIL workspace: smoke/out.txt missing or wrong content" >&2
    fail=1
  fi

  return "$fail"
}

verify_audit() {
  local fail=0
  if [[ ! -f "${AUDIT_DB}" ]]; then
    echo "FAIL audit: ${AUDIT_DB} not found" >&2
    return 1
  fi
  local count
  count="$(sqlite3 "${AUDIT_DB}" 'SELECT COUNT(*) FROM cep_audit_entries;')"
  if [[ "${count}" -lt 3 ]]; then
    echo "FAIL audit: expected >= 3 rows, got ${count}" >&2
    fail=1
  else
    echo "OK audit: ${count} cep_audit_entries rows"
  fi
  sqlite3 -header -column "${AUDIT_DB}" \
    'SELECT audit_id, method, result_or_error, impact_kind FROM cep_audit_entries ORDER BY audit_id LIMIT 20;'

  # Ensure metadata_json does not embed env secrets (only checks key name substrings).
  if sqlite3 "${AUDIT_DB}" "SELECT metadata_json FROM cep_audit_entries WHERE metadata_json LIKE '%OPENAI_COMPATIBLE_API_KEY%';" | grep -q .; then
    echo "FAIL audit: metadata_json may contain secret key name" >&2
    fail=1
  else
    echo "OK audit: no API key material in metadata_json"
  fi
  return "$fail"
}

JSONL_FAIL=0
AUDIT_FAIL=0
verify_jsonl || JSONL_FAIL=1
verify_audit || AUDIT_FAIL=1

if [[ "${RUN_EXIT}" -ne 0 ]] || [[ "${JSONL_FAIL}" -ne 0 ]] || [[ "${AUDIT_FAIL}" -ne 0 ]]; then
  echo "CEP live smoke: FAIL (run_exit=${RUN_EXIT} jsonl=${JSONL_FAIL} audit=${AUDIT_FAIL})" >&2
  echo "--- JSONL sample (tool + cep events) ---"
  jq -c 'select(.type | test("^(tool_call_|cep_)"))' "${JSONL}" 2>/dev/null | head -n 40 || true
  # slurp fallback for debug
  jq -es '[.[] | select(.type | test("^(tool_call_|cep_)"))] | .[0:40]' "${JSONL}" 2>/dev/null || true
  exit 1
fi

echo "CEP live smoke: PASS"
