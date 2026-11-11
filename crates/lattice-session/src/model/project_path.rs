// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 会话绑定的规范项目路径值类型，独立于底层存储引擎。

use std::{
    borrow::Cow,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 项目路径不符合领域约束的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProjectPathError {
    #[error("项目路径必须是以 '/' 开头的绝对路径")]
    NotAbsolute,
    #[error("项目路径不能包含 NUL 字节")]
    InteriorNul,
}

/// 规范项目路径的原始 Linux 路径字节（允许非 UTF-8，以 `/` 开头且不含 NUL）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Vec<u8>", into = "Vec<u8>")]
pub struct ProjectPath(Vec<u8>);

impl ProjectPath {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Result<Self, ProjectPathError> {
        let bytes = bytes.into();
        if !bytes.starts_with(b"/") {
            return Err(ProjectPathError::NotAbsolute);
        }
        if bytes.contains(&0) {
            return Err(ProjectPathError::InteriorNul);
        }
        Ok(Self(bytes))
    }

    pub fn from_path(path: &Path) -> Result<Self, ProjectPathError> {
        Self::from_bytes(path.as_os_str().as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(OsStr::from_bytes(&self.0))
    }

    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }
}

impl TryFrom<Vec<u8>> for ProjectPath {
    type Error = ProjectPathError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Self::from_bytes(bytes)
    }
}

impl From<ProjectPath> for Vec<u8> {
    fn from(path: ProjectPath) -> Self {
        path.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_utf8_bytes_round_trip_through_json_and_format_lossily() {
        let raw = b"/tmp/proj-\xff\xfe/work";
        let from_bytes = ProjectPath::from_bytes(raw.as_slice()).expect("合法的非 UTF-8 绝对路径");
        let from_path = ProjectPath::from_path(Path::new(OsStr::from_bytes(raw)))
            .expect("从 Path 构造合法路径");
        assert_eq!(
            from_bytes, from_path,
            "两种构造方式应产生相同的 ProjectPath"
        );
        assert_eq!(from_bytes.as_bytes(), raw, "原始字节应无损保留");
        assert_eq!(
            from_bytes.to_path_buf().as_os_str().as_bytes(),
            raw,
            "还原出的 PathBuf 应保留原始字节"
        );
        assert_eq!(
            from_bytes.to_string_lossy(),
            "/tmp/proj-\u{FFFD}\u{FFFD}/work",
            "非 UTF-8 字节应显示为替换字符"
        );

        let wire = serde_json::to_value(&from_bytes).expect("序列化为 JSON 整数数组");
        let expected_array: Vec<serde_json::Value> =
            raw.iter().copied().map(serde_json::Value::from).collect();
        assert_eq!(
            wire,
            serde_json::Value::Array(expected_array),
            "线上表示应为 0..=255 的 JSON 整数数组"
        );

        let decoded: ProjectPath = serde_json::from_value(wire).expect("从 JSON 整数数组解码");
        assert_eq!(decoded, from_bytes, "JSON 编解码后 ProjectPath 应无损相等");
        assert_eq!(decoded.as_bytes(), raw, "JSON 编解码后原始字节应不变");
    }

    #[test]
    fn rejects_relative_and_nul_inputs_on_construction_and_decode() {
        for (input, expected) in [
            (b"".as_slice(), ProjectPathError::NotAbsolute),
            (b"relative/path".as_slice(), ProjectPathError::NotAbsolute),
            (b"./repo".as_slice(), ProjectPathError::NotAbsolute),
            (b"/tmp/bad\0path".as_slice(), ProjectPathError::InteriorNul),
            (b"/\0".as_slice(), ProjectPathError::InteriorNul),
        ] {
            assert_eq!(
                ProjectPath::from_bytes(input),
                Err(expected),
                "from_bytes 应拒绝非法输入 {input:?}"
            );
            let path = Path::new(OsStr::from_bytes(input));
            assert_eq!(
                ProjectPath::from_path(path),
                Err(expected),
                "from_path 应拒绝非法输入 {input:?}"
            );
            let wire = serde_json::to_value(input).expect("构造测试输入字节数组");
            assert!(
                serde_json::from_value::<ProjectPath>(wire).is_err(),
                "JSON 解码应拒绝非法输入 {input:?}"
            );
        }
    }
}
