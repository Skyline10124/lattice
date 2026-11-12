// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_local::{
    AuthCatalogOutcome, AuthLoginEvent, AuthLoginOutcome, LocalAuthLogin, LocalError,
    LocalErrorKind, SensitiveLoginUrl,
};
use std::{
    ffi::OsStr,
    io::{self, Write},
    process::{ExitCode, Stdio},
};
use tokio::{
    process::{Child, Command},
    signal::unix::{SignalKind, signal},
};

mod input;
use input::{InputEvent, LoginInput};

fn open_browser(program: &OsStr, url: &SensitiveLoginUrl) -> io::Result<Child> {
    Command::new(program)
        .arg(url.expose())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
}

fn show_url(url: &SensitiveLoginUrl, err: &mut impl Write) -> io::Result<()> {
    writeln!(err, "Open this URL to sign in:\n{}", url.expose())
}

pub(super) async fn run(login: LocalAuthLogin, json: bool) -> anyhow::Result<ExitCode> {
    let graphical =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    interact(
        login,
        json,
        LoginInput::stdin,
        graphical.then_some(OsStr::new("xdg-open")),
        &mut io::stdout(),
        &mut io::stderr(),
    )
    .await
}

#[derive(Default)]
struct Hooks {
    #[cfg(test)]
    authorized: Option<std::sync::Arc<tokio::sync::Notify>>,
    #[cfg(test)]
    buffered: Option<std::sync::Arc<tokio::sync::Notify>>,
    #[cfg(test)]
    write: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Semaphore>,
    )>,
    #[cfg(test)]
    write_failed: std::sync::atomic::AtomicBool,
}

