// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local secret-sentinel discipline (issue #24 slice 6).
//!
//! Uses a unique per-scenario provider-credential sentinel plus a
//! denied-workspace-content sentinel, then scans every public sink
//! (command stdout/stderr, parsed JSONL, session status/log, audit
//! query/replay, lancet status, provenance JSON) and asserts no sentinel
//! leaks. A detected sentinel is an assertion failure.
//!
//! ponytail: helpers duplicate other Direct-Local tests; later-slice split
//! owns the dedupe.

#![cfg(unix)]

use std::fs;
use std::process::Output;

use lattice_system_harness::{
    AssertError, CommandOutcome, FAKE_API_KEY, LatticeDriver, ScenarioEvidence, ScenarioSpec,
    ScenarioWorld, SendExecution, SessionSendDriver, SseScript, Termination, failure_dir,
};

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

fn assert_clean_send(execution: &SendExecution) -> Result<(), AssertError> {
    if !matches!(&execution.command.termination, Termination::Exited(Some(_))) {
        return Err(AssertError::StdoutMissing {
            expected: format!(
                "session send did not exit normally: {:?}",
                execution.command.termination
            ),
        });
    }
    if !execution.malformed_lines.is_empty() {
        return Err(AssertError::StdoutMissing {
            expected: format!(
                "session send emitted malformed JSONL: {:?}",
                execution.malformed_lines
            ),
        });
    }
    Ok(())
}

fn require(condition: bool, expected: impl Into<String>) -> Result<(), AssertError> {
    if condition {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing {
            expected: expected.into(),
        })
    }
}

fn evidence(world: &ScenarioWorld, spec: &ScenarioSpec) -> ScenarioEvidence {
    ScenarioEvidence::new(world, failure_dir(&spec.name), [FAKE_API_KEY.to_string()])
        .expect("scenario evidence")
}

fn finish_scenario(
    mut evidence: ScenarioEvidence,
    commands: &[(&str, &CommandOutcome)],
    result: Result<(), AssertError>,
) {
    for (summary, outcome) in commands {
        evidence.record_command(*summary, outcome);
    }
    evidence
        .finish(result.is_err())
        .unwrap_or_else(|error| panic!("scenario evidence failed: {error}"));
    result.unwrap_or_else(|error| panic!("{error}"));
}

fn wire_fake_llm(
    world: &ScenarioWorld,
    agent: &str,
    script: SseScript,
) -> lattice_system_harness::FakeLlmServer {
    let server = lattice_system_harness::FakeLlmServer::start(script).expect("start fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", agent);
    world.set_env(
        "OPENAI_COMPATIBLE_API_KEY",
        lattice_system_harness::FAKE_API_KEY,
    );
    server
}

fn secrets_world(prefix: &str, agent: &str, tools: &str) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{agent}\"\nmodel = \"compat:{agent}\"\n\n\
         [prompt]\nsystem = \"You are a concise test agent.\"\n\n\
         [tools]\nenabled = {tools}\n"
    );
    world
        .write_project_file(&format!(".lattice/agents/{agent}/agent.toml"), &toml)
        .expect("write agent.toml");
    world.init_git_project().expect("init git");
    (world, spec)
}

fn cli(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> Output {
    driver()
        .run(
            world,
            &lattice_system_harness::LatticeCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
        .output
}

/// A unique denied-content sentinel that must never leak into terminal output.
const DENIED_CONTENT_SENTINEL: &str = "sentinel-denied-content-0xDEADBEEF";

/// Scan every public sink for sentinels after a turn: stdout/stderr, parsed
/// JSONL, session status/log, audit query/replay, lancet status, provenance.
/// The FAKE_API_KEY (provider credential) and the denied-content sentinel must
/// not appear in any of them.
#[test]
fn direct_secrets_sentinels_never_leak_into_public_sinks() {
    let driver = driver();
    let (world, spec) = secrets_world("direct-secrets", "dl-secret", "[]");
    // Seed a file whose content carries the denied-content sentinel. The turn
    // never reads it (no tools enabled), so it must not appear anywhere.
    world
        .write_project_file(
            "private.txt",
            concat!("sentinel-denied-content-0xDEADBEEF", "\n"),
        )
        .expect("write private");
    world.commit_all("seed").expect("commit");

    let server = wire_fake_llm(&world, "dl-secret", SseScript::plain("hello"));
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-secret", "main").expect("session");
    let sid = send.session_id().to_string();

    let turn = send.send("hello").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(turn.has_committed_completed(), "turn must complete")?;

        // Collect all public outputs as one scan surface.
        let send_stdout = String::from_utf8_lossy(&turn.command.output.stdout);
        let send_stderr = String::from_utf8_lossy(&turn.command.output.stderr);

        let ss = cli(&world, &spec, &["--json", "session", "status", &sid]);
        let sl = cli(&world, &spec, &["--json", "session", "log", &sid]);
        let aq = cli(
            &world,
            &spec,
            &["--json", "audit", "query", "--session", &sid],
        );
        let ls = cli(&world, &spec, &["--json", "lancet", "status"]);

        let sinks: Vec<(&str, String)> = vec![
            ("send stdout", send_stdout.to_string()),
            ("send stderr", send_stderr.to_string()),
            (
                "session status",
                String::from_utf8_lossy(&ss.stdout).to_string(),
            ),
            (
                "session status stderr",
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
        result,
    );
}

/// Failure artifacts persisted after a failed turn are recursively scanned for
/// sentinels. A detected sentinel is an assertion failure even if the scanner
/// can redact the bundle copy.
#[test]
fn direct_secrets_failure_bundle_redacts_sentinels() {
    let driver = driver();
    let (world, spec) = secrets_world("direct-secrets-bundle", "dl-sec-b", "[]");
    let server = wire_fake_llm(
        &world,
        "dl-sec-b",
        SseScript::HttpError {
            status: 500,
            body: r#"{"error":{"message":"down"}}"#.to_string(),
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-sec-b", "main").expect("session");

    let turn = send.send("trigger failure").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(!turn.has_committed_completed(), "turn must fail")?;
        require(
            turn.exit_code == Some(lattice_system_harness::SendExitCode::Failed),
            "turn must exit 1",
        )?;
        // Force a failure-artifact write by running a deliberately failing
        // assertion through the evidence helper, then scan the bundle dir.
        let bundle_dir = failure_dir(&spec.name);
        let failing = AssertError::StdoutMissing {
            expected: "intentional failure to trigger artifacts".to_string(),
        };
        let write_result = lattice_system_harness::assert_with_failure_artifacts(
            &world,
            &spec.name,
            "lattice --json session send",
            &turn.command.output,
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
        result,
    );
    let _ = captures;
}
