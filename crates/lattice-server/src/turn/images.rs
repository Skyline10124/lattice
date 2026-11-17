// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 由会话库提供图片字节的模型请求图片来源。

use std::fmt::Write;

use lattice_conversation::ImageRef;
use lattice_llm::{ImageReader, ImageSource, LlmError};
use lattice_session::store::SessionRead;

use crate::session::SessionApi;

/// 由会话库提供图片字节的模型请求图片来源。
pub(crate) struct SessionImages {
    api: SessionApi,
}

impl SessionImages {
    pub(crate) fn new(api: SessionApi) -> Self {
        Self { api }
    }
}

impl ImageSource for SessionImages {
    fn with_reader(
        &self,
        encode: &mut dyn FnMut(&dyn ImageReader) -> Result<(), LlmError>,
    ) -> Result<(), LlmError> {
        let read = self
            .api
            .store
            .begin_read()
            .map_err(|err| LlmError::Other(format!("打开会话库图片读取事务失败：{err}")))?;
        let reader = SessionImageReader { read };
        encode(&reader)
    }
}

struct SessionImageReader {
    read: SessionRead,
}

impl ImageReader for SessionImageReader {
    fn with_image(
        &self,
        image: &ImageRef,
        consume: &mut dyn FnMut(&[u8]) -> Result<(), LlmError>,
    ) -> Result<(), LlmError> {
        self.read
            .with_blob(&image.sha256, |bytes| consume(bytes))
            .map_err(|err| {
                LlmError::Other(format!(
                    "读取会话库图片 {} 失败：{err}",
                    short_digest(&image.sha256)
                ))
            })?
    }
}

fn short_digest(sha256: &[u8; 32]) -> String {
    let mut out = String::with_capacity(12);
    for byte in &sha256[..6] {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lattice_conversation::{ImageRef, ModelId, ProviderId, TurnId};
    use lattice_session::{
        AgentName, ProjectPath, StateLayout, TurnTimeout,
        store::{
            AcceptTurn, FrozenTurnConfig, SessionSettings, SessionStore, ToolCatalogSnapshot,
            TurnExecutionLimits, TurnInput, TurnInputPart,
        },
    };

    use super::*;

    fn test_config() -> FrozenTurnConfig {
        FrozenTurnConfig {
            agent_name: AgentName::parse("alpha").expect("合法测试代理名称"),
            model: ModelId::parse("vision-model").expect("合法测试模型标识"),
            provider: ProviderId::parse("openai").expect("合法测试 provider 标识"),
            effort: None,
            system_prompt: None,
            catalog: ToolCatalogSnapshot::new(Vec::new()).expect("空工具目录快照"),
            limits: TurnExecutionLimits {
                timeout_secs: TurnTimeout::default(),
                max_model_steps: 32,
                max_model_attempts: 2,
            },
        }
    }

    #[test]
    fn session_images_borrows_persisted_blobs_across_reopen_and_propagates_errors() {
        let state = tempfile::tempdir().expect("创建临时状态目录");
        let project = tempfile::tempdir().expect("创建临时项目目录");
        let canonical_project = std::fs::canonicalize(project.path()).expect("规范化项目路径");
        let layout = StateLayout::in_state_root(state.path());
        let store = Arc::new(SessionStore::create(&layout).expect("创建会话库"));

        let session_id = lattice_local::new_session_id();
        let turn_id =
            TurnId::parse("00000000-0000-4000-8000-000000000001").expect("合法测试 TurnId");
        let project_path = ProjectPath::from_path(&canonical_project).expect("合法规范项目路径");
        let config = test_config();
        let settings = SessionSettings::for_test(&config);
        let settings_version = settings.version().expect("计算设置版本");

        let lock = store.try_lock_session(session_id).expect("获取会话执行锁");
        store
            .create_session(&lock, project_path.clone(), settings, chrono::Utc::now())
            .expect("创建测试会话");

        let raw_png = b"\x89PNG\r\n\x1a\nTEST_SESSION_IMAGE_BYTES".to_vec();
        let image_ref = ImageRef::from_bytes(&raw_png).expect("构造图片引用");
        let input = TurnInput::new(vec![
            TurnInputPart::Text {
                text: "请描述图片".into(),
            },
            TurnInputPart::Image {
                bytes: raw_png.clone(),
            },
        ])
        .expect("构造含图轮次输入");

        store
            .accept_turn(
                &lock,
                AcceptTurn {
                    session_id,
                    turn_id,
                    project_path,
                    input,
                    config,
                    accepted_at: chrono::Utc::now(),
                    settings_version,
                },
            )
            .expect("接受含图轮次");
        drop(lock);

        // 1. 当前打开句柄借出的字节与原字节一致，且 consume 错误原样传回。
        let images = SessionImages::new(SessionApi::from_store(store.clone()));
        let mut calls = 0usize;
        images
            .with_reader(&mut |reader| {
                calls += 1;
                reader.with_image(&image_ref, &mut |bytes| {
                    assert_eq!(bytes, raw_png.as_slice(), "借出的图片字节必须与原字节一致");
                    Ok(())
                })
            })
            .expect("读取已持久图片应成功");
        assert_eq!(calls, 1, "with_reader 必须恰好调用一次 encode");

        let consume_err = images
            .with_reader(&mut |reader| {
                reader.with_image(&image_ref, &mut |_| {
                    Err(LlmError::RequestBudgetExceeded {
                        message: "预算超限原文".into(),
                    })
                })
            })
            .expect_err("consume 返回的错误必须原样传回");
        assert_eq!(
            consume_err,
            LlmError::RequestBudgetExceeded {
                message: "预算超限原文".into()
            }
        );

        // 2. 缺失摘要返回 LlmError::Other，消息不含原始图片字节。
        let missing = ImageRef {
            sha256: [0xAB; 32],
            ..image_ref
        };
        let missing_err = images
            .with_reader(&mut |reader| reader.with_image(&missing, &mut |_| Ok(())))
            .expect_err("缺失摘要必须返回错误");
        match missing_err {
            LlmError::Other(msg) => {
                assert!(msg.contains("读取会话库图片"), "错误消息应为中文：{msg}");
                assert!(
                    !msg.contains("TEST_SESSION_IMAGE_BYTES"),
                    "错误消息不得包含图片字节：{msg}"
                );
            }
            other => panic!("期望返回 LlmError::Other，实际为：{other:?}"),
        }

        // 3. 关闭并重新打开会话库后再读，结果仍相同。
        drop(images);
        drop(store);
        let reopened = Arc::new(SessionStore::open(&layout).expect("重新打开含图会话库"));
        let reopened_images = SessionImages::new(SessionApi::from_store(reopened));
        reopened_images
            .with_reader(&mut |reader| {
                reader.with_image(&image_ref, &mut |bytes| {
                    assert_eq!(
                        bytes,
                        raw_png.as_slice(),
                        "重开会话库后借出的图片字节必须与原字节一致"
                    );
                    Ok(())
                })
            })
            .expect("重开后读取已持久图片应成功");
    }
}
