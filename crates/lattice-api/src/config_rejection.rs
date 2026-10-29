// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::fmt;

/// 已脱敏并规范化的配置诊断，不提供绕过处理的字符串转换或反序列化入口。
#[derive(Clone, Debug)]
pub struct ConfigRejection(String);

impl ConfigRejection {
    pub fn new(message: &str, auth: Option<&lattice_llm::auth::AuthStore>) -> Self {
        let reason = auth
            .and_then(|auth| {
                auth.with_credential_values(|known| crate::redact::redact(message, known))
                    .ok()
            })
            .unwrap_or_else(|| crate::redact::redact(message, &[]));
        Self(reason)
    }
}

impl fmt::Display for ConfigRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 前端尚未交后台脱敏的诊断；Display 与 Debug 不展示其原文。
#[derive(Clone)]
pub struct ConfigurationDiagnostic(String);

impl ConfigurationDiagnostic {
    pub(crate) fn new(message: String) -> Self {
        Self(message)
    }

    pub(crate) fn into_message(self) -> String {
        self.0
    }
}

impl fmt::Display for ConfigurationDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("configuration diagnostic requires backend redaction")
    }
}

impl fmt::Debug for ConfigurationDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
#[path = "config_rejection_tests.rs"]
mod tests;
