// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 模型请求传输层的纯函数部分：请求标识、provider 错误诊断与重试判断、Retry-After。

use std::time::{Duration, SystemTime};

use reqwest::header::HeaderMap;

use crate::stub::LlmError;

/// 生成诊断的字符上限。146 条真实 provider 错误消息加上前缀后，整行 P90 为 252 字符（#265）。
pub const MAX_DIAGNOSTIC_CHARS: usize = 256;

/// 压成一行（见 [`one_line`]）后截到 [`MAX_DIAGNOSTIC_CHARS`]；被截断时以 `…` 结尾。
pub fn bound_diagnostic(text: &str) -> String {
    truncate_chars(&one_line(text), MAX_DIAGNOSTIC_CHARS)
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// provider 建议的重试等待。`retry-after-ms` 优先于 `retry-after`（秒数或 HTTP 日期）。
pub(crate) fn retry_after(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    if let Some(ms) = header("retry-after-ms").and_then(|value| seconds(value, 1e-3)) {
        return Some(ms);
    }
    let value = header("retry-after")?;
    seconds(value, 1.0).or_else(|| {
        let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
        let at = SystemTime::UNIX_EPOCH
            + Duration::from_millis(u64::try_from(at.timestamp_millis()).ok()?);
        Some(at.duration_since(now).unwrap_or(Duration::ZERO))
    })
}

/// 非负的有限数值乘以单位秒数；超出 `Duration` 表示范围时饱和，表示“等待极久”。
fn seconds(value: &str, unit: f64) -> Option<Duration> {
    let number: f64 = value.trim().parse().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    Some(Duration::try_from_secs_f64(number * unit).unwrap_or(Duration::MAX))
}

/// 非 2xx 响应的类型化错误：一行诊断、是否可重试、建议等待。
pub(crate) fn status_error(status: u16, headers: &HeaderMap, body: &str) -> LlmError {
    let (label, message) = error_body(body);
    let mut head = format!("HTTP {status}");
    if let Some(label) = &label {
        head.push(' ');
        head.push_str(label);
    }
    if !message.is_empty() {
        head.push_str(": ");
        head.push_str(&message);
    }
    let retry_after = retry_after(headers, SystemTime::now());
    let suffix = retry_after.map(|wait| format!(" (retry after {})", wait_text(wait)));
    let suffix = suffix.unwrap_or_default();
    let mut line = truncate_chars(&head, MAX_DIAGNOSTIC_CHARS - suffix.chars().count());
    line.push_str(&suffix);
    let should_retry = headers
        .get("x-should-retry")
        .and_then(|value| value.to_str().ok());
    let retryable = if label.as_deref() == Some("insufficient_quota") {
        false
    } else {
        match should_retry {
            Some("true") => true,
            Some("false") => false,
            _ => matches!(status, 408 | 409 | 429) || status >= 500,
        }
    };
    LlmError::ProviderHttp {
        status_code: status,
        message: line,
        retryable,
        retry_after,
    }
}

fn wait_text(wait: Duration) -> String {
    if wait.subsec_millis() == 0 {
        format!("{}s", wait.as_secs())
    } else {
        format!("{}ms", wait.as_millis())
    }
}

/// 三家 provider 的错误信封：OpenAI `{"error":{"code","type","message"}}`、
/// Anthropic `{"type":"error","error":{"type","message"}}`、Gemini `[{"error":{"status","message"}}]`。
/// 取类型或代码与消息；不是这些形状时取正文开头。两者都来自 provider，统一压成一行，见 [`one_line`]。
fn error_body(body: &str) -> (Option<String>, String) {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let error = parsed.as_ref().and_then(|value| {
        let root = value
            .as_array()
            .and_then(|items| items.first())
            .unwrap_or(value);
        root.get("error")
    });
    let (label, message) = match error {
        Some(serde_json::Value::String(message)) => (None, message.as_str()),
        Some(error) if error.is_object() => {
            let text = |key: &str| error.get(key).and_then(|value| value.as_str());
            let label = text("code")
                .or_else(|| text("status"))
                .or_else(|| text("type"));
            (label, text("message").unwrap_or_default())
        }
        _ => (None, body),
    };
    (label.map(one_line), one_line(message))
}

/// 诊断要进日志、notice 和终端：控制字符（含 ESC、NUL）换成空格，空白折成单个空格。
/// 先折叠再由调用方截断：先截断会让连串空白吃掉后面的消息，也会漏掉 `…`。正文最多 64 KiB。
fn one_line(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `lattice/<版本> (linux <内核版本>; <架构>)`；内核版本只保留头值安全的字符，空时省略。
pub(crate) fn user_agent(kernel_release: Option<&str>) -> String {
    let release: String = kernel_release
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | '~'))
        .collect();
    let os = std::env::consts::OS;
    let platform = if release.is_empty() {
        os.to_owned()
    } else {
        format!("{os} {release}")
    };
    format!(
        "lattice/{} ({platform}; {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH
    )
}

