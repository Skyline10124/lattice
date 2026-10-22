// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local secret discipline: a real protected-file read is refused,
//! and public sinks plus failure artifacts keep per-run sentinels private.

mod support;

use std::fs;

use lattice_system_harness::{
    AssertError, ScenarioEvidence, SessionSendDriver, SlotOutcome, SseScript, failure_dir,
    finalize_with_failure_artifacts,
};
use serde_json::Value;
use support::{
    assert_clean_send, cli, direct_world, driver, finish_recorded, finish_scenario, require,
    sequence, tool_call, tool_result_value, wire_fake_llm,
};

// Preserve all decoded JSONL fields, including records unknown to the typed
// parser, so JSON escapes cannot hide a sentinel from the evidence finalizer.
fn record_jsonl(evidence: &mut ScenarioEvidence, name: &str, bytes: &[u8]) {
    let events: Vec<Value> = serde_json::Deserializer::from_slice(bytes)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .expect("public send JSONL");
    assert!(!events.is_empty(), "send JSONL must contain events");
    assert!(
        events.iter().all(Value::is_object),
        "send event roots must be objects"
    );
    evidence.record_json(name, &Value::Array(events)).unwrap();
}

/// Prove a prepared protected file reaches the observation policy, the typed
/// refusal reaches the recovery request, and no public sink exposes its content
/// or the provider credential. Audit evidence distinguishes policy hiding from
/// a genuinely missing file even though both look like NotFound to the guest.
#[test]
fn direct_secrets_sentinels_never_leak_into_public_sinks() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-secrets",
        "dl-secret",
        "[\"observe.workspace\"]",
        "\n[lancet.workspace]\ndenied_read_patterns = [\"private.txt\"]\n",
    );
    let unique = world.root().file_name().unwrap().to_str().unwrap();
    let denied_content = format!("sentinel-denied-content-{unique}");
    // No key prefix, authentication syntax, URL, or 40-character opaque run:
    // only known-credential redaction can hide this value in the declared name.
    let credential = format!("fixture.{unique}");
    assert!(credential.len() < 40);
    world
        .write_project_file("private.txt", &format!("{denied_content}\n"))
        .expect("write private");

    let server = wire_fake_llm(
        &world,
        "dl-secret",
        sequence(vec![
            tool_call(
                "secret-read",
                "observe.workspace",
                r#"{"request":{"mode":"read_file","path":"private.txt"}}"#,
            ),
            SseScript::plain("hello"),
        ]),
    );
    world.set_env("OPENAI_COMPATIBLE_API_KEY", &credential);
    server.expect_api_key(&credential);
    let mut evidence = ScenarioEvidence::new(
        &world,
        &spec.name,
        [credential.clone(), denied_content.clone()],
    )
    .unwrap();
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-secret");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let turn = send.send("read private.txt").expect("turn");
        evidence.record_command("secrets send", &turn.command);
        record_jsonl(&mut evidence, "send-events", &turn.command.stdout);
        assert_clean_send(&turn).unwrap();
        assert!(turn.command.is_clean_exit(0));
        assert!(turn.has_completed(), "recovery turn must complete");
        let sid = send.session_id().expect("accepted Session");
        let tid = turn.accepted_turn_id().expect("accepted Turn");
        let slots = turn.parsed.slots();
        assert_eq!(
            slots.len(),
            1,
            "protected read must be attempted exactly once"
        );
        assert_eq!(slots[0].method, "observe.workspace");
        assert_eq!(slots[0].outcome, SlotOutcome::Failed);
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 2, "refusal recovery must reach provider");
        let refusal = tool_result_value(&captures[1]).expect("recovery tool result envelope");
        assert_eq!(refusal["error"]["code"], -32003);
        assert_eq!(refusal["error"]["message"], "not_found: 'private.txt'");

        let mut query = |name: &str, args: &[&str]| -> Value {
            let output = cli(&world, &spec, args);
            evidence.record_command(args.join(" "), &output);
            assert!(
                output.is_clean_exit(0),
                "{}: {}",
                args.join(" "),
                output.summary()
            );
            let value: Value = serde_json::from_slice(&output.stdout).expect("public query JSON");
            if name == "session-list" {
                assert!(value.is_array(), "session list root array");
            } else {
                assert!(value.is_object(), "public query root object");
            }
            evidence.record_json(name, &value).unwrap();
            value
        };
        let sessions = query("session-list", &["--json", "session", "list"]);
        let sessions = sessions.as_array().expect("session list root array");
        assert_eq!(sessions.len(), 1);
        let log = query("session-log", &["--json", "session", "log", sid]);
        assert!(log.is_object(), "session log root object");
        assert!(!log["nodes"].as_array().expect("session nodes").is_empty());
        let audit = query(
            "audit-query",
            &["--json", "audit", "query", "--session", sid],
        );
        assert!(audit.is_object(), "audit query root object");
        let rows = audit["rows"].as_array().expect("audit rows");
        assert_eq!(rows.len(), 1, "audit must contain the protected read");
        let row = &rows[0];
        assert_eq!(row["binding"]["method"], "observe.workspace");
        assert_eq!(row["binding"]["session_id"], sid);
        assert_eq!(row["binding"]["turn_id"], tid);
        assert_eq!(row["state"]["state"], "terminal");
        assert_eq!(row["state"]["outcome"], "failed");
        assert_eq!(row["state"]["evidence"]["kind"], "recorded");
        let denials = row["state"]["evidence"]["value"]["denied"]["denied"]
            .as_array()
            .expect("recorded host denials");
        assert_eq!(denials.len(), 1);
        assert_eq!(
            denials[0]["reason"], "read_denied",
            "protected file must reach the read policy"
        );
        let replay = query(
            "audit-replay",
            &["--json", "audit", "replay", "--session", sid, "--turn", tid],
        );
        assert!(replay.is_object(), "audit replay root object");
        assert_eq!(replay["calls"].as_array().expect("replay calls"), rows);
        let status = query("lancet-status", &["--json", "lancet", "status"]);
        assert!(status.is_object(), "lancet status root object");
        assert_eq!(status["recent"].as_array().expect("recent calls").len(), 1);
        let detail = query("session-turn", &["--json", "session", "turn", tid]);
        assert!(detail.is_object(), "session turn root object");
        assert!(!detail["steps"].as_array().expect("turn steps").is_empty());
        let text = cli(&world, &spec, &["session", "turn", tid]);
        evidence.record_command("session turn text", &text);
        assert!(text.is_clean_exit(0));

        // 配置拒绝原因必须经过脱敏，且不得产生新的 provider 请求。
        let profile = world
            .project_dir()
            .join(".lattice/agents/dl-secret/agent.toml");
        let declared = fs::read_to_string(&profile)
            .expect("read profile")
            .replace("name = \"dl-secret\"", &format!("name = '{credential}'"));
        fs::write(&profile, declared).expect("write profile");
        let rejected = ["--json", "session", "send", "again", "--agent", "dl-secret"];
        let rj = cli(&world, &spec, &rejected);
        evidence.record_command("rejected send JSON", &rj);
        assert!(rj.is_clean_exit(2), "configuration rejection exits 2");
        record_jsonl(&mut evidence, "rejected-send-events", &rj.stdout);
        let rt = cli(&world, &spec, &rejected[1..]);
        evidence.record_command("rejected send text", &rt);
        for (output, reason) in [(&rj, &rj.stdout), (&rt, &rt.stderr)] {
            assert!(output.is_clean_exit(2), "configuration rejection exits 2");
            assert!(
                String::from_utf8_lossy(reason).contains("declares '[REDACTED]'"),
                "rejection reason must show the declared name redacted"
            );
        }
        let rejection = lattice_system_harness::parse_send_stdout(&rj.stdout, rj.exit_code());
        assert!(
            rejection.malformed_lines.is_empty(),
            "rejection JSONL must parse"
        );
        assert_eq!(support::rejected_kind(&rejection), Some("bad_request"));
        assert_eq!(
            server.captured_requests().len(),
            captures.len(),
            "rejected configuration must not reach the provider"
        );
        // Raw commands and captures are scanned unconditionally by finish_report,
        // including when one of the business assertions above panics.
    }));
    evidence.record_captures(server.captured_requests());
    finish_recorded(evidence, result);
}

