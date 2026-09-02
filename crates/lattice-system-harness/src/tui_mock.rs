// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Reusable fake-provider scenarios for interactive TUI debugging.

use std::fmt;
use std::process::ExitStatus;
use std::str::FromStr;

use thiserror::Error;

use crate::{
    DriverError, FAKE_API_KEY, FakeLlmServer, LatticeDriver, ScenarioWorld, ServeError, SseScript,
    WorldError,
};

const AGENT: &str = "tui-mock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiMockScenario {
    LongText,
    ToolCall,
    Disconnect,
    Slow,
    Error,
}

impl TuiMockScenario {
    pub const ALL: [Self; 5] = [
        Self::LongText,
        Self::ToolCall,
        Self::Disconnect,
        Self::Slow,
        Self::Error,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::LongText => "long-text",
            Self::ToolCall => "tool-call",
            Self::Disconnect => "disconnect",
            Self::Slow => "slow",
            Self::Error => "error",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::LongText => "long deterministic transcript",
            Self::ToolCall => "session.todo_write lifecycle",
            Self::Disconnect => "partial text followed by disconnect",
            Self::Slow => "response stalls until Esc cancellation",
            Self::Error => "deterministic provider HTTP 500",
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
                        tool_name: "session.todo_write".into(),
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
            Self::ToolCall => r#"["session.todo_write"]"#,
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
    let profile = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{AGENT}\"\nmodel = \"compat:{AGENT}\"\n\n\
         [prompt]\nsystem = \"Respond according to the deterministic mock script.\"\n\n\
         [tools]\nenabled = {}\n",
        scenario.enabled_tools()
    );
    world.write_project_file(&format!(".lattice/agents/{AGENT}/agent.toml"), &profile)?;
    world.init_git_project()?;

    let server = scenario.start_server()?;
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", AGENT);
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);

    // ScenarioWorld::command() 有意清空子进程环境以保证 harness 隔离；
    // 交互式 TUI 还需要终端能力变量，否则真实的 TUI 子进程探测不到
    // COLORTERM，会回退为 ANSI 命名色而非设计的 24-bit RGB 调色板。
    for key in ["TERM", "COLORTERM", "LANG", "LC_ALL"] {
        if let Ok(value) = std::env::var(key) {
            world.set_env(key, value);
        }
    }

    eprintln!(
        "TUI mock scenario: {scenario} ({})\nSubmit any prompt to run it.{}",
        scenario.description(),
        if scenario == TuiMockScenario::Slow {
            " Press Esc after submission to exercise cancellation."
        } else {
            ""
        }
    );

    Ok(world
        .command(driver.binary_path())
        .args(["tui", "--agent", AGENT, "--project-branch", "main"])
        .status()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_names_round_trip() {
        for scenario in TuiMockScenario::ALL {
            assert_eq!(
                scenario.name().parse::<TuiMockScenario>().unwrap(),
                scenario
            );
        }
    }

    #[test]
    fn tool_call_is_the_only_scenario_that_enables_a_tool() {
        for scenario in TuiMockScenario::ALL {
            assert_eq!(
                scenario.enabled_tools() != "[]",
                scenario == TuiMockScenario::ToolCall
            );
        }
    }

    #[test]
    fn scenarios_select_the_expected_script_shapes() {
        assert!(matches!(
            TuiMockScenario::LongText.script(),
            SseScript::PlainText { chunks, .. } if chunks.len() == 32
        ));
        assert!(matches!(
            TuiMockScenario::ToolCall.script(),
            SseScript::Sequence { scripts } if scripts.len() == 2
        ));
        assert!(matches!(
            TuiMockScenario::Disconnect.script(),
            SseScript::Disconnect { chunks_before_drop } if chunks_before_drop.len() == 1
        ));
        assert!(matches!(
            TuiMockScenario::Slow.script(),
            SseScript::PlainText { .. }
        ));
        assert!(matches!(
            TuiMockScenario::Error.script(),
            SseScript::HttpError { status: 500, .. }
        ));
    }

    #[test]
    fn every_scenario_starts_a_local_server() {
        for scenario in TuiMockScenario::ALL {
            drop(scenario.start_server().expect("start scenario server"));
        }
    }
}
