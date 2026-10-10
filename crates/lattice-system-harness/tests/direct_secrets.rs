// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local secret-sentinel discipline (issue #46 (Codeberg #24) slice 6).
//!
//! Uses a unique per-scenario provider-credential sentinel plus a
//! denied-workspace-content sentinel, then scans every public sink
//! (command stdout/stderr, parsed JSONL, session list/log, audit query,
//! and lancet status JSON) and asserts no sentinel
//! leaks. A detected sentinel is an assertion failure.

mod support;

use std::fs;

use lattice_system_harness::{
    AssertError, FAKE_API_KEY, SessionSendDriver, SseScript, failure_dir,
};
use support::{
    assert_clean_send, cli, direct_world, driver, evidence, finish_scenario, require, sequence,
    tool_call, tool_ok, wire_fake_llm,
};

/// A unique denied-content sentinel that must never leak into terminal output.
const DENIED_CONTENT_SENTINEL: &str = "sentinel-denied-content-0xDEADBEEF";

/// Scan every public sink for sentinels after a turn: stdout/stderr, parsed
/// JSONL, session list/log, audit query, lancet status, and the reason shown
/// for a rejected configuration.
/// The FAKE_API_KEY (provider credential) and the denied-content sentinel must
/// not appear in any of them.
#[test]
fn direct_secrets_sentinels_never_leak_into_public_sinks() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-secrets",
        "dl-secret",
        "[\"session.todo_write\"]",
        "",
    );
    // Seed denied content that must remain absent even though this scenario
    // executes a real embedded Component Tool.
    world
        .write_project_file(
            "private.txt",
            concat!("sentinel-denied-content-0xDEADBEEF", "\n"),
        )
        .expect("write private");
    world.commit_all("seed").expect("commit");

    let server = wire_fake_llm(
        &world,
        "dl-secret",
        sequence(vec![
            tool_call(
                "secret-safe-tool",
                "session.todo_write",
                r#"{"todos":[{"content":"safe public task","status":"pending"}]}"#,
            ),
            SseScript::plain("hello"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-secret");

    let turn = send.send("hello").expect("turn");
    let sid = send.session_id().expect("accepted Session");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(turn.has_completed(), "turn must complete")?;
        require(
            tool_ok(&turn.parsed, "session.todo_write"),
            "Tool-bearing secret scenario must execute session.todo_write",
        )?;
        // Collect all public outputs as one scan surface.
        let send_stdout = String::from_utf8_lossy(&turn.command.stdout);
        let send_stderr = String::from_utf8_lossy(&turn.command.stderr);

        let ss = cli(&world, &spec, &["--json", "session", "list"]);
        let sl = cli(&world, &spec, &["--json", "session", "log", sid]);
        let aq = cli(
            &world,
            &spec,
            &["--json", "audit", "query", "--session", sid],
        );
        let ls = cli(&world, &spec, &["--json", "lancet", "status"]);
        // `session turn` prints the Turn's frozen configuration digest, the
        // public output closest to the credential.
        let tid = turn.accepted_turn_id().expect("accepted Turn");
        let st = cli(&world, &spec, &["--json", "session", "turn", tid]);
        let st_text = cli(&world, &spec, &["session", "turn", tid]);
        for output in [&ss, &sl, &aq, &ls, &st, &st_text] {
            require(output.is_clean_exit(0), "public sink query succeeds")?;
        }
        require(
            String::from_utf8_lossy(&aq.stdout).contains("session.todo_write"),
            "operator audit query must include the Tool-bearing invocation",
        )?;
        // 配置被拒绝的说明也是公开输出：把本次请求用过的凭据写进会被引用的配置值，
        // 先确认它到达脱敏处并被替换，再和其他输出一起扫描。
        let profile = world
            .project_dir()
            .join(".lattice/agents/dl-secret/agent.toml");
        let declared = fs::read_to_string(&profile)
            .expect("read profile")
            .replace("name = \"dl-secret\"", &format!("name = '{FAKE_API_KEY}'"));
        fs::write(&profile, declared).expect("write profile");
        let rejected = ["--json", "session", "send", "again", "--agent", "dl-secret"];
        let rj = cli(&world, &spec, &rejected);
        let rt = cli(&world, &spec, &rejected[1..]);
        for (output, reason) in [(&rj, &rj.stdout), (&rt, &rt.stderr)] {
            require(output.is_clean_exit(2), "configuration rejection exits 2")?;
            require(
                String::from_utf8_lossy(reason).contains("declares '[REDACTED]'"),
                "rejection reason must show the declared name redacted",
            )?;
        }
        require(
            server.captured_requests().len() == captures.len(),
            "rejected configuration must not reach the provider",
        )?;
        let sinks: Vec<(&str, String)> = vec![
            ("send stdout", send_stdout.to_string()),
            ("send stderr", send_stderr.to_string()),
            (
                "session list",
                String::from_utf8_lossy(&ss.stdout).to_string(),
            ),
            (
                "session list stderr",
                String::from_utf8_lossy(&ss.stderr).to_string(),
            ),
            (
                "session log",
                String::from_utf8_lossy(&sl.stdout).to_string(),
            ),
            (
                "session log stderr",
                String::from_utf8_lossy(&sl.stderr).to_string(),
            ),
            (
                "audit query",
                String::from_utf8_lossy(&aq.stdout).to_string(),
            ),
            (
                "audit query stderr",
                String::from_utf8_lossy(&aq.stderr).to_string(),
            ),
            (
                "lancet status",
                String::from_utf8_lossy(&ls.stdout).to_string(),
            ),
            (
                "lancet status stderr",
                String::from_utf8_lossy(&ls.stderr).to_string(),
            ),
            (
                "session turn --json",
                String::from_utf8_lossy(&st.stdout).to_string(),
            ),
            (
                "session turn --json stderr",
                String::from_utf8_lossy(&st.stderr).to_string(),
            ),
            (
                "session turn",
                String::from_utf8_lossy(&st_text.stdout).to_string(),
            ),
            (
                "session turn stderr",
                String::from_utf8_lossy(&st_text.stderr).to_string(),
            ),
            (
                "rejected send --json",
                String::from_utf8_lossy(&rj.stdout).to_string(),
            ),
            (
                "rejected send --json stderr",
                String::from_utf8_lossy(&rj.stderr).to_string(),
            ),
            (
                "rejected send",
                String::from_utf8_lossy(&rt.stdout).to_string(),
            ),
            (
                "rejected send stderr",
                String::from_utf8_lossy(&rt.stderr).to_string(),
            ),
        ];

        for (name, text) in &sinks {
            require(
                !text.contains(FAKE_API_KEY),
                format!("{name} must not contain provider credential sentinel"),
            )?;
            require(
                !text.contains(DENIED_CONTENT_SENTINEL),
                format!("{name} must not contain denied-content sentinel"),
            )?;
        }

        // Captured fake-provider request JSON: the authorization header value
        // must not be exposed in the captured body (the fake server captures
        // only the request body, not headers). The captured raw JSON is a
        // public sink too — scan it.
        for cap in &captures {
            let raw = serde_json::to_string(&cap.raw).unwrap_or_default();
            require(
                !raw.contains(FAKE_API_KEY),
                "captured fake request must not contain provider credential",
            )?;
            require(
                !raw.contains(DENIED_CONTENT_SENTINEL),
                "captured fake request must not contain denied content",
            )?;
        }

        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("secrets sentinels", &turn.command)],
        Vec::new(),
        result,
    );
}

/// Failure artifacts persisted after a failed turn are recursively scanned for
/// sentinels. A detected sentinel is an assertion failure even if the scanner
/// can redact the bundle copy.
#[test]
fn direct_secrets_failure_bundle_redacts_sentinels() {
    let driver = driver();
    let (world, spec) = direct_world("direct-secrets-bundle", "dl-sec-b", "[]", "");
    let server = wire_fake_llm(
        &world,
        "dl-sec-b",
        SseScript::HttpError {
            status: 500,
            body: r#"{"error":{"message":"down"}}"#.to_string(),
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-sec-b");

    let turn = send.send("trigger failure").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(!turn.has_completed(), "turn must fail")?;
        require(
            turn.exit_code == Some(lattice_system_harness::SendExitCode::Failed),
            "turn must exit 1",
        )?;
        // Use a distinct target because the Direct recorder already holds the
        // scenario claim while this compatibility fast-path bundle is written.
        let bundle_name = format!("{}-fast", spec.name);
        let bundle_dir = failure_dir(&bundle_name).expect("valid scenario name");
        let failing = AssertError::StdoutMissing {
            expected: "intentional failure to trigger artifacts".to_string(),
        };
        let write_result = lattice_system_harness::assert_with_failure_artifacts(
            &world,
            &bundle_name,
            "lattice --json session send",
            &turn.command,
            &[FAKE_API_KEY],
            Err(failing),
        );
        // assert_with_failure_artifacts returns Err because the assertion failed;
        // it also wrote the (redacted) bundle. Verify the bundle is redacted.
        require(write_result.is_err(), "failing assertion must be reported")?;
        let stdout_path = bundle_dir.join("stdout.txt");
        require(stdout_path.is_file(), "bundle stdout.txt must exist")?;
        let stdout = fs::read_to_string(&stdout_path).expect("read bundle stdout");
        require(
            !stdout.contains(FAKE_API_KEY),
            "bundle stdout must not contain provider credential",
        )?;
        // The recursive scanner should find no sentinels in the bundle dir.
        let leaks =
            lattice_system_harness::scan_artifacts_for_secrets(&bundle_dir, &[FAKE_API_KEY])
                .expect("scan artifacts");
        require(leaks.is_empty(), format!("bundle leaks: {leaks:?}"))?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("secrets bundle", &turn.command)],
        Vec::new(),
        result,
    );
    let _ = captures;
}
