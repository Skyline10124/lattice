// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `PtyTuiDriver`：经伪终端（PTY）驱动 TUI，用于冒烟场景。
//!
//! 依据 #41 的第三项决定，PTY 只用于少数冒烟场景，证明 TUI 入口仍接入与 CLI 相同的
//! 后台提交与执行链路。颜色、提示符形状与布局属于展示细节，场景断言只核对行为；就绪
//! 识别（进入备用屏幕）与 ANSI 转义清理都在本驱动内部完成。
//!
//! 只支持 Linux。驱动基于通用测试库 `portable-pty`，符合测试套件的依赖边界。

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use thiserror::Error;

use crate::driver::{LatticeDriver, kill_process_group};
use crate::world::ScenarioWorld;

/// 从 PTY 持续读取的最大字节预算。全屏 TUI 会重绘整帧，字节数与可见文本长度不再成正比；
/// 4 MiB 既能约束失控的死循环重绘，又不会将正常会话误判为输出超限。
const READ_BUDGET: usize = 4 * 1024 * 1024;

/// CSI 进入备用屏幕序列（`\x1b[?1049h`）。这是终端协议事实而非视觉布局细节，
/// 驱动借此判定全屏 TUI 已接管 PTY 并准备好接收输入。
const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";

#[derive(Debug, Error)]
pub enum PtyError {
    #[error("伪终端读写失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("伪终端错误: {0}")]
    Pty(String),
    /// 等待匹配输出超时；附带一段剥离 ANSI 转义后的有界末尾输出，以便区分静默等待与进程卡死。
    #[error("TUI 未在 {0:?} 内产出匹配输出；末尾输出: {1:?}")]
    Timeout(Duration, String),
}

/// 绑定在活跃 PTY 上的 TUI 会话驱动；`Drop` 时终止并回收子进程，确保场景结束后无残留进程。
pub struct PtyTuiDriver {
    /// 保持存活以维持主端打开；后台读取线程持有其克隆读取端。
    #[allow(dead_code)]
    master: Box<dyn MasterPty + Send>,
    master_writer: Box<dyn Write + Send>,
    /// 后台读取线程排空的输出块通道。阻塞读取仅发生在该后台线程内，
    /// 使 `drain_until` 在 PTY 静默时仍能严格遵守截止时间。
    rx: Receiver<Vec<u8>>,
    /// 在 `Drop` 或超时时尽力直接终止子进程。
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    /// 子进程句柄，供 `kill_and_reap` 在发送 `SIGKILL` 后经 `try_wait` 回收僵尸进程；
    /// 否则僵尸进程仍会留在 `/proc/<pid>/stat` 中占用同一进程组号。
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    /// 主端报告的进程组首领 PID（转为 `u32` 以对齐 `driver::kill_process_group` 与 `processes_in_pgid`）；
    /// 未报告有效正整数首领时为 `None`。
    pgid: Option<u32>,
    /// 迄今已排空的完整原始 PTY 输出（保留 ANSI 序列）。
    output: Vec<u8>,
}

impl PtyTuiDriver {
    /// 在场景世界的 PTY 中启动 `lattice tui --agent <agent>`（假 provider 须已提前接线）。
    /// 子进程在 PTY 控制终端的新会话与进程组中运行，`process_group_leader()` 返回其进程组号。
    pub fn spawn(
        driver: &LatticeDriver,
        world: &ScenarioWorld,
        agent: &str,
    ) -> Result<Self, PtyError> {
        Self::spawn_args(driver, world, &["tui", "--agent", agent])
    }

    /// 重连只选择已存在 Session，不隐式提交或恢复旧 Turn。
    pub fn reconnect(
        driver: &LatticeDriver,
        world: &ScenarioWorld,
        session_id: &str,
    ) -> Result<Self, PtyError> {
        Self::spawn_args(driver, world, &["tui", session_id])
    }

