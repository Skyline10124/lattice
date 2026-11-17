// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 五种协议请求体的同源流式 JSON 编码与零分配计长。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use base64::display::Base64Display;
use base64::engine::general_purpose::STANDARD;
use lattice_conversation::{ImageRef, Protocol};
use serde::ser::{Serialize, SerializeMap, SerializeSeq};
use serde_json::Value;

use crate::image_source::{no_images_reader, short_image_digest};
use crate::protocol::projection::ProjectionError;
use crate::{ImageReader, LlmError};

/// 后台单次模型 HTTP 请求体的本地内存预算，五种协议统一适用，不代表远端服务的上限。
pub(crate) const MAX_MODEL_REQUEST_BYTES: usize = 33_554_432;

/// 协议请求体的同源表示：顶层非消息字段按字典序存放，消息列表在对应键位流式插入。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RequestBody {
    pub(crate) fields: serde_json::Map<String, Value>,
    pub(crate) list_key: &'static str,
    pub(crate) items: Vec<WireItem>,
}

impl RequestBody {
    pub(crate) fn new(
        fields: serde_json::Map<String, Value>,
        list_key: &'static str,
        items: Vec<WireItem>,
    ) -> Self {
        debug_assert!(
            !fields.contains_key(list_key),
            "请求顶层字段表不应重复包含消息列表键"
        );
        Self {
            fields,
            list_key,
            items,
        }
    }

    pub(crate) fn has_user_images(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, WireItem::UserImages(_)))
    }
}

/// 消息列表中的单个条目。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WireItem {
    /// 不含图片的条目，直接沿用现有 `Value` 构造。
    Json(Value),
    /// 至少包含一张图片的用户消息。
    UserImages(UserWire),
}

/// 含图用户消息内部的有序片段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UserPart {
    Text(String),
    Image(ImageRef),
}

/// 含图用户消息的协议线格式描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserWire {
    protocol: Protocol,
    parts: Vec<UserPart>,
    cache_last_text: bool,
}

impl UserWire {
    pub(crate) fn new(protocol: Protocol, parts: Vec<UserPart>) -> Self {
        Self {
            protocol,
            parts,
            cache_last_text: false,
        }
    }

    /// 供 Chat Completions `cache_markers` 在该消息位于末尾时启用末块文本缓存标记。
    pub(crate) fn enable_last_text_cache_marker(&mut self) {
        self.cache_last_text = true;
    }
}

/// 图片在目标协议字段中的字符串包裹形式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageEncoding {
    /// 纯标准填充无换行 base64 字符串。
    Base64,
    /// `data:<mime>;base64,<b64>` Data URL 字符串。
    DataUrl,
}

/// 图片载荷的同源序列化策略：计长与实际写出共用同一套协议结构包装器。
pub(crate) trait ImageEncoder {
    fn serialize_image<S: serde::Serializer>(
        &self,
        image: &ImageRef,
        encoding: ImageEncoding,
        serializer: S,
    ) -> Result<S::Ok, S::Error>;
}

struct BodySer<'a, E> {
    body: &'a RequestBody,
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for BodySer<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.body.fields.len() + 1))?;
        let mut list_written = false;
        for (key, value) in &self.body.fields {
            if !list_written && self.body.list_key < key.as_str() {
                map.serialize_entry(
                    self.body.list_key,
                    &ItemsSer {
                        items: &self.body.items,
                        encoder: self.encoder,
                    },
                )?;
                list_written = true;
            }
            map.serialize_entry(key, value)?;
        }
        if !list_written {
            map.serialize_entry(
                self.body.list_key,
                &ItemsSer {
                    items: &self.body.items,
                    encoder: self.encoder,
                },
            )?;
        }
        map.end()
    }
}

struct ItemsSer<'a, E> {
    items: &'a [WireItem],
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for ItemsSer<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.items.len()))?;
        for item in self.items {
            seq.serialize_element(&ItemSer {
                item,
                encoder: self.encoder,
            })?;
        }
        seq.end()
    }
}

struct ItemSer<'a, E> {
    item: &'a WireItem,
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for ItemSer<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.item {
            WireItem::Json(value) => value.serialize(serializer),
            WireItem::UserImages(wire) => UserWireSer {
                wire,
                encoder: self.encoder,
            }
            .serialize(serializer),
        }
    }
}

