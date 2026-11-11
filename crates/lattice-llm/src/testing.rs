// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 测试输入只描述原始内容；完整消息与实时投影仍由生产包装器产生。

pub use crate::reply::{PartDelta, ProtocolEvent};
use crate::reply::{PartKind, StringPatch};
pub use crate::reply_stream::assemble_reply_stream;
use crate::{LlmStream, ProviderFailure, StreamControl, StreamStop};
use futures::{StreamExt, stream};
use lattice_conversation::{MessagePart, MessageSemantic, ReplayOrigin, UnboundToolCall};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub enum ScriptEvent {
    Text {
        content: String,
    },
    Reasoning {
        content: String,
    },
    ToolMetadata {
        index: usize,
        provider_call_id: Option<String>,
        name: String,
    },
    ToolArguments {
        index: usize,
        arguments_delta: String,
    },
    ToolEnd {
        index: usize,
    },
    Done {
        finish_reason: String,
        usage: Option<lattice_conversation::TokenUsage>,
    },
    Failure {
        failure: ProviderFailure,
    },
}

#[derive(Default)]
struct ScriptParser {
    current: Option<(usize, PartKind)>,
    calls: BTreeMap<usize, (usize, bool)>,
    next_index: usize,
}
impl ScriptParser {
    fn close(&mut self, output: &mut Vec<ProtocolEvent>) {
        if let Some((index, _)) = self.current.take() {
            output.push(ProtocolEvent::Part(PartDelta::End { index }));
        }
    }
    fn call(&mut self, wire: usize, output: &mut Vec<ProtocolEvent>) -> usize {
        if let Some((index, _)) = self.calls.get(&wire) {
            return *index;
        }
        self.close(output);
        let index = self.next_index;
        self.next_index += 1;
        self.calls.insert(wire, (index, false));
        output.push(ProtocolEvent::Part(PartDelta::Begin {
            index,
            kind: PartKind::ToolCall,
        }));
        index
    }
    fn parse(&mut self, event: ScriptEvent) -> Vec<ProtocolEvent> {
        let mut output = Vec::new();
        match event {
            ScriptEvent::Text { content } => return self.text(PartKind::Text, content),
            ScriptEvent::Reasoning { content } => return self.text(PartKind::Reasoning, content),
            ScriptEvent::ToolMetadata {
                index,
                provider_call_id,
                name,
            } => {
                let index = self.call(index, &mut output);
                output.push(ProtocolEvent::Part(PartDelta::ToolMetadata {
                    index,
                    name: (!name.is_empty()).then_some(StringPatch::Set(name)),
                    provider_call_id: provider_call_id.map(StringPatch::Set),
                }));
            }
            ScriptEvent::ToolArguments {
                index,
                arguments_delta,
            } => {
                let index = self.call(index, &mut output);
                output.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                    index,
                    delta: arguments_delta,
                }));
            }
            ScriptEvent::ToolEnd { index } => {
                let index = self.call(index, &mut output);
                let (_, closed) = self
                    .calls
                    .values_mut()
                    .find(|(key, _)| *key == index)
                    .unwrap();
                *closed = true;
                output.push(ProtocolEvent::Part(PartDelta::End { index }));
            }
            ScriptEvent::Done {
                finish_reason,
                usage,
            } => {
                self.close(&mut output);
                for (index, closed) in self.calls.values_mut() {
                    if !*closed {
                        output.push(ProtocolEvent::Part(PartDelta::End { index: *index }));
                        *closed = true;
                    }
                }
                if let Some(usage) = usage {
                    output.push(ProtocolEvent::Usage(usage));
                }
                output.push(ProtocolEvent::Done { finish_reason });
            }
            ScriptEvent::Failure { failure } => {
                output.push(ProtocolEvent::Failed(StreamStop::Provider(failure)))
            }
        }
        output
    }
    fn text(&mut self, kind: PartKind, content: String) -> Vec<ProtocolEvent> {
        let mut output = Vec::new();
        if content.is_empty() {
            return output;
        }
        let index = if let Some((index, _)) = self.current.filter(|(_, previous)| *previous == kind)
        {
            index
        } else {
            self.close(&mut output);
            let index = self.next_index;
            self.next_index += 1;
            self.current = Some((index, kind));
            output.push(ProtocolEvent::Part(PartDelta::Begin { index, kind }));
            index
        };
        output.push(ProtocolEvent::Part(if kind == PartKind::Text {
            PartDelta::Text {
                index,
                delta: content,
            }
        } else {
            PartDelta::Reasoning {
                index,
                delta: content,
            }
        }));
        output
    }
}

pub fn script_stream(
    origin: ReplayOrigin,
    events: Vec<ScriptEvent>,
    control: StreamControl,
) -> LlmStream {
    let mut parser = ScriptParser::default();
    let input = stream::iter(events).flat_map(move |event| stream::iter(parser.parse(event)));
    assemble_reply_stream(Some(origin), Box::pin(input), control)
}

/// 把夹具的有序部件转成输入脚本；不允许注入累积器的完成结局。
pub fn parts_events(parts: Vec<MessagePart<UnboundToolCall>>) -> Vec<ScriptEvent> {
    let mut events = Vec::new();
    for (index, part) in parts.into_iter().enumerate() {
        match part.semantic {
            MessageSemantic::Text { text } => events.push(ScriptEvent::Text { content: text }),
            MessageSemantic::Reasoning { text } => {
                events.push(ScriptEvent::Reasoning { content: text })
            }
            MessageSemantic::ToolCall { call } => {
                events.push(ScriptEvent::ToolMetadata {
                    index,
                    name: call.name,
                    provider_call_id: call.provider_call_id,
                });
                events.push(ScriptEvent::ToolArguments {
                    index,
                    arguments_delta: lattice_conversation::canonical_json(&call.arguments),
                });
                events.push(ScriptEvent::ToolEnd { index });
            }
        }
    }
    events
}