    fn spawn_args(
        driver: &LatticeDriver,
        world: &ScenarioWorld,
        args: &[&str],
    ) -> Result<Self, PtyError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Pty(e.to_string()))?;

        let mut cmd = CommandBuilder::new(driver.binary_path());
        cmd.args(args);
        cmd.cwd(world.project_dir());
        cmd.env_clear();
        for (k, v) in world.env_summary() {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        let killer = child.clone_killer();
        // `process_group_leader` 返回 `i32`（`pid_t`）；有效组首领必为正整数，非正值折叠为 `None`，
        // 并统一转为 `u32` 以匹配 `driver::kill_process_group` 与 `processes_in_pgid`。
        let pgid = pair
            .master
            .process_group_leader()
            .filter(|p| *p > 0)
            .map(|p| p as u32);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Pty(e.to_string()))?;

        // 立即释放从端，使主端关闭时能向子进程传递 EOF。
        drop(pair.slave);

        // 唯一的阻塞读取留在该后台线程内，驱动本身只在通道上按超时等待；
        // 全屏 TUI 在画面无变化时完全静默，若等 `read` 返回后再检查截止时间就会无限挂起。
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            master: pair.master,
            master_writer: writer,
            rx,
            killer,
            child: Some(child),
            pgid,
            output: Vec::new(),
        })
    }

    /// 向 TUI 输入框发送一行并以回车符 `\r` 结尾（与真实终端按下回车键时发送的字节一致）。
    ///
    /// 必须使用 `\r` 而非 `\n`：全屏 TUI 运行在 raw 模式下，`crossterm` 在 raw 模式关闭行规程后
    /// 会将 `\n` 解析为 Ctrl+J 而非 `KeyCode::Enter`（`crossterm` #371），用 `\n` 结尾只会留在输入缓冲区中而不会提交。
    pub fn send_line(&mut self, line: &str) -> Result<(), PtyError> {
        self.master_writer
            .write_all(line.as_bytes())
            .and_then(|_| self.master_writer.write_all(b"\r"))?;
        self.master_writer.flush()?;
        Ok(())
    }

    /// 发送原始按键字节而不自动附加回车，供取消与仅断开连接的产品交互使用。
    pub fn send_keys(&mut self, keys: &[u8]) -> Result<(), PtyError> {
        self.master_writer.write_all(keys)?;
        self.master_writer.flush()?;
        Ok(())
    }

    /// 持续读取 PTY 输出，直到剥离 ANSI 转义并折叠连续空白后的累积文本满足 `predicate`，
    /// 或超出 `timeout`；成功时返回累积的原始字节。
    ///
    /// 谓词针对剥离转义并折叠空白后的语义文本匹配，使冒烟场景无需耦合颜色、光标移动或 24x80 折行位置。
    pub fn wait_for(
        &mut self,
        timeout: Duration,
        predicate: impl Fn(&str) -> bool,
    ) -> Result<Vec<u8>, PtyError> {
        self.drain_until(timeout, |raw| {
            let stripped = strip_ansi(&String::from_utf8_lossy(raw));
            predicate(&collapse_ws(&stripped))
        })
    }

    /// 用原生窗口大小调整触发完整重绘，返回单个已完成帧的文本而非历史输出拼接。
    /// 仅用于画面稳定的门闩窗口；不模拟通用终端，也不检查布局或颜色。
    pub fn redraw_text(&mut self, timeout: Duration) -> Result<String, PtyError> {
        let offset = self.output.len();
        let mut size = self
            .master
            .get_size()
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        size.cols = if size.cols == 80 { 81 } else { 80 };
        self.master
            .resize(size)
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        self.drain_until(timeout, |raw| full_redraw(&raw[offset..]).is_some())?;
        let frame = full_redraw(&self.output[offset..]).expect("重绘帧必须完整");
        Ok(collapse_ws(&strip_ansi(&String::from_utf8_lossy(frame))))
    }

    /// 持续读取 PTY 输出，直到保留 ANSI 序列的累积原始字节满足 `predicate`；
    /// 用于检测进入备用屏幕等会被 `strip_ansi` 剥离的终端协议序列。
    pub fn wait_for_raw(
        &mut self,
        timeout: Duration,
        predicate: impl Fn(&[u8]) -> bool,
    ) -> Result<Vec<u8>, PtyError> {
        self.drain_until(timeout, predicate)
    }

    /// 等待 TUI 进入备用屏幕。该序列 `\x1b[?1049h` 封装在驱动内部而绝不散落到场景中：
    /// 场景需要确认 TUI 已可接收输入时统一调用本方法，而非匹配提示符字符。
    pub fn wait_for_ready(&mut self, timeout: Duration) -> Result<Vec<u8>, PtyError> {
        self.wait_for_raw(timeout, |bytes| {
            bytes
                .windows(ENTER_ALTERNATE_SCREEN.len())
                .any(|window| window == ENTER_ALTERNATE_SCREEN)
        })
    }

    fn drain_until(
        &mut self,
        timeout: Duration,
        mut predicate: impl FnMut(&[u8]) -> bool,
    ) -> Result<Vec<u8>, PtyError> {
        let start = Instant::now();
        // 先前等待期间已排空的字节可能已经满足条件。
        if predicate(&self.output) {
            return Ok(self.output.clone());
        }
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(PtyError::Timeout(timeout, self.tail_for_diagnostics()));
            }
            match self
                .rx
                .recv_timeout(remaining.min(Duration::from_millis(200)))
            {
                Ok(chunk) => {
                    self.output.extend_from_slice(&chunk);
                    if self.output.len() > READ_BUDGET {
                        return Err(PtyError::Pty(format!(
                            "伪终端输出超出 {READ_BUDGET} 字节上限"
                        )));
                    }
                    if predicate(&self.output) {
                        return Ok(self.output.clone());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // 读取线程已结束（子进程关闭了伪终端），后续不会再有新字节到达，立即失败而不空耗剩余超时预算。
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(PtyError::Timeout(
                        start.elapsed(),
                        format!("伪终端已关闭；{}", self.tail_for_diagnostics()),
                    ));
                }
            }
        }
    }

    /// 剥离 ANSI 转义并限制长度的末尾输出摘要，供错误诊断使用。
    fn tail_for_diagnostics(&self) -> String {
        let text = collapse_ws(&strip_ansi(&String::from_utf8_lossy(&self.output)));
        let tail: String = text
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        tail
    }

    /// 迄今累积的原始 PTY 输出字节。
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// 子进程的进程组首领 PID（若 PTY 报告了有效值），类型为与 `driver::kill_process_group` 一致的 `u32`。
    pub fn pgid(&self) -> Option<u32> {
        self.pgid
    }

    /// 终止整个进程组并尽力回收直接子进程。
    /// 统一委托 `driver::kill_process_group` 发送组信号，并以有界 `try_wait` 轮询回收子进程，避免留下僵尸进程。
    pub fn kill_and_reap(&mut self) {
        let _ = self.killer.kill();
        if let Some(pgid) = self.pgid {
            kill_process_group(pgid);
        }
        // 以最多 2 秒的有界轮询回收直接子进程，避免卡死的子进程永久阻塞 `Drop`。
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break, // 无法进一步处理，直接跳出以免阻塞 `Drop`
                }
            }
            // 若到截止时间仍未回收则丢弃句柄：已在有界窗口内尽力清理。
        }
    }

    /// 等待子进程自行干净退出而不主动发信号杀死它。
    ///
    /// 验证 `/quit` 等主动退出行为的场景必须在 `Drop` 之前调用本方法观察自然退出，
    /// 否则 `kill_and_reap` 的 `SIGKILL` 会掩盖 `/quit` 失效的问题。
    /// 等待子进程自然退出，排空已写出的伪终端输出并返回其退出码。
    pub fn wait_for_exit_code(&mut self, timeout: Duration) -> Result<u32, PtyError> {
        let start = Instant::now();
        let code = {
            let child = match self.child.as_mut() {
                Some(child) => child,
                None => return Ok(0),
            };
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status.exit_code(),
                    Ok(None) => {}
                    Err(e) => return Err(PtyError::Pty(e.to_string())),
                }
                if start.elapsed() >= timeout {
                    return Err(PtyError::Timeout(timeout, self.tail_for_diagnostics()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        };
        // 子进程退出后短暂排空后台读取通道，保留退出前写入伪终端的错误文本。
        let drain_until = Instant::now() + Duration::from_millis(100);
        while let Ok(chunk) = self
            .rx
            .recv_timeout(drain_until.saturating_duration_since(Instant::now()))
        {
            self.output.extend_from_slice(&chunk);
            if Instant::now() >= drain_until {
                break;
            }
        }
        Ok(code)
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> Result<(), PtyError> {
        let code = self.wait_for_exit_code(timeout)?;
        if code == 0 {
            Ok(())
        } else {
            Err(PtyError::Pty(format!("TUI 未干净退出: 退出码 {code}")))
        }
    }
}

impl Drop for PtyTuiDriver {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

// `ratatui` 的全屏尺寸调整会先清屏并重置后台缓冲区；`crossterm` 在完整字符差异刷新后写出光标显隐序列。
// 此处只截取该完整帧，排除其前后的增量重绘。
fn full_redraw(raw: &[u8]) -> Option<&[u8]> {
    let clear = b"\x1b[2J";
    let start = raw.windows(clear.len()).position(|part| part == clear)? + clear.len();
    let frame = &raw[start..];
    let end = frame
        .windows(6)
        .position(|part| part == b"\x1b[?25h" || part == b"\x1b[?25l")?;
    Some(&frame[..end])
}

/// 从 `text` 中剥离 ANSI 转义序列（CSI、OSC 与双字符转义），仅用于谓词文本匹配；
/// 原始输出仍独立保留。
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // 已消费 ESC，根据紧随其后的字符判定转义类别。
        match chars.next() {
            Some('[') => {
                // CSI 序列：跳过参数与中间字节，直到遇到 `0x40..=0x7e` 终止字节。
                #[allow(clippy::while_let_on_iterator)]
                while let Some(t) = chars.next() {
                    if ('@'..='~').contains(&t) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC 序列：跳过内容直到遇到 BEL（`\x07`）或 ST（`\x1b\\`）。
                #[allow(clippy::while_let_on_iterator)]
                while let Some(t) = chars.next() {
                    if t == '\x07' {
                        break;
                    }
                    if t == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some(other) => {
                // 简单双字符转义（例如 `\x1bc` 终端重置）。
                let _ = other;
            }
            None => break,
        }
    }
    out
}

/// 将连续空白（含折行产生的换行）折叠为单个空格，使场景谓词只匹配语义文本而不耦合固定终端列宽。
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_redraw_requires_a_complete_frame_and_excludes_other_paints() {
        let raw = b"old paint\x1b[2J\x1b[1;1Hprefix\x1b[2;1Hprefix\x1b[?25hnext paint";
        let end = raw
            .windows(6)
            .position(|part| part == b"\x1b[?25h")
            .unwrap()
            + 6;
        for cut in 0..end {
            assert!(
                full_redraw(&raw[..cut]).is_none(),
                "位置 {cut} 处的半截帧不应被识别为完整重绘"
            );
        }
        let frame = full_redraw(raw).unwrap();
        assert_eq!(frame, b"\x1b[1;1Hprefix\x1b[2;1Hprefix");
        assert_eq!(
            strip_ansi(std::str::from_utf8(frame).unwrap())
                .matches("prefix")
                .count(),
            2
        );
        assert_eq!(full_redraw(b"\x1b[2Jtext\x1b[?25l"), Some(&b"text"[..]));
    }

    #[test]
    fn strip_ansi_removes_color_cursor_and_title_sequences() {
        for (name, input, expected) in [
            (
                "color_and_cursor",
                "\x1b[32m> \x1b[0mhello\x1b[1mworld\x1b[0m\x1b[?25h",
                "> helloworld",
            ),
            ("osc_title", "\x1b]0;some title\x07prompt: ", "prompt: "),
            ("st_terminated_osc", "\x1b]2;window title\x1b\\done", "done"),
        ] {
            assert_eq!(strip_ansi(input), expected, "{name}");
        }
    }

    #[test]
    fn collapse_ws_flattens_wrapped_frames() {
        assert_eq!(collapse_ws("hello\n  world"), "hello world");
        assert_eq!(collapse_ws("pong"), "pong");
    }
}
