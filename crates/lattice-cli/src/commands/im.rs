// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice im` 命令组分派、凭据无回显录入、重载回执与状态输出渲染。

use std::{
    io::{self, Write},
    process::ExitCode,
};

use clap::Subcommand;
use lattice_local::{
    ImAuthRemoveReply, ImAuthSetReply, ImAuthStatus, ImCredentialStatus, ImMountResult,
    LocalClient, LocalError, LocalErrorKind, StateLayout,
};
use zeroize::Zeroizing;

use super::secret_input::{SecretReadError, read_secret_from_stdin};

const TELEGRAM_PLATFORM: &str = "telegram";
const IM_SECRET_PROMPT: &str = "请输入平台凭据：";
const IM_INVALID_UTF8_MESSAGE: &str = "平台凭据输入不是有效的 UTF-8 文本";
const UNSUPPORTED_PLATFORM_MESSAGE: &str = "不支持的 IM 平台；当前仅支持 telegram";

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ImAction {
    /// 管理 IM 平台凭据
    Auth {
        #[command(subcommand)]
        action: ImAuthAction,
    },
    /// 重载 IM 配置与凭据
    Reload,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ImAuthAction {
    /// 从终端或标准输入读取并保存平台凭据
    Set {
        /// IM 平台名称
        #[arg(value_name = "平台")]
        platform: String,
    },
    /// 删除平台凭据
    Remove {
        /// IM 平台名称
        #[arg(value_name = "平台")]
        platform: String,
    },
    /// 查看平台凭据状态
    Status,
}

/// 统一渲染 `ImMountResult` 的中文状态文本。
pub(crate) fn format_mount_result(mount: &ImMountResult) -> String {
    match mount {
        ImMountResult::Mounted => "已挂载".into(),
        ImMountResult::UsingPreviousConfiguration { reason } => {
            format!("已挂载；仍按上一份配置运行；{reason}")
        }
        ImMountResult::Unmounted { reason } => format!("未挂载；{reason}"),
        ImMountResult::Disabled => "未启用".into(),
    }
}

fn validate_platform(platform: &str) -> Result<(), LocalError> {
    if platform == TELEGRAM_PLATFORM {
        Ok(())
    } else {
        // 严禁回显传入的平台参数，防止用户误把密钥写在位置参数上导致泄露。
        Err(LocalError::new(
            LocalErrorKind::BadRequest,
            UNSUPPORTED_PLATFORM_MESSAGE,
        ))
    }
}

fn resolve_client() -> Result<LocalClient, LocalError> {
    let layout = StateLayout::resolve().map_err(|_| {
        LocalError::new(
            LocalErrorKind::Internal,
            "无法解析状态根目录：请设置绝对路径的 XDG_STATE_HOME 或 HOME",
        )
    })?;
    Ok(LocalClient::new(layout))
}

fn report_im_error(error: LocalError, json: bool) -> anyhow::Result<ExitCode> {
    if json {
        return Err(error.into());
    }
    eprintln!("错误：{}", error.message);
    if let Some(store) = &error.store
        && let Ok(raw) = serde_json::to_string(store)
    {
        eprintln!("{raw}");
    }
    let code = match error.kind {
        LocalErrorKind::BadRequest | LocalErrorKind::NotFound | LocalErrorKind::Conflict => 2,
        _ => 1,
    };
    Ok(ExitCode::from(code))
}

fn handle_write_result(result: io::Result<ExitCode>, json: bool) -> anyhow::Result<ExitCode> {
    match result {
        Ok(code) => Ok(code),
        Err(error) => report_im_error(
            LocalError::new(
                LocalErrorKind::Internal,
                &format!("写出 IM 命令结果失败：{error}"),
            ),
            json,
        ),
    }
}

pub(crate) async fn run(action: ImAction, json: bool) -> anyhow::Result<ExitCode> {
    match action {
        ImAction::Auth { action } => match action {
            ImAuthAction::Set { platform } => {
                // 先在本地校验平台名称，非法时立即拒绝且不阻塞等待标准输入。
                if let Err(error) = validate_platform(&platform) {
                    return report_im_error(error, json);
                }
                let prompt = (!json).then_some(IM_SECRET_PROMPT);
                let token = match read_secret_from_stdin(prompt, IM_INVALID_UTF8_MESSAGE) {
                    Ok(secret) => Zeroizing::new(secret),
                    Err(SecretReadError::InvalidUtf8(error)) => {
                        return report_im_error(error, json);
                    }
                    Err(SecretReadError::Io(error)) => {
                        return report_im_error(
                            LocalError::new(
                                LocalErrorKind::Internal,
                                &format!("读取平台凭据失败：{error}"),
                            ),
                            json,
                        );
                    }
                };
                let client = match resolve_client() {
                    Ok(client) => client,
                    Err(error) => return report_im_error(error, json),
                };
                let reply = match client.im_auth_set(&platform, &token).await {
                    Ok(reply) => reply,
                    Err(error) => return report_im_error(error, json),
                };
                handle_write_result(
                    render_auth_set_reply(&platform, &reply, json, &mut io::stdout()),
                    json,
                )
            }
            ImAuthAction::Remove { platform } => {
                if let Err(error) = validate_platform(&platform) {
                    return report_im_error(error, json);
                }
                let client = match resolve_client() {
                    Ok(client) => client,
                    Err(error) => return report_im_error(error, json),
                };
                let reply = match client.im_auth_remove(&platform).await {
                    Ok(reply) => reply,
                    Err(error) => return report_im_error(error, json),
                };
                handle_write_result(
                    render_auth_remove_reply(&platform, &reply, json, &mut io::stdout()),
                    json,
                )
            }
            ImAuthAction::Status => {
                let client = match resolve_client() {
                    Ok(client) => client,
                    Err(error) => return report_im_error(error, json),
                };
                let status = match client.im_auth_status().await {
                    Ok(status) => status,
                    Err(error) => return report_im_error(error, json),
                };
                handle_write_result(render_auth_status(&status, json, &mut io::stdout()), json)
            }
        },
        ImAction::Reload => {
            let client = match resolve_client() {
                Ok(client) => client,
                Err(error) => return report_im_error(error, json),
            };
            let mount = match client.im_reload().await {
                Ok(mount) => mount,
                Err(error) => return report_im_error(error, json),
            };
            handle_write_result(render_reload_result(&mount, json, &mut io::stdout()), json)
        }
    }
}

fn render_auth_set_reply(
    platform: &str,
    reply: &ImAuthSetReply,
    json: bool,
    out: &mut impl Write,
) -> io::Result<ExitCode> {
    if json {
        let encoded = serde_json::to_string(reply).map_err(io::Error::other)?;
        writeln!(out, "{encoded}")?;
    } else {
        writeln!(out, "{platform} 凭据已保存")?;
        writeln!(out, "IM：{}", format_mount_result(&reply.mount))?;
    }
    Ok(ExitCode::SUCCESS)
}

fn render_auth_remove_reply(
    platform: &str,
    reply: &ImAuthRemoveReply,
    json: bool,
    out: &mut impl Write,
) -> io::Result<ExitCode> {
    if json {
        let encoded = serde_json::to_string(reply).map_err(io::Error::other)?;
        writeln!(out, "{encoded}")?;
    } else {
        if reply.removed {
            writeln!(out, "{platform} 凭据已删除")?;
        } else {
            writeln!(out, "{platform} 凭据未配置")?;
        }
        writeln!(out, "IM：{}", format_mount_result(&reply.mount))?;
    }
    Ok(ExitCode::SUCCESS)
}

fn render_auth_status(
    status: &ImAuthStatus,
    json: bool,
    out: &mut impl Write,
) -> io::Result<ExitCode> {
    if json {
        let encoded = serde_json::to_string(status).map_err(io::Error::other)?;
        writeln!(out, "{encoded}")?;
    } else {
        for row in &status.platforms {
            match &row.credential {
                ImCredentialStatus::Present => writeln!(out, "{}：已配置", row.platform)?,
                ImCredentialStatus::Missing => writeln!(out, "{}：未配置", row.platform)?,
                ImCredentialStatus::Invalid { reason } => {
                    writeln!(out, "{}：无效；{reason}", row.platform)?;
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn render_reload_result(
    mount: &ImMountResult,
    json: bool,
    out: &mut impl Write,
) -> io::Result<ExitCode> {
    if json {
        let encoded = serde_json::to_string(mount).map_err(io::Error::other)?;
        writeln!(out, "{encoded}")?;
    } else {
        writeln!(out, "IM：{}", format_mount_result(mount))?;
    }
    let code = match mount {
        ImMountResult::Mounted | ImMountResult::Disabled => 0,
        ImMountResult::UsingPreviousConfiguration { .. } | ImMountResult::Unmounted { .. } => 1,
    };
    Ok(ExitCode::from(code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_local::ImPlatformAuthStatus;

    #[test]
    fn validate_platform_rejects_unknown_without_echoing_input() {
        assert!(validate_platform("telegram").is_ok());
        let leaked_token = "123456:ABC-DEF-SECRET-TOKEN";
        let err = validate_platform(leaked_token).unwrap_err();
        assert_eq!(err.kind, LocalErrorKind::BadRequest);
        assert!(
            !err.message.contains(leaked_token),
            "平台校验错误严禁回显输入参数：{}",
            err.message
        );
        assert!(err.message.contains("telegram"));
    }

    #[tokio::test]
    async fn invalid_platform_exits_2_immediately_without_waiting_on_stdin() {
        let leaked_token = "999999:MISPLACED_SECRET_PLATFORM_ARG";
        let code = run(
            ImAction::Auth {
                action: ImAuthAction::Set {
                    platform: leaked_token.into(),
                },
            },
            false,
        )
        .await
        .unwrap();
        assert_eq!(code, ExitCode::from(2));

        let json_err = run(
            ImAction::Auth {
                action: ImAuthAction::Remove {
                    platform: leaked_token.into(),
                },
            },
            true,
        )
        .await
        .unwrap_err();
        let local = json_err.downcast_ref::<LocalError>().unwrap();
        assert_eq!(local.kind, LocalErrorKind::BadRequest);
        assert!(!local.message.contains(leaked_token));
    }

    #[test]
    fn text_and_json_rendering_and_reload_exit_codes_match_contract() {
        // 1. im auth set：无论挂载结果如何均退出 0
        for (mount, expected_line) in [
            (ImMountResult::Mounted, "IM：已挂载"),
            (ImMountResult::Disabled, "IM：未启用"),
            (
                ImMountResult::Unmounted {
                    reason: "配置无效".into(),
                },
                "IM：未挂载；配置无效",
            ),
            (
                ImMountResult::UsingPreviousConfiguration {
                    reason: "workspace 不是绝对路径".into(),
                },
                "IM：已挂载；仍按上一份配置运行；workspace 不是绝对路径",
            ),
        ] {
            let reply = ImAuthSetReply { mount };
            let mut out = Vec::new();
            let code = render_auth_set_reply("telegram", &reply, false, &mut out).unwrap();
            assert_eq!(code, ExitCode::SUCCESS);
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!("telegram 凭据已保存\n{expected_line}\n")
            );

            let mut json_out = Vec::new();
            let json_code = render_auth_set_reply("telegram", &reply, true, &mut json_out).unwrap();
            assert_eq!(json_code, ExitCode::SUCCESS);
            assert_eq!(
                serde_json::from_slice::<ImAuthSetReply>(&json_out).unwrap(),
                reply
            );
        }

        // 2. im auth remove：区分已删除 / 未配置，均退出 0
        for (removed, first_line) in [
            (true, "telegram 凭据已删除"),
            (false, "telegram 凭据未配置"),
        ] {
            let reply = ImAuthRemoveReply {
                removed,
                mount: ImMountResult::Unmounted {
                    reason: "缺少 Telegram 平台凭据（im/auth.json）".into(),
                },
            };
            let mut out = Vec::new();
            let code = render_auth_remove_reply("telegram", &reply, false, &mut out).unwrap();
            assert_eq!(code, ExitCode::SUCCESS);
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!("{first_line}\nIM：未挂载；缺少 Telegram 平台凭据（im/auth.json）\n")
            );

            let mut json_out = Vec::new();
            let json_code =
                render_auth_remove_reply("telegram", &reply, true, &mut json_out).unwrap();
            assert_eq!(json_code, ExitCode::SUCCESS);
            assert_eq!(
                serde_json::from_slice::<ImAuthRemoveReply>(&json_out).unwrap(),
                reply
            );
        }

        // 3. im auth status：Present / Missing / Invalid
        let status = ImAuthStatus {
            platforms: vec![
                ImPlatformAuthStatus {
                    platform: "telegram".into(),
                    credential: ImCredentialStatus::Present,
                },
                ImPlatformAuthStatus {
                    platform: "other".into(),
                    credential: ImCredentialStatus::Missing,
                },
                ImPlatformAuthStatus {
                    platform: "broken".into(),
                    credential: ImCredentialStatus::Invalid {
                        reason: "im/auth.json 权限过宽".into(),
                    },
                },
            ],
        };
        let mut status_out = Vec::new();
        assert_eq!(
            render_auth_status(&status, false, &mut status_out).unwrap(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            String::from_utf8(status_out).unwrap(),
            "telegram：已配置\nother：未配置\nbroken：无效；im/auth.json 权限过宽\n"
        );
        let mut status_json = Vec::new();
        assert_eq!(
            render_auth_status(&status, true, &mut status_json).unwrap(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            serde_json::from_slice::<ImAuthStatus>(&status_json).unwrap(),
            status
        );

        // 4. im reload：Mounted / Disabled 退出 0，UsingPreviousConfiguration / Unmounted 退出 1
        for (mount, expected_code, expected_text) in [
            (ImMountResult::Mounted, 0u8, "IM：已挂载\n"),
            (ImMountResult::Disabled, 0u8, "IM：未启用\n"),
            (
                ImMountResult::UsingPreviousConfiguration {
                    reason: "解析失败".into(),
                },
                1u8,
                "IM：已挂载；仍按上一份配置运行；解析失败\n",
            ),
            (
                ImMountResult::Unmounted {
                    reason: "缺少凭据".into(),
                },
                1u8,
                "IM：未挂载；缺少凭据\n",
            ),
        ] {
            let mut out = Vec::new();
            let code = render_reload_result(&mount, false, &mut out).unwrap();
            assert_eq!(code, ExitCode::from(expected_code));
            assert_eq!(String::from_utf8(out).unwrap(), expected_text);

            let mut json_out = Vec::new();
            let json_code = render_reload_result(&mount, true, &mut json_out).unwrap();
            assert_eq!(json_code, ExitCode::from(expected_code));
            assert_eq!(
                serde_json::from_slice::<ImMountResult>(&json_out).unwrap(),
                mount
            );
        }
    }

    #[tokio::test]
    async fn im_secret_reader_supports_pty_no_echo_and_untrimmed_pipe_input() {
        use crate::commands::secret_input::{TerminalInput, read_pipe_secret, read_terminal_line};
        use std::{
            fs::File,
            os::fd::{AsRawFd, FromRawFd},
        };

        // 管道输入仅剥离单个行尾，保留两侧空格。
        let piped = read_pipe_secret(
            &mut " 123456:token_with_space \r\n".as_bytes(),
            IM_INVALID_UTF8_MESSAGE,
        )
        .unwrap();
        assert_eq!(piped, " 123456:token_with_space ");

        let bad_utf8 =
            read_pipe_secret(&mut [0xffu8, b'\n'].as_slice(), IM_INVALID_UTF8_MESSAGE).unwrap_err();
        match bad_utf8 {
            SecretReadError::InvalidUtf8(err) => {
                assert_eq!(err.kind, LocalErrorKind::BadRequest);
                assert_eq!(err.message, IM_INVALID_UTF8_MESSAGE);
            }
            other => panic!("非法 UTF-8 应返回 InvalidUtf8：{other:?}"),
        }

        // PTY 无回显与规范行退格编辑。
        let _guard = crate::test_support::signal_guard().await;
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        let mut master = unsafe { File::from_raw_fd(master) };
        let mut slave = unsafe { File::from_raw_fd(slave) };
        {
            let _term = TerminalInput::new(slave.as_raw_fd()).unwrap();
            master.write_all(b"12345:bot-tokex\x7fn\n").unwrap();
            let got = read_terminal_line(&mut slave, IM_INVALID_UTF8_MESSAGE).unwrap();
            assert_eq!(got, "12345:bot-token");
            let mut echoed = 0i32;
            assert_eq!(
                unsafe { libc::ioctl(master.as_raw_fd(), libc::FIONREAD, &mut echoed) },
                0
            );
            assert_eq!(echoed, 0, "无回显终端录入不得把凭据回显到 PTY master");
        }
    }
}