struct UserWireSer<'a, E> {
    wire: &'a UserWire,
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for UserWireSer<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        match self.wire.protocol {
            Protocol::ChatCompletions | Protocol::Responses | Protocol::Messages => {
                map.serialize_entry(
                    "content",
                    &UserPartsSer {
                        wire: self.wire,
                        encoder: self.encoder,
                    },
                )?;
                map.serialize_entry("role", "user")?;
            }
            Protocol::GenerateContent => {
                map.serialize_entry(
                    "parts",
                    &UserPartsSer {
                        wire: self.wire,
                        encoder: self.encoder,
                    },
                )?;
                map.serialize_entry("role", "user")?;
            }
            Protocol::Interactions => {
                map.serialize_entry(
                    "content",
                    &UserPartsSer {
                        wire: self.wire,
                        encoder: self.encoder,
                    },
                )?;
                map.serialize_entry("type", "user_input")?;
            }
        }
        map.end()
    }
}

struct UserPartsSer<'a, E> {
    wire: &'a UserWire,
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for UserPartsSer<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len = self.wire.parts.len();
        let mut seq = serializer.serialize_seq(Some(len))?;
        for (index, part) in self.wire.parts.iter().enumerate() {
            let cache_ephemeral = self.wire.cache_last_text && index + 1 == len;
            seq.serialize_element(&UserPartSer {
                protocol: self.wire.protocol,
                part,
                cache_ephemeral,
                encoder: self.encoder,
            })?;
        }
        seq.end()
    }
}

struct UserPartSer<'a, E> {
    protocol: Protocol,
    part: &'a UserPart,
    cache_ephemeral: bool,
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for UserPartSer<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match (self.protocol, self.part) {
            (Protocol::ChatCompletions, UserPart::Text(text)) => {
                if self.cache_ephemeral {
                    let mut map = serializer.serialize_map(Some(3))?;
                    map.serialize_entry("cache_control", &EphemeralCacheControl)?;
                    map.serialize_entry("text", text)?;
                    map.serialize_entry("type", "text")?;
                    map.end()
                } else {
                    let mut map = serializer.serialize_map(Some(2))?;
                    map.serialize_entry("text", text)?;
                    map.serialize_entry("type", "text")?;
                    map.end()
                }
            }
            (Protocol::ChatCompletions, UserPart::Image(image)) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry(
                    "image_url",
                    &ChatCompletionsImageUrl {
                        image,
                        encoder: self.encoder,
                    },
                )?;
                map.serialize_entry("type", "image_url")?;
                map.end()
            }
            (Protocol::Responses, UserPart::Text(text)) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("text", text)?;
                map.serialize_entry("type", "input_text")?;
                map.end()
            }
            (Protocol::Responses, UserPart::Image(image)) => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("detail", "auto")?;
                map.serialize_entry(
                    "image_url",
                    &ImageValueSer {
                        image,
                        encoding: ImageEncoding::DataUrl,
                        encoder: self.encoder,
                    },
                )?;
                map.serialize_entry("type", "input_image")?;
                map.end()
            }
            (Protocol::Messages, UserPart::Text(text)) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("text", text)?;
                map.serialize_entry("type", "text")?;
                map.end()
            }
            (Protocol::Messages, UserPart::Image(image)) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry(
                    "source",
                    &MessagesImageSource {
                        image,
                        encoder: self.encoder,
                    },
                )?;
                map.serialize_entry("type", "image")?;
                map.end()
            }
            (Protocol::GenerateContent, UserPart::Text(text)) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("text", text)?;
                map.end()
            }
            (Protocol::GenerateContent, UserPart::Image(image)) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "inlineData",
                    &GenerateContentInlineData {
                        image,
                        encoder: self.encoder,
                    },
                )?;
                map.end()
            }
            (Protocol::Interactions, UserPart::Text(text)) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("text", text)?;
                map.serialize_entry("type", "text")?;
                map.end()
            }
            (Protocol::Interactions, UserPart::Image(image)) => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry(
                    "data",
                    &ImageValueSer {
                        image,
                        encoding: ImageEncoding::Base64,
                        encoder: self.encoder,
                    },
                )?;
                map.serialize_entry("mime_type", image.media_type.mime_type())?;
                map.serialize_entry("type", "image")?;
                map.end()
            }
        }
    }
}

struct EphemeralCacheControl;

impl Serialize for EphemeralCacheControl {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("type", "ephemeral")?;
        map.end()
    }
}

struct ChatCompletionsImageUrl<'a, E> {
    image: &'a ImageRef,
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for ChatCompletionsImageUrl<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(
            "url",
            &ImageValueSer {
                image: self.image,
                encoding: ImageEncoding::DataUrl,
                encoder: self.encoder,
            },
        )?;
        map.end()
    }
}

