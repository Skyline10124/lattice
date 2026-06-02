#!/usr/bin/env bash
# Run the legacy-maintenance benchmark against a pinned real upstream project.
# Legacy benchmark: phase prompts still use pre-v1.3.1 CEP tool names and are
# kept for historical comparison, not as the current LANCET catalog contract.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

MODEL="${BENCH_MODEL:-compat:deepseek-v4-pro}"
PROJECT_URL="${BENCH_PROJECT_URL:-}"
PROJECT_REV="${BENCH_PROJECT_REV:-}"
PROJECT_NAME="${BENCH_PROJECT_NAME:-legacy-target}"
ISSUE_TITLE="${BENCH_ISSUE_TITLE:-Maintenance task}"
ISSUE_URL="${BENCH_ISSUE_URL:-n/a}"
ISSUE_TEXT="${BENCH_ISSUE_TEXT:-No frozen issue text provided.}"
ISSUE_FILE="${BENCH_ISSUE_FILE:-}"
VERIFY_COMMAND="${BENCH_VERIFY_COMMAND:-cargo test}"
STRICT_PHASES="${BENCH_STRICT_PHASES:-0}"
BASE="${ROOT}/build/tmp/benchmarks/legacy-maintenance"
REPORT="${BENCH_REPORT:-${BASE}/report.md}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
RUN_DIR="${BASE}/${RUN_ID}"
WORKSPACE="${RUN_DIR}/repo"

declare -a LATTICE
SESSION_ID=""
PHASE_FAIL_COUNT=0
VERIFY_EXIT_CODE=0

require_env() {
  if [[ -z "${!1:-}" ]]; then
    echo "FAIL: $1 is not set" >&2
    exit 2
  fi
}

log() {
  echo "$*" | tee -a "$REPORT"
}

json_string() {
  jq -Rn --arg value "$1" '$value'
}

ensure_lattice() {
  if [[ -n "${BENCH_LATTICE_BIN:-}" ]]; then
    # shellcheck disable=SC2206
    LATTICE=(${BENCH_LATTICE_BIN})
  else
    cargo build -p lattice-cli --quiet
    LATTICE=("${ROOT}/target/debug/lattice")
  fi
}

clone_target() {
  mkdir -p "$RUN_DIR"
  git clone --quiet "$PROJECT_URL" "$WORKSPACE"
  git -C "$WORKSPACE" checkout --quiet "$PROJECT_REV"
}

write_agent_profile() {
  local name="$1"
  local system="$2"
  local tools="$3"
  local dir="${WORKSPACE}/.lattice/agents/${name}"
  mkdir -p "$dir"
  cat >"${dir}/agent.toml" <<EOF
schema_version = 1

[agent]
name = "${name}"
model = "${MODEL}"

[prompt]
system = $(json_string "$system")

[tools]
enabled = [${tools}]

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
EOF
}

seed_profiles() {
  write_agent_profile \
    "bench-reviewer" \
    "You are a benchmark reviewer. Use CEP tools only. Prefer read-only inspection. Keep outputs concise and evidence-based." \
    '"workspace.read_file", "workspace.grep", "workspace.list_directory", "workspace.find_files"'

  write_agent_profile \
    "bench-maintainer" \
    "You are a benchmark maintainer. Use CEP tools only. Declare envelopes before mutation or diagnostic command execution. Keep changes minimal. Use bounded reads. For workspace.patch, use LATTICE structured patch syntax with Old Hash from read_file content_hash; do not use git unified diff syntax or whole-file write_file fallback for small edits. A successful workspace.patch or workspace.write_file under an envelope with verify means that verify passed; do not manually rerun the same command unless no verify was attached." \
    '"envelope.declare", "workspace.read_file", "workspace.grep", "workspace.list_directory", "workspace.find_files", "workspace.write_file", "workspace.patch", "command.run"'

  write_agent_profile \
    "bench-tester" \
    "You are a benchmark tester. Use CEP tools only. Add focused regression coverage and verify it with declared authority before command.run or edits. Use bounded reads. For workspace.patch, use LATTICE structured patch syntax with Old Hash from read_file content_hash; do not use git unified diff syntax. A successful workspace.patch or workspace.write_file under an envelope with verify means that verify passed; do not manually rerun the same command unless no verify was attached." \
    '"envelope.declare", "workspace.read_file", "workspace.grep", "workspace.list_directory", "workspace.find_files", "workspace.write_file", "workspace.patch", "command.run"'
}

