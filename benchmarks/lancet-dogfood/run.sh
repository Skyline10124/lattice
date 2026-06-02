#!/usr/bin/env bash
# Systematic real-LLM LANCET dogfood benchmark.
#
# This is a benchmark harness, not a release-gate smoke test. It exercises the
# current 9-method LANCET catalog through the production CLI turn path and
# records JSONL/summary/report artifacts for comparison across models.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

MODEL="${LANCET_DOGFOOD_MODEL:-sonnet}"
AGENT_NAME="${LANCET_DOGFOOD_AGENT:-lancet-dogfood}"
BASE="${LANCET_DOGFOOD_BASE:-${ROOT}/build/tmp/benchmarks/lancet-dogfood}"
REPORT="${LANCET_DOGFOOD_REPORT:-${BASE}/report.md}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
RUN_DIR="${BASE}/${RUN_ID}"
SUMMARY="${RUN_DIR}/benchmark-summary.json"

declare -a LATTICE=()
declare -a SCENARIOS=()
declare -a MATRIX_SLUG=()
declare -a MATRIX_RESULT=()
declare -a MATRIX_DETAIL=()

usage() {
  cat <<'EOF'
Usage: benchmarks/lancet-dogfood/run.sh [options]

Options:
  --scenario <smoke|mutation|command|all>
      Run one scenario. Repeat for multiple scenarios. Default: all.
  --model <alias>
      Override LANCET_DOGFOOD_MODEL.
  --report <path>
      Override LANCET_DOGFOOD_REPORT.
  --keep
      Keep the per-run workspace under build/tmp/benchmarks/lancet-dogfood/.
  -h, --help
      Show this help.

Required provider env:
  ANTHROPIC_API_KEY
    or OPENAI_API_KEY
    or OPENAI_COMPATIBLE_API_KEY + OPENAI_COMPATIBLE_BASE_URL

Optional env:
  LANCET_DOGFOOD_MODEL       default: sonnet
  LANCET_DOGFOOD_BIN         existing lattice binary/command
  LANCET_DOGFOOD_BASE        default: build/tmp/benchmarks/lancet-dogfood
  LANCET_DOGFOOD_REPORT      default: $BASE/report.md
  LANCET_DOGFOOD_SCENARIOS   comma list, e.g. smoke,command
  LANCET_DOGFOOD_KEEP=1      retain workspace
EOF
}

add_scenario() {
  local scenario="$1"
  case "$scenario" in
    all)
      add_scenario smoke
      add_scenario mutation
      add_scenario command
      return 0
      ;;
    smoke|mutation|command)
      ;;
    *)
      echo "FAIL: unknown scenario '${scenario}'" >&2
      exit 2
      ;;
  esac

  local existing
  for existing in "${SCENARIOS[@]}"; do
    if [[ "$existing" == "$scenario" ]]; then
      return 0
    fi
  done
  SCENARIOS+=("$scenario")
}

