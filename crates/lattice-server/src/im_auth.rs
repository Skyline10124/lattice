// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! IM 平台凭据文件存储：经 `lattice-store` 私有目录原语严格核验并原子替换 `<config_dir>/im/auth.json`。

use std::{collections::BTreeMap, ffi::OsStr, fmt, path::PathBuf, sync::Mutex};

use lattice_store::{PrivateDirectory, PrivateFileError, PrivateFileOperation};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// 当前唯一支持的 IM 平台标识。
pub const TELEGRAM_PLATFORM: &str = "telegram";

const IM_SUBDIR_NAME: &str = "im";
const AUTH_FILE_NAME: &str = "auth.json";
const MAX_TOKEN_BYTES: usize = 16_384;
const MAX_FILE_BYTES: usize = 1_048_576;

// 进程级写锁串行化“重读 -> 修改 -> 原子替换”，防止同进程多个句柄并发覆盖彼此更新。
static WRITER: Mutex<()> = Mutex::new(());

/// 经过长度与控制字符校验的 IM 平台凭据值。
///
/// 为防止意外格式化泄露秘密，本类型不实现 `Debug` 与 `Display`，并在释放时清零底层缓冲。
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct ImToken(String);

impl ImToken {
    /// 校验并构造平台凭据（仅向测试与 `test-utils` 导出）。
    #[cfg(any(test, feature = "test-utils"))]
    pub fn parse(raw: &str) -> Result<Self, ImAuthError> {
        validate_token(raw)?;
        Ok(Self(raw.to_owned()))
    }

    /// 返回凭据的只读字符串切片。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 单个 IM 平台在本地凭据文件中的静态状态（不发起联网验证）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImPlatformCredentialState {
    /// 凭据文件合法且存在该平台的有效凭据。
    Present,
    /// 配置目录、`im/`、`auth.json` 或该平台条目不存在。
    Missing,
    /// 目录或文件权限、属主、类型或 JSON 结构不合规。
    Invalid {
        /// 脱敏后的中文原因说明。
        reason: String,
    },
}

/// IM 凭据读写与校验错误。
///
/// 所有变体均不包含平台参数原文、`serde_json` 源码片段或凭据原文。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImAuthError {
    /// 凭据文件或其父目录的权限、类型、大小或 JSON 内容不合规。
    #[error("im/auth.json：{0}")]
    InvalidFile(&'static str),
    /// 传入的平台标识不受支持（不回显调用方传入的字符串，防止误传密钥导致泄露）。
    #[error("不支持的 IM 平台；当前仅支持 telegram")]
    UnsupportedPlatform,
    /// 传入的凭据为空、超长或含控制字符。
    #[error("IM 平台凭据长度须为 1–16384 字节且不得包含控制字符")]
    InvalidToken,
    /// 内部不变式错误。
    #[error("IM 凭据内部错误：{0}")]
    Internal(&'static str),
    /// 底层系统 I/O 失败。
    #[error("im/auth.json：{operation}失败（{}）", io_kind_label(*.kind))]
    Io {
        /// 执行的底层操作描述。
        operation: &'static str,
        /// 标准库错误分类。
        kind: std::io::ErrorKind,
    },
}

fn io_kind_label(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::PermissionDenied => "权限不足",
        std::io::ErrorKind::NotFound => "路径不存在",
        std::io::ErrorKind::AlreadyExists => "目标已存在",
        std::io::ErrorKind::StorageFull => "存储空间不足",
        std::io::ErrorKind::Interrupted => "操作被中断",
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => "输入数据无效",
        _ => "系统 I/O 错误",
    }
}

#[derive(Clone, Copy)]
enum PathRole {
    ConfigDir,
    ImDir,
    AuthFile,
}

