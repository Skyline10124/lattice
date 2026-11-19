// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! IM 平台凭据脱敏与单行诊断规整：剥离 `reqwest::Error` 中的 URL，替换任意长度非空凭据并限制长度。

use lattice_server::ImToken;

/// 单条脱敏诊断的最大 Unicode 字符数。
pub(crate) const MAX_DIAGNOSTIC_CHARS: usize = 512;

/// 持有当前 IM 实例已知平台凭据的脱敏器。
#[derive(Clone)]
pub(crate) struct ImRedactor {
    tokens: Vec<ImToken>,
}

impl ImRedactor {
    /// 按凭据字节长度从长到短排序构造脱敏器，避免短子串抢先破坏长凭据替换。
    ///
    /// 不设最小长度门槛，任何非空凭据都会被替换为 `[REDACTED]`。
    pub(crate) fn new(tokens: impl IntoIterator<Item = ImToken>) -> Self {
        let mut unique: Vec<ImToken> = Vec::new();
        for token in tokens {
            if !token.as_str().is_empty() && !unique.contains(&token) {
                unique.push(token);
            }
        }
        unique.sort_by_key(|token| std::cmp::Reverse(token.as_str().len()));
        Self { tokens: unique }
    }

    /// 替换文本中出现的全部已知凭据，保留换行与其余文本形态。
    pub(crate) fn redact_str(&self, input: &str) -> String {
        let mut output = input.to_owned();
        for token in &self.tokens {
            let needle = token.as_str();
            if !needle.is_empty() && output.contains(needle) {
                output = output.replace(needle, "[REDACTED]");
            }
        }
        output
    }

    /// 替换全部已知凭据，并将换行、控制字符与连续空白折叠为单个空格（不做字符长度截断）。
    pub(crate) fn redact_single_line_unbounded(&self, raw: &str) -> String {
        let redacted = self.redact_str(raw);
        let mut normalized = String::with_capacity(redacted.len());
        let mut prev_space = false;
        for ch in redacted.chars() {
            if ch.is_control() || ch.is_whitespace() {
                if !prev_space && !normalized.is_empty() {
                    normalized.push(' ');
                    prev_space = true;
                }
            } else {
                normalized.push(ch);
                prev_space = false;
            }
        }
        if normalized.ends_with(' ') {
            normalized.pop();
        }
        normalized
    }

    /// 替换全部已知凭据，将换行、控制字符与空白折叠为单个空格，并按字符边界截断到上限以内。
    pub(crate) fn sanitize_single_line(&self, input: &str) -> String {
        let normalized = self.redact_single_line_unbounded(input);
        truncate_diagnostic_chars(&normalized, MAX_DIAGNOSTIC_CHARS)
    }

    /// 对 `reqwest::Error` 先剥离携带 `/bot<token>/<method>` 的请求 URL，再执行凭据替换与单行截断。
    pub(crate) fn redact_reqwest_error(&self, error: reqwest::Error) -> String {
        let stripped = error.without_url();
        self.sanitize_single_line(&stripped.to_string())
    }
}

impl std::fmt::Debug for ImRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImRedactor")
            .field("token_count", &self.tokens.len())
            .finish()
    }
}

/// 按 Unicode 字符边界将文本截断到 `max_chars` 以内，超长时保留 `max_chars - 1` 个字符并附 `…`。
pub(crate) fn truncate_diagnostic_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_owned();
    }
    let keep = max_chars - 1;
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_short_tokens_and_longer_overlapping_tokens_first() {
        let short = ImToken::parse("short").expect("短凭据应合法");
        let tiny = ImToken::parse("ab").expect("两字符凭据应合法");
        let longer = ImToken::parse("short-extended-secret").expect("长凭据应合法");
        let redactor = ImRedactor::new([short, tiny, longer]);

        let text = "url=/botshort-extended-secret/sendMessage fallback=short pair=ab";
        let cleaned = redactor.sanitize_single_line(text);
        assert_eq!(
            cleaned, "url=/bot[REDACTED]/sendMessage fallback=[REDACTED] pair=[REDACTED]",
            "短凭据与重叠长凭据均须完整替换为 [REDACTED]"
        );
        assert!(!cleaned.contains("short"));
        assert!(!cleaned.contains("ab"));
    }

    #[test]
    fn collapses_multiline_control_chars_and_bounds_length() {
        let secret = ImToken::parse("tg-secret-token-999").expect("测试凭据应合法");
        let redactor = ImRedactor::new([secret]);
        let long_tail = "甲".repeat(600);
        let raw = format!("第一行\r\n\t携带 tg-secret-token-999\n{long_tail}");

        let unbounded = redactor.redact_single_line_unbounded(&raw);
        assert!(
            !unbounded.contains('\n') && !unbounded.contains('\r') && !unbounded.contains('\t')
        );
        assert!(!unbounded.contains("tg-secret-token-999"));
        assert!(unbounded.contains("[REDACTED]"));
        assert!(
            unbounded.chars().count() > MAX_DIAGNOSTIC_CHARS,
            "无长度上限的单行脱敏不得截断末尾文本"
        );
        assert!(unbounded.ends_with(&long_tail));

        let cleaned = redactor.sanitize_single_line(&raw);
        assert!(!cleaned.contains('\n'), "单行诊断不得残留换行");
        assert!(!cleaned.contains('\r'), "单行诊断不得残留回车");
        assert!(!cleaned.contains('\t'), "单行诊断不得残留制表符");
        assert!(
            !cleaned.contains("tg-secret-token-999"),
            "单行诊断不得残留原始凭据"
        );
        assert!(cleaned.contains("[REDACTED]"));
        assert_eq!(
            cleaned.chars().count(),
            MAX_DIAGNOSTIC_CHARS,
            "超长诊断应按字符边界截断至 512 字符"
        );
        assert!(cleaned.ends_with('…'));
    }

    #[tokio::test]
    async fn strips_url_from_reqwest_error_and_redacts_token() {
        let fixture_marker = "123456:ABC-DEF-FIXTURE";
        let credential = ImToken::parse(fixture_marker).expect("测试凭据应合法");
        let redactor = ImRedactor::new([credential]);

        // 绑定后立即释放端口并以 https:// 制造连接拒绝错误，验证 URL 中的路径片段被剥离且不会明文发包。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("分配本地端口应成功");
        let addr = listener.local_addr().expect("读取本地端口应成功");
        drop(listener);

        let endpoint = format!("https://{addr}/bot{fixture_marker}/getUpdates");
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("构造客户端应成功");
        let err = client
            .get(&endpoint)
            .send()
            .await
            .expect_err("已关闭端口应返回连接错误");
        assert!(
            err.to_string().contains(fixture_marker),
            "原始 reqwest 错误包含请求地址中的路径片段"
        );

        let cleaned = redactor.redact_reqwest_error(err);
        assert!(
            !cleaned.contains(fixture_marker),
            "脱敏后的 reqwest 错误诊断不得包含凭据原文"
        );
        assert!(
            !cleaned.contains("/getUpdates"),
            "without_url 应剥离请求路径"
        );
    }
}