struct MessagesImageSource<'a, E> {
    image: &'a ImageRef,
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for MessagesImageSource<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry(
            "data",
            &ImageValueSer {
                image: self.image,
                encoding: ImageEncoding::Base64,
                encoder: self.encoder,
            },
        )?;
        map.serialize_entry("media_type", self.image.media_type.mime_type())?;
        map.serialize_entry("type", "base64")?;
        map.end()
    }
}

struct GenerateContentInlineData<'a, E> {
    image: &'a ImageRef,
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for GenerateContentInlineData<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry(
            "data",
            &ImageValueSer {
                image: self.image,
                encoding: ImageEncoding::Base64,
                encoder: self.encoder,
            },
        )?;
        map.serialize_entry("mimeType", self.image.media_type.mime_type())?;
        map.end()
    }
}

struct ImageValueSer<'a, E> {
    image: &'a ImageRef,
    encoding: ImageEncoding,
    encoder: &'a E,
}

impl<E: ImageEncoder> Serialize for ImageValueSer<'_, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.encoder
            .serialize_image(self.image, self.encoding, serializer)
    }
}

struct CountingWriter {
    count: Rc<Cell<Option<usize>>>,
}

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let next = self
            .count
            .get()
            .and_then(|current| current.checked_add(buf.len()));
        self.count.set(next);
        if next.is_none() {
            return Err(std::io::Error::other("请求体编码长度超出上限"));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 零图片读取的精确计长编码器：只写出前缀与首尾双引号，将 base64 长度受检累加到共享计数器。
pub(crate) struct SizingEncoder {
    count: Rc<Cell<Option<usize>>>,
}

impl ImageEncoder for SizingEncoder {
    fn serialize_image<S: serde::Serializer>(
        &self,
        image: &ImageRef,
        encoding: ImageEncoding,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let b64_u64 = image
            .byte_length
            .div_ceil(3)
            .checked_mul(4)
            .ok_or_else(|| {
                self.count.set(None);
                serde::ser::Error::custom("请求体编码长度超出上限")
            })?;
        let b64_len = usize::try_from(b64_u64).map_err(|_| {
            self.count.set(None);
            serde::ser::Error::custom("请求体编码长度超出上限")
        })?;
        let ok = match encoding {
            ImageEncoding::Base64 => serializer.serialize_str("")?,
            ImageEncoding::DataUrl => serializer.collect_str(&format_args!(
                "data:{};base64,",
                image.media_type.mime_type()
            ))?,
        };
        let next = self
            .count
            .get()
            .and_then(|current| current.checked_add(b64_len));
        self.count.set(next);
        if next.is_none() {
            return Err(serde::ser::Error::custom("请求体编码长度超出上限"));
        }
        Ok(ok)
    }
}

/// 从读取作用域按需借用图片切片、直接流式写出 base64 的编码器。
pub(crate) struct StreamingBlobEncoder<'a> {
    reader: &'a dyn ImageReader,
    error: RefCell<Option<LlmError>>,
}

impl<'a> StreamingBlobEncoder<'a> {
    pub(crate) fn new(reader: &'a dyn ImageReader) -> Self {
        Self {
            reader,
            error: RefCell::new(None),
        }
    }

    fn record_error(&self, err: LlmError) {
        let mut slot = self.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(err);
        }
    }

    fn take_error(&self) -> Option<LlmError> {
        self.error.borrow_mut().take()
    }
}

fn ensure_other_with_digest(error: LlmError, sha256: &[u8; 32]) -> LlmError {
    let digest = short_image_digest(sha256);
    match error {
        LlmError::Other(message) => {
            if message.contains(&digest) {
                LlmError::Other(message)
            } else {
                LlmError::Other(format!("读取图片 {digest} 失败：{message}"))
            }
        }
        _ => LlmError::Other(format!("读取图片 {digest} 失败")),
    }
}