fn map_private_file_error(role: PathRole, error: PrivateFileError) -> ImAuthError {
    match error {
        PrivateFileError::InvalidName => ImAuthError::Internal("凭据目录或文件名包含非法路径分量"),
        PrivateFileError::WrongOwner => {
            ImAuthError::InvalidFile("凭据文件及所属配置目录必须归当前用户所有")
        }
        PrivateFileError::NotADirectory | PrivateFileError::DirectorySymlink => match role {
            PathRole::ConfigDir => {
                ImAuthError::InvalidFile("配置目录必须是真实目录且不得为符号链接")
            }
            PathRole::ImDir | PathRole::AuthFile => {
                ImAuthError::InvalidFile("im 配置子目录必须是真实目录且不得为符号链接")
            }
        },
        PrivateFileError::BroadDirectoryPermissions => match role {
            PathRole::ConfigDir => ImAuthError::InvalidFile(
                "配置目录权限过宽，不得授予同组或其他用户任何权限（未自动修改权限）",
            ),
            PathRole::ImDir | PathRole::AuthFile => ImAuthError::InvalidFile(
                "im 配置子目录权限过宽，不得授予同组或其他用户任何权限（未自动修改权限）",
            ),
        },
        PrivateFileError::DirectoryDisappeared => {
            ImAuthError::Internal("凭据所属配置目录在操作期间消失")
        }
        PrivateFileError::NotARegularFile => ImAuthError::InvalidFile("凭据文件必须是普通文件"),
        PrivateFileError::FileSymlink => ImAuthError::InvalidFile("凭据文件不得为符号链接"),
        PrivateFileError::BroadFilePermissions => ImAuthError::InvalidFile(
            "凭据文件权限过宽，不得授予同组或其他用户任何权限（未自动修改权限）",
        ),
        PrivateFileError::FileTooLarge { .. } => {
            ImAuthError::InvalidFile("凭据文件大小超过 1 MiB 上限")
        }
        PrivateFileError::DirectorySyncAfterReplace { source } => ImAuthError::Io {
            operation: "替换凭据文件后同步目录",
            kind: source.kind(),
        },
        PrivateFileError::Io { operation, source } => {
            let operation = match operation {
                PrivateFileOperation::CreateDirectory => "创建私有配置目录",
                PrivateFileOperation::OpenDirectory | PrivateFileOperation::OpenFile => {
                    "打开凭据目录或文件"
                }
                PrivateFileOperation::InspectDirectory | PrivateFileOperation::InspectFile => {
                    "检查凭据目录或文件元数据"
                }
                PrivateFileOperation::ReadFile => "读取凭据文件",
                PrivateFileOperation::CreateTemporaryFile => "创建临时凭据文件",
                PrivateFileOperation::InspectTemporaryFile => "检查临时凭据文件元数据",
                PrivateFileOperation::WriteTemporaryFile => "写入临时凭据文件",
                PrivateFileOperation::SyncTemporaryFile => "刷盘临时凭据文件",
                PrivateFileOperation::ReplaceFile => "原子替换凭据文件",
            };
            ImAuthError::Io {
                operation,
                kind: source.kind(),
            }
        }
    }
}

fn validate_platform(platform: &str) -> Result<&'static str, ImAuthError> {
    match platform {
        TELEGRAM_PLATFORM => Ok(TELEGRAM_PLATFORM),
        _ => Err(ImAuthError::UnsupportedPlatform),
    }
}

fn validate_token(token: &str) -> Result<(), ImAuthError> {
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES || token.chars().any(char::is_control) {
        return Err(ImAuthError::InvalidToken);
    }
    Ok(())
}

/// 仅供内部 JSON 编解码的秘密包装，释放时自动清零。
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop, PartialEq, Eq)]
#[serde(transparent)]
struct Secret(String);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u64,
    #[serde(deserialize_with = "deserialize_platforms")]
    platforms: BTreeMap<String, Secret>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            platforms: BTreeMap::new(),
        }
    }
}

fn deserialize_object<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    struct ObjectVisitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
        type Value = T;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("JSON 对象")
        }

        fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<T, M::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }
    deserializer.deserialize_map(ObjectVisitor(std::marker::PhantomData))
}

