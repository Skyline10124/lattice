// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::LazyLock;

use regex::{Captures, Regex};

const REDACTED: &str = "[REDACTED]";
const MAX_REASON_CHARS: usize = 1024;

static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([a-zA-Z][a-zA-Z0-9+.-]*)://[^:\s/?#]+:[^@\s]+@")
        .expect("valid URL userinfo regex")
});
static AUTH_SCHEME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(Bearer|Basic|Token)\s+[A-Za-z0-9_\-.+/=]+")
        .expect("valid authentication regex")
});
static QUERY_CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([?&](?:api[_-]?key|access[_-]?token|refresh[_-]?token|token|password|passwd|secret|signature|sig|auth|x[_-]?api[_-]?key))=[^&\s#]+")
        .expect("valid credential query regex")
});
static PREFIXED_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sk-ant-[A-Za-z0-9_\-]+|sk-[A-Za-z0-9_\-]+|(?:ghp|gho|ghs|ghu)_[A-Za-z0-9]+|glpat-[A-Za-z0-9_\-]+|AIza[A-Za-z0-9_\-]+|(?:AKIA|ASIA)[A-Z0-9]+|xox[baprs]-[A-Za-z0-9\-]+)")
        .expect("valid key prefix regex")
});
static OPAQUE_VALUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_-]{40,}").expect("valid opaque value regex"));

/// 按顺序替换已知值、凭据位置及密钥形状，再规范化为有界单行诊断。
pub(crate) fn redact(message: &str, known_secrets: &[&str]) -> String {
    let mut text = message.to_owned();
    for &secret in known_secrets {
        if secret.chars().count() >= 8 {
            text = text.replace(secret, REDACTED);
        }
    }
    text = URL_USERINFO
        .replace_all(&text, "${1}://[REDACTED]@")
        .into_owned();
    text = AUTH_SCHEME
        .replace_all(&text, "${1} [REDACTED]")
        .into_owned();
    text = QUERY_CREDENTIAL
        .replace_all(&text, "${1}=[REDACTED]")
        .into_owned();
    text = PREFIXED_KEY
        .replace_all(&text, |caps: &Captures<'_>| {
            if caps[0].len() >= 20 {
                REDACTED.to_owned()
            } else {
                caps[0].to_owned()
            }
        })
        .into_owned();
    // 相对 rtb-redact 的不透明串规则，去掉 /、+、=，避免整段遮住配置路径。
    // 必须同时含字母和数字且不限空白边界：覆盖引号内的值，保留长纯字母标识符及不足 40 字符的 ID。
    text = OPAQUE_VALUE
        .replace_all(&text, |caps: &Captures<'_>| {
            let value = &caps[0];
            if value.bytes().any(|b| b.is_ascii_alphabetic())
                && value.bytes().any(|b| b.is_ascii_digit())
            {
                REDACTED.to_owned()
            } else {
                value.to_owned()
            }
        })
        .into_owned();

    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= MAX_REASON_CHARS {
        line
    } else {
        line.chars()
            .take(MAX_REASON_CHARS - 1)
            .chain(['…'])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_values_and_credential_shapes() {
        for (input, known, expected) in [
            (
                "key=ordinary-secret-value",
                vec!["ordinary-secret-value"],
                "key=[REDACTED]",
            ),
            (
                "12345678 twice 12345678",
                vec!["12345678"],
                "[REDACTED] twice [REDACTED]",
            ),
            (
                "https://user:pass@example.test/v1",
                vec![],
                "https://[REDACTED]@example.test/v1",
            ),
            ("bEaReR abc.DEF_123-+/=", vec![], "bEaReR [REDACTED]"),
            ("Basic dXNlcjpwYXNz", vec![], "Basic [REDACTED]"),
            ("Token short", vec![], "Token [REDACTED]"),
            (
                "https://example.test/?api_key=abc&x-api-key=def#fragment",
                vec![],
                "https://example.test/?api_key=[REDACTED]&x-api-key=[REDACTED]#fragment",
            ),
            ("'sk-12345678901234567'", vec![], "'[REDACTED]'"),
            (
                "'a123456789012345678901234567890123456789'",
                vec![],
                "'[REDACTED]'",
            ),
            (
                "password=普通秘密值八个汉字",
                vec!["普通秘密值八个汉字"],
                "password=[REDACTED]",
            ),
        ] {
            assert_eq!(redact(input, &known), expected);
        }
        for prefix in [
            "sk-ant-", "sk-", "ghp_", "gho_", "ghs_", "ghu_", "glpat-", "AIza", "AKIA", "ASIA",
            "xoxb-", "xoxa-", "xoxp-", "xoxr-", "xoxs-",
        ] {
            let key = format!("{prefix}{}", "A1".repeat(10));
            assert_eq!(redact(&format!("'{key}'"), &[]), "'[REDACTED]'", "{prefix}");
        }
        for name in [
            "apiKey",
            "api-key",
            "ACCESS_TOKEN",
            "refresh-token",
            "token",
            "password",
            "passwd",
            "secret",
            "signature",
            "sig",
            "auth",
            "x_api_key",
        ] {
            assert_eq!(
                redact(&format!("?{name}=value&other=visible"), &[]),
                format!("?{name}=[REDACTED]&other=visible")
            );
        }
    }

    #[test]
    fn preserves_paths_ids_and_short_or_plain_values() {
        for input in [
            "/home/user123/projects/configuration/really-long-directory-name/agent.toml",
            "0123456789abcdef0123456789abcdef01",
            "12345678-1234-4234-8234-123456789012",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "123456789012345678901234567890123456789012345",
            "a12345678901234567890123456789012345678",
            "sk-1234567890123456",
            "sonet",
            "tiny 1234567",
        ] {
            assert_eq!(redact(input, &["", "tiny", "1234567"]), input);
        }
    }

    #[test]
    fn redacts_before_single_line_normalization_and_character_limit() {
        assert_eq!(
            redact("before\r\n\tBearer\nabc123\u{1b}\0  after", &[]),
            "before Bearer [REDACTED] after"
        );
        assert_eq!(
            redact("prefix\nsecret value\nend", &["secret value"]),
            "prefix [REDACTED] end"
        );
        assert_eq!(redact(&"界".repeat(1024), &[]), "界".repeat(1024));
        let text = redact(&format!("sk-12345678901234567 {}", "界".repeat(1100)), &[]);
        assert_eq!(text.chars().count(), MAX_REASON_CHARS);
        assert!(text.starts_with("[REDACTED] "));
        assert!(text.ends_with('…'));
    }
}
