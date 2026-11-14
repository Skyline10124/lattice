// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn configuration_errors_keep_reason_and_exit_two_through_anyhow() {
    let local = LocalError::new(
        LocalErrorKind::BadRequest,
        "agent profile 'reader' not found at project path /project/.lattice/agents/reader/agent.toml or global path /home/user/.lattice/agents/reader/agent.toml",
    );
    for error in [
        anyhow::Error::new(local.clone()).context("configuration rejection"),
        anyhow::Error::new(local.clone()).context("TUI configuration"),
        submission_error(local.clone()),
    ] {
        let (code, record) = error_record(&error);
        assert_eq!(code, 2);
        assert_eq!(record["type"], "error");
        assert_eq!(record["kind"], "bad_request");
        assert_eq!(record["message"], local.message);
        assert!(
            record["message"]
                .as_str()
                .unwrap()
                .contains("agent profile")
        );
    }
    let error = anyhow::Error::new(LocalError::new(
        LocalErrorKind::BadRequest,
        "model reference must be <provider>/<model>",
    ));
    let (code, record) = error_record(&error);
    assert_eq!(code, 2);
    assert_eq!(record["kind"], "bad_request");
    assert_eq!(
        record["message"],
        "model reference must be <provider>/<model>"
    );
    let error = anyhow::Error::new(LocalError::new(
        LocalErrorKind::BadRequest,
        "request or frozen configuration is invalid",
    ));
    let (code, record) = error_record(&error);
    assert_eq!(code, 2);
    assert_eq!(
        record["message"],
        "request or frozen configuration is invalid"
    );
}

#[test]
fn frontend_local_errors_keep_json_human_output_and_exit_codes() {
    let invalid_key = super::super::auth::finish_key(zeroize::Zeroizing::new(vec![0xff, b'\n']))
        .expect_err("非法 UTF-8 key 应失败");
    let invalid_callback = super::super::auth::login::input_failure(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "callback URL must be UTF-8 and at most 16384 bytes",
    ));
    for (error, expected_message) in [
        (invalid_key, "API key input must be valid UTF-8"),
        (
            invalid_callback,
            "callback URL must be UTF-8 and at most 16384 bytes",
        ),
    ] {
        let (code, record) = error_record(&error);
        assert_eq!(code, 2, "前端输入错误应退出 2");
        assert_eq!(
            record,
            serde_json::json!({
                "type": "error",
                "kind": "bad_request",
                "message": expected_message,
                "recovery": null,
                "store": null,
            })
        );
        let mut human = Vec::new();
        render_human_error(&record, &mut human).unwrap();
        assert_eq!(
            String::from_utf8(human).unwrap(),
            format!("error: {expected_message}\n")
        );
    }

    let other_io = super::super::auth::login::input_failure(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "pipe closed",
    ));
    let (code, record) = error_record(&other_io);
    assert_eq!(code, 1, "其他 I/O 错误应保持退出 1");
    assert_eq!(
        record,
        serde_json::json!({
            "type": "error",
            "kind": "internal",
            "message": "command failed; inspect configuration and durable state",
        })
    );

    let empty_root = std::path::PathBuf::from("/tmp").join(format!(
        "ltc-cli-nr-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let client_error = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            lattice_local::LocalClient::new(lattice_local::StateLayout::in_state_root(&empty_root))
                .status()
                .await
                .unwrap_err()
        });
    let not_running = anyhow::Error::new(client_error);
    let (nr_code, nr_record) = error_record(&not_running);
    assert_eq!(nr_code, 1, "not_running 应退出 1");
    assert_eq!(nr_record["type"], "error");
    assert_eq!(nr_record["kind"], "not_running");
    let nr_msg = nr_record["message"].as_str().unwrap();
    assert!(
        nr_msg.contains("后台未运行")
            && nr_msg.contains("systemctl --user start lattice")
            && nr_msg.contains("lattice daemon install")
            && nr_msg.contains("状态根"),
        "not_running 中文提示不完整：{nr_msg}"
    );
}
