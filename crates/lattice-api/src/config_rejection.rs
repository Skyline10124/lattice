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

#[cfg(test)]
#[path = "config_rejection_tests.rs"]
mod tests;
