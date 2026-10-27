// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Reusable fake-provider scenarios for interactive TUI debugging.

use std::fmt;
use std::process::ExitStatus;
use std::str::FromStr;

use thiserror::Error;

use crate::{
    DriverError, FAKE_API_KEY, FakeLlmServer, LatticeDriver, ProviderFixture, ScenarioWorld,
    ServeError, SseScript, WorldError,
};

const AGENT: &str = "tui-mock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiMockScenario {
    LongText,
    ToolCall,
    Disconnect,
    Slow,
    Error,
    Flaky,
}

impl TuiMockScenario {
    pub const ALL: [Self; 6] = [
        Self::LongText,
        Self::ToolCall,
        Self::Disconnect,
        Self::Slow,
        Self::Error,
        Self::Flaky,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::LongText => "long-text",
            Self::ToolCall => "tool-call",
            Self::Disconnect => "disconnect",
            Self::Slow => "slow",
            Self::Error => "error",
            Self::Flaky => "flaky",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::LongText => "确定性的长对话记录",
            Self::ToolCall => "session__todo_write 工具生命周期",
            Self::Disconnect => "输出部分文本后断开连接",
            Self::Slow => "响应保持停滞，等待 Ctrl+C 取消",
            Self::Error => "稳定返回提供方 HTTP 500",
            Self::Flaky => "首次请求返回 HTTP 500，重试后成功",
        }
    }

    pub fn script(self) -> SseScript {
        match self {
            Self::LongText => SseScript::PlainText {
                chunks: (1..=32)
                    .map(|line| {
                        format!("mock line {line:02}: deterministic TUI transcript content.\n")
                    })
                    .collect(),
                finish_reason: "stop".into(),
            },
            Self::ToolCall => SseScript::Sequence {
                scripts: vec![
                    SseScript::ToolCall {
                        call_id: "mock-tool-call-1".into(),
                        tool_name: "session__todo_write".into(),
                        arguments_json:
                            r#"{"todos":[{"content":"inspect mock TUI","status":"in_progress"}]}"#
                                .into(),
                    },
                    SseScript::plain("mock tool call complete"),
                ],
            },
            Self::Disconnect => SseScript::Disconnect {
                chunks_before_drop: vec!["partial mock response before disconnect".into()],
            },
            Self::Slow => SseScript::plain("released mock response"),
            Self::Error => SseScript::HttpError {
                status: 500,
                body: r#"{"error":{"message":"mock provider failure"}}"#.into(),
            },
            // Cycle 按请求轮转、永不收敛到最后一个脚本，所以同一 TUI
            // 会话里每一次 prompt 都重演「第一次 500、重试成功」；用于
            // 真机验证 WARN Dock 在 turn 成功后不自动消失（§14.3 无自
            // 动恢复），直到 Space 释放。
            Self::Flaky => SseScript::Cycle {
                scripts: vec![
                    SseScript::HttpError {
                        status: 500,
                        body: r#"{"error":{"message":"mock provider failure"}}"#.into(),
                    },
                    SseScript::plain("mock response after retry"),
                ],
            },
        }
    }

    pub fn start_server(self) -> Result<FakeLlmServer, ServeError> {
        if self == Self::Slow {
            Ok(FakeLlmServer::start_gated(self.script())?.0)
        } else {
            FakeLlmServer::start(self.script())
        }
    }

    fn enabled_tools(self) -> &'static str {
        match self {
            Self::ToolCall => r#"["session__todo_write"]"#,
            _ => "[]",
        }
    }
}

impl fmt::Display for TuiMockScenario {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for TuiMockScenario {
    type Err = TuiMockError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|scenario| scenario.name() == value)
            .ok_or_else(|| TuiMockError::UnknownScenario(value.into()))
    }
}

#[derive(Debug, Error)]
pub enum TuiMockError {
    #[error("unknown TUI mock scenario '{0}'")]
    UnknownScenario(String),
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error(transparent)]
    World(#[from] WorldError),
    #[error(transparent)]
    Server(#[from] ServeError),
    #[error("failed to run lattice TUI: {0}")]
    Io(#[from] std::io::Error),
}

pub fn run_tui_mock(scenario: TuiMockScenario) -> Result<ExitStatus, TuiMockError> {
    let driver = LatticeDriver::from_env()?;
    let world = ScenarioWorld::new("tui-mock")?;
    let server = scenario.start_server()?;
    let model = world.wire_provider(
        ProviderFixture::chat_completions(AGENT),
        &server,
        FAKE_API_KEY,
    )?;
    let profile = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{AGENT}\"\nmodel = \"{model}\"\n\n\
         [prompt]\nsystem = \"Respond according to the deterministic mock script.\"\n\n\
         [tools]\nenabled = {}\n",
        scenario.enabled_tools()
    );
    world.write_project_file(&format!(".lattice/agents/{AGENT}/agent.toml"), &profile)?;
    world.init_git_project()?;

    // ScenarioWorld::command() 有意清空子进程环境以保证 harness 隔离；
    // 交互式调试保留宿主的终端与 locale 环境；TUI 固定使用 RGB 和 Unicode，
    // 不依赖这些变量选择颜色或符号。
    for key in ["TERM", "COLORTERM", "LANG", "LC_ALL"] {
        if let Ok(value) = std::env::var(key) {
            world.set_env(key, value);
        }
    }

    eprintln!(
        "TUI mock scenario: {scenario} ({})\nSubmit any prompt to run it.{}",
        scenario.description(),
        if scenario == TuiMockScenario::Slow {
            " Press Ctrl+C after submission to exercise cancellation."
        } else {
            ""
        }
    );

    Ok(world
        .command(driver.binary_path())
        .args(["tui", "--agent", AGENT])
        .status()?)
}