fn deserialize_platforms<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Secret>, D::Error> {
    struct PlatformsVisitor;
    impl<'de> Visitor<'de> for PlatformsVisitor {
        type Value = BTreeMap<String, Secret>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("不重复的平台凭据映射")
        }

        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut entries = BTreeMap::new();
            while let Some((raw_key, secret)) = map.next_entry::<String, Secret>()? {
                let mut key = Zeroizing::new(raw_key);
                if entries.contains_key(key.as_str()) {
                    return Err(serde::de::Error::custom("重复平台键"));
                }
                let owned_key = std::mem::take(&mut *key);
                entries.insert(owned_key, secret);
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_map(PlatformsVisitor)
}

fn parse_document(bytes: &[u8]) -> Result<Document, ImAuthError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    // 丢弃 serde_json 原始错误详情，防止解析异常回显未知键名或文件片段。
    let mut document: Document = deserialize_object(&mut deserializer).map_err(|_| {
        ImAuthError::InvalidFile("凭据文件 JSON 结构无效、包含未知字段或存在重复平台键")
    })?;
    deserializer.end().map_err(|_| {
        clear_document(&mut document);
        ImAuthError::InvalidFile("凭据文件包含多余尾随内容")
    })?;
    if document.version != 1 {
        clear_document(&mut document);
        return Err(ImAuthError::InvalidFile(
            "凭据文件版本不受支持（仅支持 version = 1）",
        ));
    }
    for (platform, secret) in &document.platforms {
        if platform != TELEGRAM_PLATFORM {
            clear_document(&mut document);
            return Err(ImAuthError::InvalidFile("凭据文件包含不受支持的平台键"));
        }
        if validate_token(&secret.0).is_err() {
            clear_document(&mut document);
            return Err(ImAuthError::InvalidFile("凭据文件中的密钥格式无效"));
        }
    }
    Ok(document)
}

fn clear_document(document: &mut Document) {
    let mut keys: Vec<String> = Vec::with_capacity(document.platforms.len());
    while let Some((mut key, secret)) = document.platforms.pop_first() {
        key.zeroize();
        drop(secret);
        keys.push(key);
    }
    drop(keys);
}