parse_scenario_csv() {
  local csv="$1"
  local old_ifs="$IFS"
  local part
  local -a parts
  IFS=',' read -r -a parts <<<"$csv"
  IFS="$old_ifs"
  for part in "${parts[@]}"; do
    part="${part//[[:space:]]/}"
    [[ -z "$part" ]] || add_scenario "$part"
  done
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --scenario)
        [[ $# -ge 2 ]] || { echo "FAIL: --scenario needs a value" >&2; exit 2; }
        add_scenario "$2"
        shift 2
        ;;
      --model)
        [[ $# -ge 2 ]] || { echo "FAIL: --model needs a value" >&2; exit 2; }
        MODEL="$2"
        shift 2
        ;;
      --report)
        [[ $# -ge 2 ]] || { echo "FAIL: --report needs a value" >&2; exit 2; }
        REPORT="$2"
        shift 2
        ;;
      --keep)
        export LANCET_DOGFOOD_KEEP=1
        shift
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        echo "FAIL: unknown argument '$1'" >&2
        usage >&2
        exit 2
        ;;
    esac
  done

  if [[ ${#SCENARIOS[@]} -eq 0 ]]; then
    if [[ -n "${LANCET_DOGFOOD_SCENARIOS:-}" ]]; then
      parse_scenario_csv "$LANCET_DOGFOOD_SCENARIOS"
    else
      add_scenario all
    fi
  fi
}

log() {
  echo "$*" | tee -a "$REPORT"
}

require_one_provider() {
  if [[ -n "${OPENAI_COMPATIBLE_API_KEY:-}" ]] && \
     [[ -z "${OPENAI_COMPATIBLE_BASE_URL:-}" ]]; then
    echo "FAIL: OPENAI_COMPATIBLE_BASE_URL is required with OPENAI_COMPATIBLE_API_KEY" >&2
    exit 2
  fi

  if [[ -z "${ANTHROPIC_API_KEY:-}" ]] && \
     [[ -z "${OPENAI_API_KEY:-}" ]] && \
     [[ -z "${OPENAI_COMPATIBLE_API_KEY:-}" ]]; then
    echo "FAIL: set ANTHROPIC_API_KEY, OPENAI_API_KEY, or OPENAI_COMPATIBLE_API_KEY" >&2
    exit 2
  fi
}

preflight_tools() {
  local missing=()
  local cmd
  for cmd in jq cargo git; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
      missing+=("$cmd")
    fi
  done
  if [[ ${#missing[@]} -gt 0 ]]; then
    echo "FAIL: required tools not found: ${missing[*]}" >&2
    exit 2
  fi
}

ensure_lattice() {
  if [[ -n "${LANCET_DOGFOOD_BIN:-}" ]]; then
    # shellcheck disable=SC2206
    LATTICE=(${LANCET_DOGFOOD_BIN})
  else
    cargo build -p lattice-cli --quiet
    LATTICE=("${ROOT}/target/debug/lattice")
  fi
}

write_agent_profile() {
  local ws="$1"
  local system_prompt="$2"
  mkdir -p "${ws}/.lattice/agents/${AGENT_NAME}"
  cat >"${ws}/.lattice/agents/${AGENT_NAME}/agent.toml" <<EOF
schema_version = 1

[agent]
name = "${AGENT_NAME}"
model = "${MODEL}"

[prompt]
system = $(printf '%s' "$system_prompt" | jq -Rs .)

[tools]
enabled = [
  "session.envelope",
  "observe.workspace",
  "mutate.create",
  "mutate.patch",
  "exec.command_run",
  "session.todo_write",
]

[lancet]
mode = "enforce"

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
EOF
}

run_lattice_turn() {
  local ws="$1"
  local prompt="$2"
  local jsonl="$3"
  local stderr="$4"
  local session_json="${jsonl%.jsonl}.session.json"
  local session_id=""
  local status=0

  : >"$jsonl"
  : >"$stderr"

  set +e
  (
    cd "$ws"
    HOME="$ws" XDG_CONFIG_HOME="${ws}/.config" CARGO_TARGET_DIR="${ws}/target" \
      "${LATTICE[@]}" --json session new --agent "$AGENT_NAME" >"$session_json" 2>"$stderr"
  )
  status=$?
  if [[ $status -eq 0 ]]; then
    session_id="$(jq -r '.session_id // empty' "$session_json" 2>>"$stderr")"
    if [[ -z "$session_id" ]]; then
      echo "FAIL: session new did not return a session_id" >>"$stderr"
      status=2
    fi
  fi
  if [[ $status -eq 0 ]]; then
    (
      cd "$ws"
      HOME="$ws" XDG_CONFIG_HOME="${ws}/.config" CARGO_TARGET_DIR="${ws}/target" \
        "${LATTICE[@]}" --json session send "$session_id" "$prompt" >"$jsonl" 2>>"$stderr"
    )
    status=$?
  fi
  set -e

  return "$status"
}

audit_row_count() {
  local ws="$1"
  local db="${ws}/.lattice/cep_audit.db"
  if command -v sqlite3 >/dev/null 2>&1 && [[ -f "$db" ]]; then
    sqlite3 "$db" 'SELECT COUNT(*) FROM cep_audit_entries;' 2>/dev/null || echo "null"
  else
    echo "null"
  fi
}

build_summary() {
  local jsonl="$1"
  local summary="$2"
  local exit_code="$3"
  local ws="$4"
  local audit_rows
  audit_rows="$(audit_row_count "$ws")"
  if [[ "$audit_rows" != "null" ]] && ! [[ "$audit_rows" =~ ^[0-9]+$ ]]; then
    audit_rows="null"
  fi

  jq -s --argjson exit_code "$exit_code" --argjson audit_rows "$audit_rows" '
    {
      exit_code: $exit_code,
      event_count: length,
      turn_committed: any(.[]; .type == "turn_committed"),
      turn_failed: any(.[]; .type == "turn_failed"),
      terminal_reasons: [
        .[]
        | select(.type == "turn_failed")
        | .reason
      ],
      tool_calls: [
        .[]
        | select(.type == "tool_call_finished")
        | {name, ok, error_summary}
      ],
      lancet: {
        call_started: ([.[] | select(.type == "cep_call_started")] | length),
        call_finished: ([.[] | select(.type == "cep_call_finished")] | length),
        envelope_declared: ([.[] | select(.type == "cep_envelope_declared")] | length),
        envelope_superseded: ([.[] | select(.type == "cep_envelope_superseded")] | length),
        verify_failed: ([.[] | select(.type == "cep_verify_failed")] | length),
        replan_required: ([.[] | select(.type == "cep_replan_required")] | length),
        audit_rows: $audit_rows
      },
      usage: (
        reduce (.[] | select(.type == "model_finished" and .usage != null) | .usage) as $u
        ({prompt_tokens: 0, completion_tokens: 0, total_tokens: 0};
          .prompt_tokens += ($u.prompt_tokens // 0)
          | .completion_tokens += ($u.completion_tokens // 0)
          | .total_tokens += ($u.total_tokens // 0)
        )
      )
    }
  ' "$jsonl" >"$summary"
}

declare -a SCENARIO_NOTES=()
SCENARIO_FAIL=0

note() { SCENARIO_NOTES+=("$1"); }
fail() { SCENARIO_FAIL=1; note "FAIL: $1"; }
pass() { note "PASS: $1"; }

assert_jsonl() {
  local jsonl="$1"
  local filter="$2"
  local message="$3"
  if jq -e -s "$filter" "$jsonl" >/dev/null 2>&1; then
    pass "$message"
  else
    fail "$message"
  fi
}

assert_tool_ok() {
  local jsonl="$1"
  local tool="$2"
  if jq -e -s --arg tool "$tool" \
    'any(.[]; .type == "tool_call_finished" and .name == $tool and .ok == true)' \
    "$jsonl" >/dev/null 2>&1; then
    pass "tool ${tool} finished ok"
  else
    fail "tool ${tool} did not finish ok"
  fi
}

common_judgment() {
  local exit_code="$1"
  local jsonl="$2"
  if [[ "$exit_code" -eq 0 ]]; then
    pass "lattice exit code is 0"
  else
    fail "lattice exit code is ${exit_code}"
  fi
  assert_jsonl "$jsonl" 'any(.[]; .type == "turn_committed")' "turn_committed present"
  assert_jsonl "$jsonl" 'all(.[]; .type != "turn_failed")' "turn_failed absent"
  assert_jsonl "$jsonl" 'any(.[]; .type == "cep_envelope_declared")' "cep_envelope_declared present"
  assert_tool_ok "$jsonl" "session.envelope"
}

append_scenario_report() {
  local slug="$1"
  local result="$2"
  local ws="$3"
  local jsonl="$4"
  local stderr="$5"
  local summary="$6"

  log ""
  log "## Scenario ${slug}"
  log ""
  log "- result: ${result}"
  log "- workspace: \`${ws}\`"
  log "- jsonl: \`${jsonl}\`"
  log "- summary: \`${summary}\`"
  log ""
  log "### Summary"
  log '```json'
  jq '.' "$summary" | tee -a "$REPORT" >/dev/null
  log '```'
  if [[ -s "$stderr" ]]; then
    log ""
    log "### stderr tail"
    log '```text'
    tail -n 60 "$stderr" | tee -a "$REPORT" >/dev/null
    log '```'
  fi
  log ""
  log "### Judgment"
  local n
  for n in "${SCENARIO_NOTES[@]}"; do
    log "- ${n}"
  done
}

record_scenario() {
  MATRIX_SLUG+=("$1")
  MATRIX_RESULT+=("$2")
  MATRIX_DETAIL+=("$3")
}

smoke_system_prompt() {
  cat <<'EOF'
You are executing the LANCET dogfood benchmark smoke scenario.

Use only current LANCET tool names: session.envelope, observe.workspace, mutate.create, mutate.patch, exec.command_run, session.todo_write. Do not invent aliases or use retired workspace, command, or envelope tool names.

Required sequence:
1. Call session.envelope with mode=declare. The envelope intent is "lancet dogfood smoke". Grant WritePath for smoke/** and RunCommand with argv_allowlist containing "echo". Include impact entries for workspace_mutation target smoke/out.txt and command_execution target echo. Include verify command "echo verify-ok", expected_exit_code 0, and on_failure retain_diff.
2. Call observe.workspace with mode=read_file for smoke/fixture.txt, line_numbers true.
3. Call observe.workspace with mode=grep for pattern "version" under smoke, max_results 10.
4. Call mutate.create to create smoke/out.txt with content exactly "ok\n".
5. Call exec.command_run with argv ["echo", "LANCET_SMOKE_OK"] and declared_impact workspace_observation.
6. Final assistant message exactly: LANCET_SMOKE_COMPLETE

Do not write outside smoke/.
EOF
}

mutation_system_prompt() {
  cat <<'EOF'
You are executing the LANCET dogfood benchmark mutation scenario.

Use only current LANCET tool names: session.envelope, observe.workspace, mutate.create, mutate.patch, exec.command_run, session.todo_write. Do not invent aliases or use retired workspace, command, or envelope tool names.

Required sequence:
1. Call session.envelope with mode=declare. The envelope intent is "lancet dogfood patch counter". Grant WritePath for src/counter.rs and RunCommand with argv_allowlist containing "cargo check". Include impact entries for workspace_mutation target src/counter.rs and command_execution target cargo check. Include verify command "cargo check --manifest-path Cargo.toml", expected_exit_code 0, and on_failure retain_diff.
2. Call observe.workspace with mode=read_file for src/counter.rs, line_numbers true.
3. Call mutate.patch to add a Rust doc comment immediately above pub fn count. Use the content_hash and line anchors returned by observe.workspace. Do not use mutate.create and do not use git unified diff syntax.
4. Final assistant message exactly: LANCET_MUTATION_COMPLETE

Do not call exec.command_run for the verify command; verify is part of the envelope/mutation path.
EOF
}

command_system_prompt() {
  cat <<'EOF'
You are executing the LANCET dogfood benchmark command scenario.

Use only current LANCET tool names: session.envelope, observe.workspace, mutate.create, mutate.patch, exec.command_run, session.todo_write. Do not invent aliases or use retired workspace, command, or envelope tool names.

Required sequence:
1. Call session.envelope with mode=declare. The envelope intent is "lancet dogfood command". Grant RunCommand with argv_allowlist containing "echo". Include a command_execution impact target echo. Use no WritePath capability.
2. Call exec.command_run with argv ["echo", "LANCET_COMMAND_OK"] and declared_impact workspace_observation.
3. Final assistant message exactly: LANCET_COMMAND_COMPLETE
EOF
}

prepare_smoke_workspace() {
  local ws="$1"
  mkdir -p "${ws}/smoke"
  printf 'version: 1\n' >"${ws}/smoke/fixture.txt"
}

prepare_mutation_workspace() {
  local ws="$1"
  mkdir -p "${ws}/src"
  cat >"${ws}/Cargo.toml" <<'EOF'
[package]
name = "lancet-dogfood-fixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[workspace]
EOF
  cat >"${ws}/src/lib.rs" <<'EOF'
pub mod counter;
EOF
  cat >"${ws}/src/counter.rs" <<'EOF'
pub fn count(items: &[u32]) -> usize {
    items.len()
}
EOF
}

prepare_command_workspace() {
  local ws="$1"
  mkdir -p "$ws"
}

judge_smoke() {
  local exit_code="$1"
  local jsonl="$2"
  local ws="$3"
  common_judgment "$exit_code" "$jsonl"
  assert_tool_ok "$jsonl" "observe.workspace"
  assert_tool_ok "$jsonl" "mutate.create"
  assert_tool_ok "$jsonl" "exec.command_run"
  if [[ -f "${ws}/smoke/out.txt" ]] && grep -q '^ok$' "${ws}/smoke/out.txt"; then
    pass "smoke/out.txt contains ok"
  else
    fail "smoke/out.txt missing or wrong"
  fi
}

judge_mutation() {
  local exit_code="$1"
  local jsonl="$2"
  local ws="$3"
  common_judgment "$exit_code" "$jsonl"
  assert_tool_ok "$jsonl" "observe.workspace"
  assert_tool_ok "$jsonl" "mutate.patch"
  assert_jsonl "$jsonl" 'all(.[]; .type != "cep_verify_failed")' "cep_verify_failed absent"
  if grep -q '^///' "${ws}/src/counter.rs"; then
    pass "src/counter.rs has a doc comment"
  else
    fail "src/counter.rs does not have a doc comment"
  fi
}

judge_command() {
  local exit_code="$1"
  local jsonl="$2"
  common_judgment "$exit_code" "$jsonl"
  assert_tool_ok "$jsonl" "exec.command_run"
}

run_scenario() {
  local slug="$1"
  local scenario_dir="${RUN_DIR}/${slug}"
  local ws="${scenario_dir}/workspace"
  local jsonl="${scenario_dir}/events.jsonl"
  local stderr="${scenario_dir}/stderr.log"
  local summary="${scenario_dir}/summary.json"
  local system_prompt=""
  local prompt=""
  local exit_code=0

  mkdir -p "$scenario_dir" "$ws"
  case "$slug" in
    smoke)
      prepare_smoke_workspace "$ws"
      system_prompt="$(smoke_system_prompt)"
      prompt="Execute the LANCET dogfood smoke sequence from your system prompt now."
      ;;
    mutation)
      prepare_mutation_workspace "$ws"
      system_prompt="$(mutation_system_prompt)"
      prompt="Execute the LANCET dogfood mutation sequence from your system prompt now."
      ;;
    command)
      prepare_command_workspace "$ws"
      system_prompt="$(command_system_prompt)"
      prompt="Execute the LANCET dogfood command sequence from your system prompt now."
      ;;
  esac

  write_agent_profile "$ws" "$system_prompt"

  set +e
  run_lattice_turn "$ws" "$prompt" "$jsonl" "$stderr"
  exit_code=$?
  set -e

  build_summary "$jsonl" "$summary" "$exit_code" "$ws"

  SCENARIO_NOTES=()
  SCENARIO_FAIL=0
  "judge_${slug}" "$exit_code" "$jsonl" "$ws"

  local result="PASS"
  local detail="ok"
  if [[ "$SCENARIO_FAIL" -ne 0 ]]; then
    result="FAIL"
    detail="${SCENARIO_NOTES[*]}"
  fi

  append_scenario_report "$slug" "$result" "$ws" "$jsonl" "$stderr" "$summary"
  record_scenario "$slug" "$result" "$detail"
  [[ "$SCENARIO_FAIL" -eq 0 ]]
}

final_report() {
  jq -s '
    {
      scenarios: length,
      failed: ([.[] | select(.exit_code != 0 or .turn_committed != true or .turn_failed == true)] | length),
      tool_calls: (map(.tool_calls | length) | add // 0),
      cep_calls: (map(.lancet.call_finished // 0) | add // 0),
      usage: {
        prompt_tokens: (map(.usage.prompt_tokens // 0) | add // 0),
        completion_tokens: (map(.usage.completion_tokens // 0) | add // 0),
        total_tokens: (map(.usage.total_tokens // 0) | add // 0)
      }
    }
  ' "${RUN_DIR}"/*/summary.json >"$SUMMARY"

  log ""
  log "## Benchmark Summary"
  log ""
  log '```json'
  jq '.' "$SUMMARY" | tee -a "$REPORT" >/dev/null
  log '```'
  log ""
  log "| Scenario | Result | Detail |"
  log "|----------|--------|--------|"
  local i
  for i in "${!MATRIX_SLUG[@]}"; do
    log "| ${MATRIX_SLUG[$i]} | ${MATRIX_RESULT[$i]} | ${MATRIX_DETAIL[$i]} |"
  done
  log ""
  log "- run_dir: ${RUN_DIR}"
  log "- report: ${REPORT}"
}

main() {
  parse_args "$@"
  require_one_provider
  preflight_tools
  ensure_lattice

  mkdir -p "$BASE" "$RUN_DIR" "$(dirname "$REPORT")"
  : >"$REPORT"

  log "# LANCET Dogfood Benchmark"
  log ""
  log "- run_id: ${RUN_ID}"
  log "- model: ${MODEL}"
  log "- binary: ${LATTICE[*]}"
  log "- scenarios: ${SCENARIOS[*]}"
  log "- run_dir: ${RUN_DIR}"
  log ""

  local failures=0
  local scenario
  for scenario in "${SCENARIOS[@]}"; do
    run_scenario "$scenario" || failures=$((failures + 1))
  done

  final_report

  if [[ "${LANCET_DOGFOOD_KEEP:-0}" != "1" ]]; then
    rm -rf "$RUN_DIR"
  fi

  if [[ "$failures" -ne 0 ]]; then
    exit 1
  fi
}

main "$@"
