// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

// 信号处置属于整个进程，安装处理函数或向本进程发信号的测试共用此锁。
pub(crate) async fn signal_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static SIGNALS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    SIGNALS.lock().await
}