fn encode_document(document: &Document) -> Result<Zeroizing<Vec<u8>>, ImAuthError> {
    let mut bytes = Zeroizing::new(Vec::new());
    serde_json::to_writer(&mut *bytes, document)
        .map_err(|_| ImAuthError::Internal("序列化 IM 凭据文件失败"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(ImAuthError::InvalidFile("凭据文件大小超过 1 MiB 上限"));
    }
    Ok(bytes)
}

/// IM 平台凭据存储句柄：只持有配置目录路径，每次操作按需读取磁盘，不在内存中缓存秘密。
#[derive(Clone, Debug)]
pub struct ImAuthStore {
    config_dir: PathBuf,
}

impl ImAuthStore {
    /// 以配置目录（通常为 `$HOME/.lattice`）构造凭据存储句柄；不在构造时访问磁盘。
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }

    /// 读取指定平台的凭据。
    ///
    /// 配置目录、`im/` 或 `auth.json` 不存在时均返回 `Ok(None)` 且不创建目录或文件；
    /// 已存在路径的权限、属主、符号链接或文件内容异常时返回类型化错误。
    pub fn get(&self, platform: &str) -> Result<Option<ImToken>, ImAuthError> {
        let platform = validate_platform(platform)?;
        let Some(mut document) = self.read_existing_document()? else {
            return Ok(None);
        };
        let token = document
            .platforms
            .get(platform)
            .map(|secret| ImToken(secret.0.clone()));
        clear_document(&mut document);
        Ok(token)
    }

    /// 查询指定平台的本地凭据状态（不联网验证）。
    pub fn status(&self, platform: &str) -> Result<ImPlatformCredentialState, ImAuthError> {
        validate_platform(platform)?;
        match self.get(platform) {
            Ok(Some(_token)) => Ok(ImPlatformCredentialState::Present),
            Ok(None) => Ok(ImPlatformCredentialState::Missing),
            Err(ImAuthError::InvalidFile(reason)) => Ok(ImPlatformCredentialState::Invalid {
                reason: reason.to_owned(),
            }),
            Err(other) => Err(other),
        }
    }

    /// 写入或覆盖指定平台的凭据；缺失配置目录或 `im/` 时以 `0700` 创建，并以 `0600` 原子替换 `auth.json`。
    pub fn set(&self, platform: &str, token: &str) -> Result<(), ImAuthError> {
        let platform = validate_platform(platform)?;
        validate_token(token)?;
        let _guard = WRITER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let config_dir = PrivateDirectory::open_or_create(&self.config_dir)
            .map_err(|err| map_private_file_error(PathRole::ConfigDir, err))?;
        let im_dir = config_dir
            .child(OsStr::new(IM_SUBDIR_NAME), true)
            .map_err(|err| map_private_file_error(PathRole::ImDir, err))?
            .ok_or(ImAuthError::Internal("创建 im 子目录后未返回目录句柄"))?;
        let mut document = match im_dir
            .read(OsStr::new(AUTH_FILE_NAME), Some(MAX_FILE_BYTES))
            .map_err(|err| map_private_file_error(PathRole::AuthFile, err))?
        {
            Some(bytes) => parse_document(&bytes)?,
            None => Document::default(),
        };
        document
            .platforms
            .insert(platform.to_owned(), Secret(token.to_owned()));
        let encoded = encode_document(&document);
        clear_document(&mut document);
        let bytes = encoded?;
        im_dir
            .replace(OsStr::new(AUTH_FILE_NAME), &bytes)
            .map_err(|err| map_private_file_error(PathRole::AuthFile, err))
    }

    /// 删除指定平台的凭据；目录、文件或该平台条目本就不存在时返回 `Ok(false)` 且不创建任何文件。
    pub fn remove(&self, platform: &str) -> Result<bool, ImAuthError> {
        let platform = validate_platform(platform)?;
        let _guard = WRITER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(config_dir) = PrivateDirectory::open(&self.config_dir)
            .map_err(|err| map_private_file_error(PathRole::ConfigDir, err))?
        else {
            return Ok(false);
        };
        let Some(im_dir) = config_dir
            .child(OsStr::new(IM_SUBDIR_NAME), false)
            .map_err(|err| map_private_file_error(PathRole::ImDir, err))?
        else {
            return Ok(false);
        };
        let Some(bytes) = im_dir
            .read(OsStr::new(AUTH_FILE_NAME), Some(MAX_FILE_BYTES))
            .map_err(|err| map_private_file_error(PathRole::AuthFile, err))?
        else {
            return Ok(false);
        };
        let mut document = parse_document(&bytes)?;
        if document.platforms.remove(platform).is_none() {
            clear_document(&mut document);
            return Ok(false);
        }
        let encoded = encode_document(&document);
        clear_document(&mut document);
        let out = encoded?;
        im_dir
            .replace(OsStr::new(AUTH_FILE_NAME), &out)
            .map_err(|err| map_private_file_error(PathRole::AuthFile, err))?;
        Ok(true)
    }

    fn read_existing_document(&self) -> Result<Option<Document>, ImAuthError> {
        let Some(config_dir) = PrivateDirectory::open(&self.config_dir)
            .map_err(|err| map_private_file_error(PathRole::ConfigDir, err))?
        else {
            return Ok(None);
        };
        let Some(im_dir) = config_dir
            .child(OsStr::new(IM_SUBDIR_NAME), false)
            .map_err(|err| map_private_file_error(PathRole::ImDir, err))?
        else {
            return Ok(None);
        };
        let Some(bytes) = im_dir
            .read(OsStr::new(AUTH_FILE_NAME), Some(MAX_FILE_BYTES))
            .map_err(|err| map_private_file_error(PathRole::AuthFile, err))?
        else {
            return Ok(None);
        };
        parse_document(&bytes).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, path::Path};

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .expect("读取测试路径元数据应成功")
            .permissions()
            .mode()
            & 0o777
    }

    fn set_mode(path: &Path, bits: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits))
            .expect("设置测试路径权限应成功");
    }

    fn write_raw_auth_file(config_dir: &Path, content: &[u8]) -> PathBuf {
        let im_dir = config_dir.join("im");
        std::fs::create_dir_all(&im_dir).expect("创建测试目录应成功");
        set_mode(config_dir, 0o700);
        set_mode(&im_dir, 0o700);
        let auth_path = im_dir.join("auth.json");
        std::fs::write(&auth_path, content).expect("写入测试凭据文件应成功");
        set_mode(&auth_path, 0o600);
        auth_path
    }

    #[test]
    fn missing_directories_or_file_report_missing_without_creating_paths() {
        let temp = tempfile::tempdir().expect("创建临时目录应成功");
        let missing_config = temp.path().join(".lattice");
        let store = ImAuthStore::new(missing_config.clone());

        assert!(
            store
                .get("telegram")
                .expect("缺失配置目录读取应成功")
                .is_none(),
            "缺失配置目录时 get 应返回 None"
        );
        assert_eq!(
            store
                .status("telegram")
                .expect("缺失配置目录状态查询应成功"),
            ImPlatformCredentialState::Missing,
            "缺失配置目录时状态应为 Missing"
        );
        assert!(
            !store.remove("telegram").expect("缺失配置目录删除应成功"),
            "缺失配置目录时 remove 应返回 false"
        );
        assert!(!missing_config.exists(), "只读查询与空删除不得创建配置目录");

        std::fs::create_dir(&missing_config).expect("创建配置目录应成功");
        set_mode(&missing_config, 0o700);
        assert!(
            store
                .get("telegram")
                .expect("缺失 im 子目录读取应成功")
                .is_none(),
            "缺失 im 子目录时 get 应返回 None"
        );
        assert!(
            !missing_config.join("im").exists(),
            "只读查询不得创建 im 子目录"
        );

        let im_dir = missing_config.join("im");
        std::fs::create_dir(&im_dir).expect("创建 im 子目录应成功");
        set_mode(&im_dir, 0o700);
        assert_eq!(
            store
                .status("telegram")
                .expect("缺失 auth.json 状态查询应成功"),
            ImPlatformCredentialState::Missing,
            "缺失 auth.json 时状态应为 Missing"
        );
        assert!(
            !store.remove("telegram").expect("缺失 auth.json 删除应成功"),
            "缺失 auth.json 时 remove 应返回 false"
        );
        assert!(
            !im_dir.join("auth.json").exists(),
            "缺失 auth.json 时 remove 不得创建空文件"
        );
    }

    #[test]
    fn set_creates_private_hierarchy_overwrites_and_removes_cleanly() {
        let temp = tempfile::tempdir().expect("创建临时目录应成功");
        let config_dir = temp.path().join(".lattice");
        let store = ImAuthStore::new(config_dir.clone());

        store
            .set("telegram", "123456:ABCDEF-first")
            .expect("首次写入 telegram 凭据应成功");

        assert_eq!(mode(&config_dir), 0o700, "新建配置目录权限须为 0700");
        assert_eq!(
            mode(&config_dir.join("im")),
            0o700,
            "新建 im 子目录权限须为 0700"
        );
        assert_eq!(
            mode(&config_dir.join("im/auth.json")),
            0o600,
            "新建 auth.json 权限须为 0600"
        );

        let token = store
            .get("telegram")
            .expect("读取已写入凭据应成功")
            .expect("telegram 凭据应存在");
        assert_eq!(
            token.as_str(),
            "123456:ABCDEF-first",
            "读取到的凭据应与写入一致"
        );
        assert_eq!(
            store.status("telegram").expect("状态查询应成功"),
            ImPlatformCredentialState::Present,
            "写入有效凭据后状态应为 Present"
        );

        store
            .set("telegram", "123456:ABCDEF-second")
            .expect("覆盖更新 telegram 凭据应成功");
        let updated = store
            .get("telegram")
            .expect("覆盖后读取凭据应成功")
            .expect("覆盖后凭据应存在");
        assert_eq!(
            updated.as_str(),
            "123456:ABCDEF-second",
            "覆盖更新后应读到新凭据"
        );
        assert_eq!(
            mode(&config_dir.join("im/auth.json")),
            0o600,
            "覆盖替换后 auth.json 权限仍须为 0600"
        );

        assert!(
            store.remove("telegram").expect("删除已存在凭据应成功"),
            "删除已存在条目应返回 true"
        );
        assert!(
            store.get("telegram").expect("删除后读取应成功").is_none(),
            "删除后 get 应返回 None"
        );
        assert_eq!(
            store.status("telegram").expect("删除后状态查询应成功"),
            ImPlatformCredentialState::Missing,
            "删除后状态应回到 Missing"
        );
        assert!(
            !store.remove("telegram").expect("重复删除应成功"),
            "条目已不存在时再次删除应返回 false"
        );
    }

    #[test]
    fn rejects_invalid_json_schema_versions_duplicate_keys_and_trailing_bytes() {
        let temp = tempfile::tempdir().expect("创建临时目录应成功");
        let config_dir = temp.path().join(".lattice");
        let store = ImAuthStore::new(config_dir.clone());

        let cases: &[(&str, &[u8])] = &[
            (
                "重复平台键",
                br#"{"version":1,"platforms":{"telegram":"tok-1","telegram":"tok-2"}}"#,
            ),
            (
                "未知顶层字段",
                br#"{"version":1,"platforms":{"telegram":"tok-1"},"extra":true}"#,
            ),
            (
                "尾随非空白字节",
                br#"{"version":1,"platforms":{"telegram":"tok-1"}} trailing"#,
            ),
            (
                "错误版本号",
                br#"{"version":2,"platforms":{"telegram":"tok-1"}}"#,
            ),
            (
                "不受支持的文件内平台键",
                br#"{"version":1,"platforms":{"discord":"tok-1"}}"#,
            ),
            ("顶层非对象", br#"["not-an-object"]"#),
            ("损坏 JSON", br#"{"version":1,"platforms":"#),
        ];

        for (label, raw) in cases {
            write_raw_auth_file(&config_dir, raw);
            let Err(err) = store.get("telegram") else {
                panic!("{label} 必须被 get 拒绝");
            };
            assert!(
                matches!(err, ImAuthError::InvalidFile(_)),
                "{label} 应映射为 InvalidFile: {err}"
            );
            let state = store
                .status("telegram")
                .expect("损坏文件时 status 应返回 Invalid 状态而非崩溃");
            assert!(
                matches!(state, ImPlatformCredentialState::Invalid { .. }),
                "{label} 时 status 应返回 Invalid: {state:?}"
            );
        }

        // 超过 1 MiB 的文件也必须返回 InvalidFile / Invalid 状态。
        let oversized = vec![b' '; MAX_FILE_BYTES + 1];
        write_raw_auth_file(&config_dir, &oversized);
        assert!(
            matches!(store.get("telegram"), Err(ImAuthError::InvalidFile(_))),
            "超过 1 MiB 的凭据文件必须被拒绝"
        );
        assert!(
            matches!(
                store.status("telegram").expect("超限文件状态查询应成功"),
                ImPlatformCredentialState::Invalid { .. }
            ),
            "超过 1 MiB 的凭据文件状态必须为 Invalid"
        );
    }

    #[test]
    fn rejects_empty_oversized_and_control_char_tokens() {
        let temp = tempfile::tempdir().expect("创建临时目录应成功");
        let config_dir = temp.path().join(".lattice");
        let store = ImAuthStore::new(config_dir.clone());

        let max_valid = "a".repeat(MAX_TOKEN_BYTES);
        store
            .set("telegram", &max_valid)
            .expect("恰好 16384 字节的合法凭据应允许写入");

        let too_long = "a".repeat(MAX_TOKEN_BYTES + 1);
        for bad in [
            "",
            &too_long,
            "tok\nwith-newline",
            "tok\0nul",
            "tok\u{0085}nel",
        ] {
            assert_eq!(
                store.set("telegram", bad),
                Err(ImAuthError::InvalidToken),
                "非法凭据必须在 set 阶段被拒绝"
            );
            assert!(
                ImToken::parse(bad).is_err(),
                "ImToken::parse 必须拒绝非法凭据"
            );
        }

        // 磁盘中若手工写入含控制字符或空字符串的密钥，读取也必须拒绝。
        write_raw_auth_file(&config_dir, br#"{"version":1,"platforms":{"telegram":""}}"#);
        assert!(
            matches!(store.get("telegram"), Err(ImAuthError::InvalidFile(_))),
            "文件中为空字符串的密钥必须被读取拒绝"
        );

        write_raw_auth_file(
            &config_dir,
            br#"{"version":1,"platforms":{"telegram":"bad\ntoken"}}"#,
        );
        assert!(
            matches!(store.get("telegram"), Err(ImAuthError::InvalidFile(_))),
            "文件中含换行控制字符的密钥必须被读取拒绝"
        );
    }

    #[test]
    fn rejects_broad_permissions_symlinks_and_non_regular_files_without_chmod() {
        let temp = tempfile::tempdir().expect("创建临时目录应成功");
        let config_dir = temp.path().join(".lattice");
        let auth_path = write_raw_auth_file(
            &config_dir,
            br#"{"version":1,"platforms":{"telegram":"valid-token"}}"#,
        );
        let im_dir = config_dir.join("im");
        let store = ImAuthStore::new(config_dir.clone());

        // 1. 凭据文件权限过宽（0644），必须拒绝且不自动改回 0600。
        set_mode(&auth_path, 0o644);
        assert!(
            matches!(store.get("telegram"), Err(ImAuthError::InvalidFile(_))),
            "0644 凭据文件必须被 get 拒绝"
        );
        assert!(
            matches!(
                store.set("telegram", "new-token"),
                Err(ImAuthError::InvalidFile(_))
            ),
            "已有 0644 凭据文件时 set 读取阶段必须拒绝"
        );
        assert_eq!(mode(&auth_path), 0o644, "拒绝过宽文件权限时不得自动 chmod");
        set_mode(&auth_path, 0o600);

        // 2. im 子目录权限过宽（0750），必须拒绝且不自动改回 0700。
        set_mode(&im_dir, 0o750);
        assert!(
            matches!(store.get("telegram"), Err(ImAuthError::InvalidFile(_))),
            "0750 im 子目录必须被 get 拒绝"
        );
        assert!(
            matches!(
                store.set("telegram", "new-token"),
                Err(ImAuthError::InvalidFile(_))
            ),
            "0750 im 子目录必须被 set 拒绝"
        );
        assert_eq!(mode(&im_dir), 0o750, "拒绝过宽子目录权限时不得自动 chmod");
        set_mode(&im_dir, 0o700);

        // 3. 配置目录权限过宽（0755），必须拒绝且不自动改回 0700。
        set_mode(&config_dir, 0o755);
        assert!(
            matches!(store.get("telegram"), Err(ImAuthError::InvalidFile(_))),
            "0755 配置目录必须被拒绝"
        );
        assert_eq!(
            mode(&config_dir),
            0o755,
            "拒绝过宽配置目录权限时不得自动 chmod"
        );
        set_mode(&config_dir, 0o700);

        // 4. auth.json 为符号链接时拒绝。
        let target = temp.path().join("outside.json");
        std::fs::write(&target, br#"{"version":1,"platforms":{"telegram":"leak"}}"#)
            .expect("写入外部目标文件应成功");
        set_mode(&target, 0o600);
        std::fs::remove_file(&auth_path).expect("移除原凭据文件应成功");
        std::os::unix::fs::symlink(&target, &auth_path).expect("创建文件符号链接应成功");
        assert!(
            matches!(store.get("telegram"), Err(ImAuthError::InvalidFile(_))),
            "符号链接凭据文件必须被 get 拒绝"
        );
        assert!(
            matches!(store.remove("telegram"), Err(ImAuthError::InvalidFile(_))),
            "符号链接凭据文件必须被 remove 拒绝"
        );
        std::fs::remove_file(&auth_path).expect("清理符号链接应成功");

        // 5. auth.json 位置为目录（非普通文件）时拒绝。
        std::fs::create_dir(&auth_path).expect("在 auth.json 位置创建目录应成功");
        set_mode(&auth_path, 0o700);
        assert!(
            matches!(store.get("telegram"), Err(ImAuthError::InvalidFile(_))),
            "目录冒充凭据文件必须被拒绝"
        );
        std::fs::remove_dir(&auth_path).expect("移除冒充目录应成功");

        // 6. im 子目录为符号链接时拒绝。
        std::fs::remove_dir(&im_dir).expect("移除 im 目录应成功");
        let outside_dir = temp.path().join("outside-im");
        std::fs::create_dir(&outside_dir).expect("创建外部目录应成功");
        set_mode(&outside_dir, 0o700);
        std::os::unix::fs::symlink(&outside_dir, &im_dir).expect("创建目录符号链接应成功");
        assert!(
            matches!(store.get("telegram"), Err(ImAuthError::InvalidFile(_))),
            "符号链接 im 目录必须被 get 拒绝"
        );
        assert!(
            matches!(
                store.set("telegram", "new-token"),
                Err(ImAuthError::InvalidFile(_))
            ),
            "符号链接 im 目录必须被 set 拒绝"
        );
    }

    #[test]
    fn errors_never_echo_platform_argument_or_secret_contents() {
        const ACCIDENTAL_SECRET_IN_PLATFORM: &str = "998877:SUPER_SECRET_IN_PLATFORM_ARG";
        const SECRET_IN_TOKEN: &str = "112233:SUPER_SECRET_BOT_TOKEN_VALUE";

        let temp = tempfile::tempdir().expect("创建临时目录应成功");
        let config_dir = temp.path().join(".lattice");
        let store = ImAuthStore::new(config_dir.clone());

        let platform_err = store
            .set(ACCIDENTAL_SECRET_IN_PLATFORM, "valid-token")
            .expect_err("非法平台名应被拒绝");
        assert_eq!(platform_err, ImAuthError::UnsupportedPlatform);
        let rendered = format!("{platform_err} {platform_err:?}");
        assert!(
            !rendered.contains("SUPER_SECRET") && !rendered.contains("998877"),
            "非法平台错误不得回显平台参数原文: {rendered}"
        );

        let token_err = store
            .set("telegram", &format!("{SECRET_IN_TOKEN}\n"))
            .expect_err("含控制字符的密钥应被拒绝");
        let rendered_token_err = format!("{token_err} {token_err:?}");
        assert!(
            !rendered_token_err.contains("SUPER_SECRET") && !rendered_token_err.contains("112233"),
            "非法密钥错误不得回显密钥内容: {rendered_token_err}"
        );

        // 文件中损坏 JSON 含有秘密字面量时，解析报错与 status 原因也不得泄露文件内容。
        let corrupted = format!(
            r#"{{"version":1,"platforms":{{"{ACCIDENTAL_SECRET_IN_PLATFORM}":"{SECRET_IN_TOKEN}"}}}}"#
        );
        write_raw_auth_file(&config_dir, corrupted.as_bytes());
        let Err(file_err) = store.get("telegram") else {
            panic!("含非法平台键的文件应被拒绝");
        };
        let rendered_file_err = format!("{file_err} {file_err:?}");
        assert!(
            !rendered_file_err.contains("SUPER_SECRET")
                && !rendered_file_err.contains("998877")
                && !rendered_file_err.contains("112233"),
            "文件校验错误不得回显文件中的键或值: {rendered_file_err}"
        );

        let status = store
            .status("telegram")
            .expect("文件校验错误应转为 Invalid 状态");
        let rendered_status = format!("{status:?}");
        assert!(
            !rendered_status.contains("SUPER_SECRET")
                && !rendered_status.contains("998877")
                && !rendered_status.contains("112233"),
            "状态查询原因不得回显文件中的键或值: {rendered_status}"
        );
    }
}