render_prompt() {
  local template="$1"
  BENCH_PROJECT_NAME_VALUE="$PROJECT_NAME" \
  BENCH_ISSUE_TITLE_VALUE="$ISSUE_TITLE" \
  BENCH_ISSUE_URL_VALUE="$ISSUE_URL" \
  BENCH_ISSUE_TEXT_VALUE="$ISSUE_TEXT" \
  BENCH_ISSUE_FILE_VALUE="$ISSUE_FILE" \
  BENCH_VERIFY_COMMAND_VALUE="$VERIFY_COMMAND" \
    perl -0pe '
      BEGIN {
        $issue = $ENV{"BENCH_ISSUE_TEXT_VALUE"};
        if ($ENV{"BENCH_ISSUE_FILE_VALUE"} ne "") {
          open my $fh, "<", $ENV{"BENCH_ISSUE_FILE_VALUE"} or die "cannot read issue file: $!";
          local $/;
          $issue = <$fh>;
        }
      }
      s/\{\{PROJECT_NAME\}\}/$ENV{"BENCH_PROJECT_NAME_VALUE"}/ge;
      s/\{\{ISSUE_TITLE\}\}/$ENV{"BENCH_ISSUE_TITLE_VALUE"}/ge;
      s/\{\{ISSUE_URL\}\}/$ENV{"BENCH_ISSUE_URL_VALUE"}/ge;
      s/\{\{VERIFY_COMMAND\}\}/$ENV{"BENCH_VERIFY_COMMAND_VALUE"}/ge;
      s/\{\{ISSUE_TEXT\}\}/$issue/ge;
    ' "$template"
}

start_session() {
  local session_json="${RUN_DIR}/session.json"
  (
    cd "$WORKSPACE"
    "${LATTICE[@]}" -j session new --agent bench-reviewer --label "legacy-maintenance-${RUN_ID}"
  ) >"$session_json"
  SESSION_ID="$(jq -r '.session_id' "$session_json")"
}

phase_agent() {
  case "$1" in
    01-review) echo "bench-reviewer" ;;
    02-reproduce|03-fix) echo "bench-maintainer" ;;
    04-regression) echo "bench-tester" ;;
    05-handoff) echo "bench-reviewer" ;;
    *) echo "bench-reviewer" ;;
  esac
}

run_phase() {
  local slug="$1"
  local template="${ROOT}/benchmarks/legacy-maintenance/phases/${slug}.md"
  local phase_dir="${RUN_DIR}/${slug}"
  local jsonl="${phase_dir}/events.jsonl"
  local stderr="${phase_dir}/stderr.log"
  local summary="${phase_dir}/summary.json"
  local agent
  local prompt
  local exit_code=0

  if [[ -n "${BENCH_PHASE:-}" && "${BENCH_PHASE}" != "$slug" ]]; then
    return 0
  fi

  mkdir -p "$phase_dir"
  agent="$(phase_agent "$slug")"
  prompt="$(render_prompt "$template")"

  (
    cd "$WORKSPACE"
    "${LATTICE[@]}" session agent "$SESSION_ID" "$agent" >/dev/null
  )

  set +e
  (
    cd "$WORKSPACE"
    "${LATTICE[@]}" -j session send "$SESSION_ID" "$prompt" >"$jsonl" 2>"$stderr"
  )
  exit_code=$?
  set -e

  summarize_phase "$jsonl" "$summary" "$exit_code"
  if jq -e '.exit_code == 0 and .turn_committed == true and .turn_failed == false' "$summary" >/dev/null; then
    :
  else
    PHASE_FAIL_COUNT=$((PHASE_FAIL_COUNT + 1))
  fi
  append_phase_report "$slug" "$agent" "$summary" "$stderr"
}

