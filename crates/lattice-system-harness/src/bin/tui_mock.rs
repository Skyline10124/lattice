// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::process::ExitCode;

use lattice_system_harness::{TuiMockScenario, run_tui_mock};

fn usage() {
    eprintln!("usage: tui-mock [--scenario <name>] [--list]");
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let scenario = match args.next().as_deref() {
        None => TuiMockScenario::LongText,
        Some("--list") if args.next().is_none() => {
            for scenario in TuiMockScenario::ALL {
                println!("{:<12} {}", scenario.name(), scenario.description());
            }
            return ExitCode::SUCCESS;
        }
        Some("--scenario") => match (args.next(), args.next()) {
            (Some(name), None) => match name.parse() {
                Ok(scenario) => scenario,
                Err(error) => {
                    eprintln!("error: {error}");
                    usage();
                    return ExitCode::from(2);
                }
            },
            _ => {
                usage();
                return ExitCode::from(2);
            }
        },
        _ => {
            usage();
            return ExitCode::from(2);
        }
    };

    match run_tui_mock(scenario) {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(1, u8::MAX as i32) as u8),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