impl ImageEncoder for StreamingBlobEncoder<'_> {
    fn serialize_image<S: serde::Serializer>(
        &self,
        image: &ImageRef,
        encoding: ImageEncoding,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut serializer_slot = Some(serializer);
        let mut result_slot: Option<Result<S::Ok, S::Error>> = None;

        let callback_res = self.reader.with_image(image, &mut |bytes| {
            let Some(ser) = serializer_slot.take() else {
                return Err(LlmError::Other(format!(
                    "图片 {} 读取回调被重复调用",
                    short_image_digest(&image.sha256)
                )));
            };
            if bytes.len() as u64 != image.byte_length {
                return Err(LlmError::Other(format!(
                    "图片 {} 实际字节数（{}）与记录长度（{}）不符",
                    short_image_digest(&image.sha256),
                    bytes.len(),
                    image.byte_length,
                )));
            }
            let b64 = Base64Display::new(bytes, &STANDARD);
            let res = match encoding {
                ImageEncoding::Base64 => ser.collect_str(&b64),
                ImageEncoding::DataUrl => ser.collect_str(&format_args!(
                    "data:{};base64,{b64}",
                    image.media_type.mime_type()
                )),
            };
            let failed = res.is_err();
            result_slot = Some(res);
            if failed {
                Err(LlmError::Other(format!(
                    "图片 {} 序列化写出失败",
                    short_image_digest(&image.sha256)
                )))
            } else {
                Ok(())
            }
        });

        match callback_res {
            Ok(()) => match result_slot {
                Some(res) => res,
                None => {
                    self.record_error(LlmError::Other(format!(
                        "图片 {} 读取器未交付字节",
                        short_image_digest(&image.sha256)
                    )));
                    Err(serde::ser::Error::custom("图片读取器未交付字节"))
                }
            },
            Err(err) => {
                if let Some(Err(ser_err)) = result_slot {
                    return Err(ser_err);
                }
                self.record_error(ensure_other_with_digest(err, &image.sha256));
                Err(serde::ser::Error::custom("图片读取失败"))
            }
        }
    }
}

/// 不访问任何图片来源，精确计算整包 JSON 请求体的 UTF-8 字节长度。
pub(crate) fn encoded_len(body: &RequestBody) -> Result<usize, LlmError> {
    let count = Rc::new(Cell::new(Some(0usize)));
    let encoder = SizingEncoder {
        count: count.clone(),
    };
    let writer = CountingWriter {
        count: count.clone(),
    };
    let mut serializer = serde_json::Serializer::new(writer);
    let result = (BodySer {
        body,
        encoder: &encoder,
    })
    .serialize(&mut serializer);
    if count.get().is_none() {
        return Err(LlmError::Other("请求体编码长度超出上限".into()));
    }
    if result.is_err() {
        return Err(LlmError::Other("计算请求体编码长度失败".into()));
    }
    count
        .get()
        .ok_or_else(|| LlmError::Other("请求体编码长度超出上限".into()))
}

/// 按 `expected_len` 预分配缓冲并一次性流式写出完整 JSON 请求体。
pub(crate) fn encode_body(
    body: &RequestBody,
    reader: &dyn ImageReader,
    expected_len: usize,
) -> Result<Vec<u8>, LlmError> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(expected_len)
        .map_err(|_| LlmError::Other(format!("无法为请求体预分配 {expected_len} 字节缓冲")))?;
    let encoder = StreamingBlobEncoder::new(reader);
    let mut serializer = serde_json::Serializer::new(&mut buf);
    if (BodySer {
        body,
        encoder: &encoder,
    })
    .serialize(&mut serializer)
    .is_err()
    {
        if let Some(err) = encoder.take_error() {
            return Err(err);
        }
        return Err(LlmError::Other("序列化请求体失败".into()));
    }
    if buf.len() != expected_len {
        return Err(LlmError::Other(format!(
            "请求体实际编码字节数（{}）与预估长度（{}）不符",
            buf.len(),
            expected_len,
        )));
    }
    Ok(buf)
}

/// 纯文本请求专用入口：不调用外部 `ImageSource`，直接写出字节。
pub(crate) fn encode_body_without_images(
    body: &RequestBody,
    expected_len: usize,
) -> Result<Vec<u8>, LlmError> {
    encode_body(body, no_images_reader(), expected_len)
}

/// 供公开纯文本投影测试入口把 `WireItem` 列表经同一编码器还原为 `Value` 列表。
pub(crate) fn items_to_values(items: &[WireItem]) -> Result<Vec<Value>, ProjectionError> {
    let encoder = StreamingBlobEncoder::new(no_images_reader());
    let bytes = serde_json::to_vec(&ItemsSer {
        items,
        encoder: &encoder,
    })
    .map_err(|_| ProjectionError::ImageSourceRequired)?;
    serde_json::from_slice(&bytes).map_err(|_| ProjectionError::ImageSourceRequired)
}

#[cfg(test)]
pub(crate) fn body_to_value(body: &RequestBody) -> Value {
    let len = encoded_len(body).expect("测试请求体计长应成功");
    let bytes = encode_body_without_images(body, len).expect("测试请求体编码应成功");
    serde_json::from_slice(&bytes).expect("编码后的请求字节应为合法 JSON")
}