summarize_phase() {
  local jsonl="$1"
  local summary="$2"
  local exit_code="$3"
  jq -s --argjson exit_code "$exit_code" '
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
      commits: ([.[] | select(.type == "turn_committed")] | length),
      llm_request_errors: [
        .[]
        | select(.type == "llm_request_error")
        | {
            error_kind,
            retryable,
            will_retry,
            status_code,
            attempt,
            max_attempts,
            message
          }
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
        replan_required: ([.[] | select(.type == "cep_replan_required")] | length)
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

append_phase_report() {
  local slug="$1"
  local agent="$2"
  local summary="$3"
  local stderr="$4"

  log ""
  log "## ${slug}"
  log ""
  log "- agent: ${agent}"
  log "- summary: ${summary}"
  log ""
  log '```json'
  jq '.' "$summary" | tee -a "$REPORT"
  log '```'
  if [[ -s "$stderr" ]]; then
    log ""
    log "stderr:"
    log '```text'
    sed -n '1,120p' "$stderr" | tee -a "$REPORT" >/dev/null
    log '```'
  fi
}

run_external_verify() {
  local verify_log="${RUN_DIR}/external-verify.log"
  local exit_code=0
  set +e
  (cd "$WORKSPACE" && eval "$VERIFY_COMMAND") >"$verify_log" 2>&1
  exit_code=$?
  set -e
  VERIFY_EXIT_CODE="$exit_code"
  log ""
  log "## External Verify"
  log ""
  log "- command: \`${VERIFY_COMMAND}\`"
  log "- exit_code: ${exit_code}"
  log "- log: ${verify_log}"
}

final_report() {
  local aggregate="${RUN_DIR}/benchmark-summary.json"
  jq -s --argjson verify_exit_code "$VERIFY_EXIT_CODE" --argjson strict_phase_gating "$STRICT_PHASES" '
    {
      phases: length,
      failed_phases: ([.[] | select(.exit_code != 0 or .turn_committed != true or .turn_failed == true)] | length),
      task_passed: ($verify_exit_code == 0),
      verify_exit_code: $verify_exit_code,
      strict_phase_gating: ($strict_phase_gating == 1),
      commits: (map(.commits // 0) | add // 0),
      llm_request_errors: (map(.llm_request_errors | length) | add // 0),
      tool_calls: (map(.tool_calls | length) | add // 0),
      cep_calls: (map(.lancet.call_finished // 0) | add // 0),
      usage: {
        prompt_tokens: (map(.usage.prompt_tokens // 0) | add // 0),
        completion_tokens: (map(.usage.completion_tokens // 0) | add // 0),
        total_tokens: (map(.usage.total_tokens // 0) | add // 0)
      }
    }
  ' "${RUN_DIR}"/*/summary.json >"$aggregate" 2>/dev/null || echo '{}' >"$aggregate"

  log ""
  log "## Benchmark Summary"
  log ""
  log '```json'
  jq '.' "$aggregate" | tee -a "$REPORT"
  log '```'

  log ""
  log "## Final Workspace Diff"
  log ""
  log '```diff'
  git -C "$WORKSPACE" diff --stat | tee -a "$REPORT" || true
  git -C "$WORKSPACE" diff -- . ':(exclude).lattice/**' | sed -n '1,240p' | tee -a "$REPORT" || true
  log '```'
  log ""
  log "Report: ${REPORT}"
}

main() {
  require_env OPENAI_COMPATIBLE_API_KEY
  require_env OPENAI_COMPATIBLE_BASE_URL
  require_env BENCH_PROJECT_URL
  require_env BENCH_PROJECT_REV
  command -v jq >/dev/null 2>&1 || { echo "FAIL: jq not found" >&2; exit 2; }
  command -v git >/dev/null 2>&1 || { echo "FAIL: git not found" >&2; exit 2; }

  ensure_lattice
  mkdir -p "$BASE"
  : >"$REPORT"

  export XDG_CONFIG_HOME="${RUN_DIR}/xdg"
  mkdir -p "$XDG_CONFIG_HOME"

  log "# Legacy Maintenance Benchmark"
  log ""
  log "- run_id: ${RUN_ID}"
  log "- project: ${PROJECT_NAME}"
  log "- revision: ${PROJECT_REV}"
  log "- model: ${MODEL}"
  log "- workspace: ${WORKSPACE}"
  log ""

  clone_target
  seed_profiles
  start_session

  run_phase 01-review
  run_phase 02-reproduce
  run_phase 03-fix
  run_phase 04-regression
  run_phase 05-handoff

  run_external_verify
  final_report

  if [[ "${BENCH_KEEP:-0}" != "1" ]]; then
    rm -rf "$RUN_DIR"
  fi

  if [[ "$VERIFY_EXIT_CODE" -ne 0 ]]; then
    exit 1
  fi

  if [[ "$STRICT_PHASES" == "1" && "$PHASE_FAIL_COUNT" -ne 0 ]]; then
    exit 1
  fi
}

main "$@"