impl Hooks {
    fn authorized(&self) {
        #[cfg(test)]
        if let Some(ready) = &self.authorized {
            ready.notify_one();
        }
    }
    fn buffered(&self) {
        #[cfg(test)]
        if let Some(ready) = &self.buffered {
            ready.notify_one();
        }
    }
    async fn before_write(&self) {
        #[cfg(test)]
        if let Some((entered, release)) = &self.write {
            entered.notify_one();
            release.acquire().await.expect("write gate").forget();
        }
    }
    fn wrote(&self, _failed: bool) {
        #[cfg(test)]
        self.write_failed
            .fetch_or(_failed, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn send_action(
    login: &mut LocalAuthLogin,
    callback: Option<SensitiveLoginUrl>,
    hooks: &Hooks,
) -> lattice_local::Result<()> {
    hooks.before_write().await;
    let result = match callback {
        Some(url) => login.submit_callback(url).await,
        None => login.cancel().await,
    };
    hooks.wrote(result.is_err());
    result
}

async fn interact(
    login: LocalAuthLogin,
    json: bool,
    input: impl FnOnce() -> io::Result<LoginInput>,
    browser_program: Option<&OsStr>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> anyhow::Result<ExitCode> {
    interact_with_hooks(
        login,
        json,
        input,
        browser_program,
        out,
        err,
        &Hooks::default(),
    )
    .await
}

async fn interact_with_hooks(
    mut login: LocalAuthLogin,
    json: bool,
    input: impl FnOnce() -> io::Result<LoginInput>,
    browser_program: Option<&OsStr>,
    out: &mut impl Write,
    err: &mut impl Write,
    hooks: &Hooks,
) -> anyhow::Result<ExitCode> {
    let mut interrupted = signal(SignalKind::interrupt())?;
    let mut terminated = signal(SignalKind::terminate())?;
    let mut hung_up = signal(SignalKind::hangup())?;
    let mut input = input()?;
    let mut input_ready = false;
    let mut browser: Option<Child> = None;
    let mut authorization = None;
    let mut presented = false;
    let mut callback = None;
    let mut ending = false;
    let mut input_error = None;
    let result = async {
        loop {
            if input_ready && !presented && !ending && let Some(url) = &authorization {
                match browser_program {
                    Some(program) => match open_browser(program, url) {
                        Ok(child) => browser = Some(child),
                        Err(_) => show_url(url, err)?,
                    },
                    None => show_url(url, err)?,
                }
                writeln!(err, "Complete sign-in in your browser, or paste the callback URL here (input hidden). Ctrl-C cancels.")?;
                presented = true;
            }
            if !ending && authorization.is_some() && let Some(url) = callback.take() {
                // 写失败不能裁定提交结果，继续读取后台的终态或明确读取失败。
                if send_action(&mut login, Some(url), hooks).await.is_err() { ending = true; }
            }
            tokio::select! {
                biased;
                event = login.next() => match event? {
                    Some(AuthLoginEvent::Authorization(url)) => {
                        authorization = Some(url);
                        hooks.authorized();
                    }
                    Some(AuthLoginEvent::Finished(outcome)) => {
                        if matches!(outcome, AuthLoginOutcome::Cancelled {}) && let Some(error) = input_error.take() {
                            return Err(input_failure(error));
                        }
                        return render(&outcome, json, out, err);
                    }
                    None => return Err(LocalError::from(io::Error::new(io::ErrorKind::UnexpectedEof, "login connection ended")).into()),
                },
                entered = input.next(), if !ending => {
                    match entered {
                        Ok(InputEvent::Ready) => input_ready = true,
                        Ok(InputEvent::Callback(url)) => { callback = Some(url); hooks.buffered(); }
                        Err(error) if !input_ready => return Err(LocalError::from(error).into()),
                        result => {
                            input_error = result.err();
                            let _ = send_action(&mut login, None, hooks).await;
                            ending = true;
                        }
                    }
                },
                _ = async { tokio::select! { _ = interrupted.recv() => {}, _ = terminated.recv() => {}, _ = hung_up.recv() => {} } }, if !ending => {
                    let _ = send_action(&mut login, None, hooks).await;
                    ending = true;
                },
                status = async { browser.as_mut().expect("browser branch enabled").wait().await }, if browser.is_some() => {
                    browser.take();
                    if !ending && !status.is_ok_and(|status| status.success()) && let Some(url) = &authorization { show_url(url, err)?; }
                },
            }
        }
    }.await;
    drop(input);
    if let Some(mut child) = browser {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result.map_err(|error: anyhow::Error| {
        if let Some(local) = error.downcast_ref::<LocalError>()
            && matches!(
                local.kind,
                LocalErrorKind::Transport | LocalErrorKind::NotRunning | LocalErrorKind::Protocol
            )
        {
            let mut local = local.clone();
            local
                .message
                .push_str("; sign-in result unknown; use lattice auth status to check the result");
            local.into()
        } else {
            error
        }
    })
}

pub(crate) fn input_failure(error: io::Error) -> anyhow::Error {
    if error.kind() == io::ErrorKind::InvalidInput {
        LocalError::new(
            LocalErrorKind::BadRequest,
            "callback URL must be UTF-8 and at most 16384 bytes",
        )
        .into()
    } else {
        error.into()
    }
}

fn render(
    outcome: &AuthLoginOutcome,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> anyhow::Result<ExitCode> {
    let mut value = serde_json::json!({"type":"auth","action":"login","provider":"chatgpt"});
    let code = match outcome {
        AuthLoginOutcome::SignedIn { status, catalog } => {
            value["status"] = status.availability.status_str().into();
            if let Some(email) = &status.email {
                value["email"] = email.clone().into();
            }
            if let Some(expiry) = &status.expires_at {
                value["expires_at"] = expiry.clone().into();
            }
            value["catalog"] = serde_json::to_value(catalog)?;
            if !json && let AuthCatalogOutcome::Failed { warning } = catalog {
                writeln!(
                    err,
                    "Signed in; ChatGPT model directory refresh failed: {warning}"
                )?;
            }
            ExitCode::SUCCESS
        }
        AuthLoginOutcome::Cancelled {} => {
            value["status"] = "cancelled".into();
            ExitCode::from(2)
        }
    };
    if json {
        writeln!(out, "{value}")?;
    } else {
        writeln!(out, "chatgpt {}", value["status"].as_str().unwrap())?;
    }
    Ok(code)
}

#[cfg(test)]
#[path = "login/tests.rs"]
mod tests;

#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn directory_failure_is_a_successful_login_with_a_structured_warning() {
        let outcome = AuthLoginOutcome::SignedIn {
            status: lattice_local::ProviderAuthStatus {
                provider: "chatgpt".into(),
                availability: lattice_local::Availability::Ready,
                email: None,
                expires_at: None,
            },
            catalog: AuthCatalogOutcome::Failed {
                warning: "HTTP 503".into(),
            },
        };
        for json in [false, true] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            assert_eq!(
                render(&outcome, json, &mut out, &mut err).unwrap(),
                ExitCode::SUCCESS
            );
            if json {
                let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
                assert_eq!(value["status"], "ready");
                assert_eq!(
                    value["catalog"],
                    serde_json::json!({"status":"failed","warning":"HTTP 503"})
                );
                assert!(err.is_empty());
            } else {
                assert_eq!(String::from_utf8(out).unwrap(), "chatgpt ready\n");
                assert!(
                    String::from_utf8(err)
                        .unwrap()
                        .contains("directory refresh failed: HTTP 503")
                );
            }
        }
    }
    #[test]
    fn final_json_contains_only_safe_status_and_cancel_is_exit_two() {
        let mut out = Vec::new();
        assert_eq!(
            render(
                &AuthLoginOutcome::Cancelled {},
                true,
                &mut out,
                &mut Vec::new()
            )
            .unwrap(),
            ExitCode::from(2)
        );
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"type":"auth","action":"login","provider":"chatgpt","status":"cancelled"})
        );
        out.clear();
        render(
            &AuthLoginOutcome::SignedIn {
                status: lattice_local::ProviderAuthStatus {
                    provider: "chatgpt".into(),
                    availability: lattice_local::Availability::Ready,
                    email: Some("user@example.com".into()),
                    expires_at: None,
                },
                catalog: AuthCatalogOutcome::Refreshed { model_count: 2 },
            },
            true,
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 6);
        assert_eq!(value["status"], "ready");
        assert_eq!(
            value["catalog"],
            serde_json::json!({"status":"refreshed","model_count":2})
        );
    }
}