/// Prove redaction actually transforms a secret-bearing model reply, retains
/// the original business failure, and reports the raw-output leak separately.
#[test]
fn direct_secrets_failure_bundle_redacts_sentinels() {
    let driver = driver();
    let (world, spec) = direct_world("direct-secrets-bundle", "dl-sec-b", "[]", "");
    let unique = world.root().file_name().unwrap().to_str().unwrap();
    let credential = format!("bundle-auth.{unique}");
    let fake_key = format!("sk-reply-fixture-{unique}");
    let protected = format!("protected-content-{unique}");
    let injected = format!("injected-secret-{unique}");
    let reply = format!("credential={fake_key} protected={protected} injected={injected}");
    let reply_secrets = [
        ("credential", fake_key.as_str()),
        ("protected", protected.as_str()),
        ("injected", injected.as_str()),
    ];
    let server = wire_fake_llm(&world, "dl-sec-b", SseScript::plain(&reply));
    world.set_env("OPENAI_COMPATIBLE_API_KEY", &credential);
    server.expect_api_key(&credential);
    let scenario_evidence =
        ScenarioEvidence::new(&world, &spec.name, [credential.clone()]).unwrap();
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-sec-b");
    let turn = send.send("reply with the fixture text").expect("turn");
    let captures = server.captured_requests();
    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            turn.command.is_clean_exit(0) && turn.has_completed(),
            "model reply completes",
        )?;
        require(captures.len() == 1, "model reply must reach provider")?;
        let raw_stdout = String::from_utf8_lossy(&turn.command.stdout);
        let mut expected_redactions = 0;
        for (label, secret) in reply_secrets {
            require(
                raw_stdout.contains(&format!("{label}={secret}")),
                format!("raw stdout must contain injected {label} value"),
            )?;
            expected_redactions += raw_stdout.matches(secret).count();
        }
        require(
            !raw_stdout.contains(&credential),
            "authentication credential must stay out of reply",
        )?;
        require(
            !raw_stdout.contains("[REDACTED]"),
            "raw reply must contain unredacted inputs",
        )?;
        // The intentional leak belongs to this finalizer, which returns its
        // report for inspection rather than failing the surrounding scenario.
        let bundle_name = format!("{}-fast", spec.name);
        let bundle_dir = failure_dir(&bundle_name).expect("valid scenario name");
        let primary = "intentional failure to trigger artifacts";
        let report = finalize_with_failure_artifacts(
            &world,
            &bundle_name,
            "lattice --json session send",
            &turn.command,
            &[&credential, &fake_key, &protected, &injected],
            Err(AssertError::StdoutMissing {
                expected: primary.to_string(),
            }),
        )
        .expect_err("intentional business assertion and raw leak must be reported");
        let stdout = fs::read_to_string(bundle_dir.join("stdout.txt")).expect("read bundle stdout");
        for (label, secret) in reply_secrets {
            require(
                !stdout.contains(secret),
                format!("bundle stdout must redact {label} value"),
            )?;
            require(
                stdout.matches(&format!("{label}=[REDACTED]")).count()
                    == raw_stdout.matches(secret).count(),
                format!("bundle stdout must replace every {label} occurrence"),
            )?;
        }
        require(
            stdout.matches("[REDACTED]").count() == expected_redactions,
            "bundle stdout must replace every occurrence of all three inputs",
        )?;
        let text = report.to_string();
        require(
            text.contains(primary),
            "report must retain original assertion text",
        )?;
        require(
            text.contains("- command stdout"),
            "report must locate original stdout leak",
        )?;
        for (label, secret) in reply_secrets {
            require(
                !text.contains(secret),
                format!("report must redact {label} value"),
            )?;
        }
        let leaks = lattice_system_harness::scan_artifacts_for_secrets(
            &bundle_dir,
            &[&credential, &fake_key, &protected, &injected],
        )
        .expect("scan artifacts");
        require(leaks.is_empty(), format!("bundle leaks: {leaks:?}"))?;
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[("secrets bundle send", &turn.command)],
        captures,
        result,
    );
}
