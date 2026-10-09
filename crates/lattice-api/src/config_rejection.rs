// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::fmt;

/// 已脱敏并规范化的配置诊断，不提供绕过处理的字符串转换或反序列化入口。
#[derive(Clone, Debug)]
pub struct ConfigRejection(String);

impl ConfigRejection {
    pub fn new(message: &str, known_secrets: &[&str]) -> Self {
        Self(crate::redact::redact(message, known_secrets))
    }
}

impl fmt::Display for ConfigRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 仅用于提交端配置加载；后台不能用自己的环境代替本 Turn 的凭据。
pub(crate) fn client_configuration_error(message: &str) -> crate::ApiError {
    let secrets: Vec<String> = lattice_llm::credential_env_vars()
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect();
    let known: Vec<&str> = secrets.iter().map(String::as_str).collect();
    crate::ApiError::Configuration(ConfigRejection::new(message, &known))
}

#[cfg(test)]
#[path = "config_rejection_tests.rs"]
mod tests;