/// provider 返回的请求 ID，只接受短的可打印标识，避免把任意响应头写进日志。
pub(crate) fn provider_request_id(headers: &HeaderMap) -> Option<String> {
    ["request-id", "x-request-id"]
        .iter()
        .find_map(|name| headers.get(*name)?.to_str().ok())
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn retry_after_formats() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_444_000_000);
        // now 之后与之前各 30 秒。
        let future = "Sun, 04 Oct 2015 23:07:10 GMT";
        let past = "Sun, 04 Oct 2015 23:06:10 GMT";
        for (name, pairs, expected) in [
            (
                "seconds",
                vec![("retry-after", "2")],
                Some(Duration::from_secs(2)),
            ),
            (
                "fractional seconds",
                vec![("retry-after", "1.5")],
                Some(Duration::from_millis(1500)),
            ),
            (
                "milliseconds win",
                vec![("retry-after", "9"), ("retry-after-ms", "250")],
                Some(Duration::from_millis(250)),
            ),
            (
                "http date ahead",
                vec![("retry-after", future)],
                Some(Duration::from_secs(30)),
            ),
            (
                "http date passed",
                vec![("retry-after", past)],
                Some(Duration::ZERO),
            ),
            ("garbage", vec![("retry-after", "soon")], None),
            ("negative", vec![("retry-after", "-1")], None),
            ("infinite", vec![("retry-after", "inf")], None),
            (
                "overflow saturates",
                vec![("retry-after", "99999999999999999999999")],
                Some(Duration::MAX),
            ),
            ("absent", vec![], None),
        ] {
            assert_eq!(retry_after(&headers(&pairs), now), expected, "{name}");
        }
    }

    #[test]
    fn status_errors_are_parsed_and_classified() {
        let html = format!(
            "<html>\r\n<head><title>502 Bad Gateway</title></head>\r\n<body>{}</body></html>",
            "x".repeat(5_000)
        );
        let long = format!(
            r#"{{"error":{{"message":"{}","code":"rate_limit_exceeded"}}}}"#,
            "y".repeat(400)
        );
        // 折叠之后才截断：连串空白不能吃掉后面的内容，超长时仍以 `…` 结尾。
        let spaced = format!("{}quota", "x   ".repeat(128));
        let cut = bound_diagnostic(&spaced);
        assert_eq!(cut.chars().count(), 256);
        assert!(cut.starts_with("x x x") && cut.ends_with('…'), "{cut}");
        let padded = format!(
            "{{\"error\":{{\"message\":\"{}quota exhausted\"}}}}",
            " ".repeat(600)
        );
        assert_eq!(
            status_error(400, &HeaderMap::new(), &padded).to_string(),
            "HTTP 400: quota exhausted"
        );
        for (name, status, pairs, body, line, retryable, wait) in [
            (
                "openai rate limit with retry-after",
                429,
                vec![("retry-after", "2")],
                r#"{"error":{"message":"Rate limit reached for requests","type":"requests","code":"rate_limit_exceeded"}}"#.to_string(),
                "HTTP 429 rate_limit_exceeded: Rate limit reached for requests (retry after 2s)".to_string(),
                true,
                Some(Duration::from_secs(2)),
            ),
            (
                "openai quota is final",
                429,
                vec![],
                r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#.into(),
                "HTTP 429 insufficient_quota: You exceeded your current quota".into(),
                false,
                None,
            ),
            (
                "anthropic overload keeps request id out",
                529,
                vec![],
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_011CTest"}"#.into(),
                "HTTP 529 overloaded_error: Overloaded".into(),
                true,
                None,
            ),
            (
                "gemini array envelope uses status",
                503,
                vec![],
                r#"[{"error":{"code":503,"message":"The model is overloaded.","status":"UNAVAILABLE"}}]"#.into(),
                "HTTP 503 UNAVAILABLE: The model is overloaded.".into(),
                true,
                None,
            ),
            (
                "null code falls back to type",
                400,
                vec![],
                r#"{"error":{"message":"bad tools","type":"invalid_request_error","code":null}}"#.into(),
                "HTTP 400 invalid_request_error: bad tools".into(),
                false,
                None,
            ),
            ("request timeout", 408, vec![], String::new(), "HTTP 408".into(), true, None),
            ("conflict", 409, vec![], String::new(), "HTTP 409".into(), true, None),
            ("not found", 404, vec![], String::new(), "HTTP 404".into(), false, None),
            (
                "should-retry false wins over 5xx",
                500,
                vec![("x-should-retry", "false")],
                String::new(),
                "HTTP 500".into(),
                false,
                None,
            ),
            (
                "should-retry true wins over 4xx",
                400,
                vec![("x-should-retry", "true")],
                String::new(),
                "HTTP 400".into(),
                true,
                None,
            ),
            (
                "string error",
                500,
                vec![],
                r#"{"error":"upstream exploded"}"#.into(),
                "HTTP 500: upstream exploded".into(),
                true,
                None,
            ),
            (
                "whitespace collapses",
                400,
                vec![],
                "{\"error\":{\"message\":\"line one\\n\\n  line two\",\"type\":\"invalid_request_error\"}}".into(),
                "HTTP 400 invalid_request_error: line one line two".into(),
                false,
                None,
            ),
            (
                "control characters leave label and message",
                400,
                vec![],
                "{\"error\":{\"message\":\"bad\\u001b[31m red\\u0000 end\",\"code\":\"bad\\ncode\"}}".into(),
                "HTTP 400 bad code: bad [31m red end".into(),
                false,
                None,
            ),
            (
                "control characters leave non-JSON excerpts",
                502,
                vec![],
                "gate\u{7}way\u{1b}]0;title\u{7} down".into(),
                "HTTP 502: gate way ]0;title down".into(),
                true,
                None,
            ),
        ] {
            let error = status_error(status, &headers(&pairs), &body);
            assert_eq!(error.to_string(), line, "{name}");
            assert_eq!(error.retryable(), retryable, "{name}");
            assert_eq!(error.retry_after(), wait, "{name}");
            assert_eq!(error.status_code(), Some(status), "{name}");
        }

        let gateway = status_error(502, &HeaderMap::new(), &html).to_string();
        assert!(
            gateway.starts_with("HTTP 502: <html> <head><title>502 Bad Gateway</title></head>")
        );
        assert_eq!(gateway.chars().count(), MAX_DIAGNOSTIC_CHARS);
        assert!(gateway.ends_with('…'));

        let bounded = status_error(429, &headers(&[("retry-after", "7")]), &long).to_string();
        assert_eq!(bounded.chars().count(), MAX_DIAGNOSTIC_CHARS);
        assert!(bounded.ends_with("… (retry after 7s)"), "{bounded}");
    }

    #[test]
    fn diagnostics_are_bounded_by_characters() {
        assert_eq!(bound_diagnostic(&"界".repeat(256)), "界".repeat(256));
        let cut = bound_diagnostic(&"界".repeat(300));
        assert_eq!(cut.chars().count(), 256);
        assert!(cut.ends_with('…'));
        // 流内错误事件同样来自 provider，也要压成一行。
        assert_eq!(
            bound_diagnostic("over\nloaded\u{1b}[0m\0 now"),
            "over loaded [0m now"
        );
    }

    #[test]
    fn user_agent_names_lattice_and_stays_a_valid_header() {
        let version = env!("CARGO_PKG_VERSION");
        let arch = std::env::consts::ARCH;
        for (release, expected) in [
            (
                Some("7.2.0-arch1-1"),
                format!("lattice/{version} (linux 7.2.0-arch1-1; {arch})"),
            ),
            (
                Some("6.1 (x);y\n"),
                format!("lattice/{version} (linux 6.1xy; {arch})"),
            ),
            (Some(""), format!("lattice/{version} (linux; {arch})")),
            (None, format!("lattice/{version} (linux; {arch})")),
        ] {
            let agent = user_agent(release);
            assert_eq!(agent, expected);
            assert!(reqwest::header::HeaderValue::from_str(&agent).is_ok());
        }
    }

    #[test]
    fn provider_request_ids_are_short_printable_tokens() {
        for (pairs, expected) in [
            (vec![("request-id", "req_011CTest")], Some("req_011CTest")),
            (
                vec![("x-request-id", "abc-123:def.9")],
                Some("abc-123:def.9"),
            ),
            (vec![("x-request-id", "has space")], None),
            (vec![("x-request-id", &*"a".repeat(129))], None),
            (vec![], None),
        ] {
            assert_eq!(provider_request_id(&headers(&pairs)).as_deref(), expected);
        }
    }
}
