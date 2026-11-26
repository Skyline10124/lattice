// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_local::TurnId;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Fragment {
    pub(crate) turn: TurnId,
    pub(crate) step: u32,
    pub(crate) part: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Location {
    pub(crate) fragment: Fragment,
    pub(crate) range: Range<usize>,
}

/// 本地未发送评论草稿的语义载荷；`location` 仅用于屏内同位置挂载探测，不序列化上线。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftComment {
    pub(crate) location: Location,
    pub(crate) quote: String,
    pub(crate) text: String,
}

/// `tui/comment` 通道线上与历史块中的自包含评论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SentComment {
    pub(crate) quote: String,
    pub(crate) text: String,
}

/// 只由 History 中恰好一个文字片段构成的显示块携带身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryText {
    pub(crate) fragment: Fragment,
    pub(crate) text: std::sync::Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
    location: Location,
    source: std::sync::Arc<str>,
}

impl HistoryText {
    pub(crate) fn anchor(&self, range: Range<usize>) -> Option<Anchor> {
        self.text.get(range.clone())?;
        Some(Anchor {
            location: Location {
                fragment: self.fragment.clone(),
                range,
            },
            source: self.text.clone(),
        })
    }
}

impl Anchor {
    pub(crate) fn location(&self) -> &Location {
        &self.location
    }

    pub(crate) fn matches(&self, comment: &DraftComment) -> bool {
        self.location == comment.location
            && self.source.get(self.location.range.clone()) == Some(comment.quote.as_str())
    }

    pub(crate) fn into_draft(self) -> DraftComment {
        let quote = self
            .source
            .get(self.location.range.clone())
            .unwrap_or("")
            .to_owned();
        DraftComment {
            location: self.location,
            quote,
            text: String::new(),
        }
    }
}

fn encode_pair(quote: &str, text: &str) -> String {
    format!(
        "<lattice-comment>\n<quote>\n{}\n</quote>\n{}\n</lattice-comment>",
        escape(quote),
        escape(text)
    )
}

pub(crate) fn encode_sent_comments(comments: &[SentComment]) -> String {
    comments
        .iter()
        .map(|comment| encode_pair(&comment.quote, &comment.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn encode_draft_comments(comments: &[DraftComment]) -> String {
    comments
        .iter()
        .map(|comment| encode_pair(&comment.quote, &comment.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 严格解析 `tui/comment` 通道的自包含评论序列；任一结构损坏、空白批注或非规范转义均返回 `None`。
pub(crate) fn parse_sent_comments(source: &str) -> Option<Vec<SentComment>> {
    let mut rest = source;
    let mut comments = Vec::new();
    loop {
        let after = rest.strip_prefix("<lattice-comment>\n<quote>\n")?;
        let (quote, after) = section(after, "</quote>")?;
        let (text, after) = section(after.strip_prefix('\n')?, "</lattice-comment>")?;
        if text.trim().is_empty() {
            return None;
        }
        comments.push(SentComment { quote, text });
        if after.is_empty() {
            break;
        }
        rest = after.strip_prefix("\n\n")?;
    }
    if comments.is_empty() {
        return None;
    }
    // 拒绝多余属性、非规范转义与分隔。
    (encode_sent_comments(&comments) == source).then_some(comments)
}

/// 与 `lattice-context` 保持一致：先剥离行尾 `\r` 再判定保留闭合标签，转义与解转义均保留原 `\r`。
fn reserved(line: &str) -> bool {
    let bare = line.strip_suffix('\r').unwrap_or(line);
    matches!(
        bare.trim_start_matches('\\'),
        "</quote>" | "</lattice-comment>"
    )
}

fn escape(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            if reserved(line) {
                format!("\\{line}")
            } else {
                line.into()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn section<'a>(source: &'a str, end: &str) -> Option<(String, &'a str)> {
    let mut lines = Vec::new();
    let mut rest = source;
    loop {
        let (line, after) = rest.split_once('\n').map_or((rest, ""), |(line, _)| {
            (line, rest.get(line.len()..).unwrap_or(""))
        });
        if line == end {
            // 写出时补一个结构换行；split/join 保留载荷本身的末尾换行与行尾 \r。
            return Some((lines.join("\n"), after));
        }
        if reserved(line) && !line.starts_with('\\') {
            return None;
        }
        lines.push(if reserved(line) {
            line.strip_prefix('\\')?
        } else {
            line
        });
        rest = after.strip_prefix('\n')?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_contained_comments_round_trip_preserves_crlf_quotes_and_escapes() {
        for quote in [
            "",
            "原文",
            "原文\n\n末段\n",
            "首行\r\n次行\r\n",
            "</quote>\n\\</lattice-comment>\n",
            "</quote>\r\n\\</lattice-comment>\r\n尾行",
        ] {
            for body in [
                "第一段\n\n第二段",
                "含回车\r\n第二行",
                "</quote>\r\n\\</lattice-comment>\r\n末尾\r\n",
            ] {
                let single = vec![SentComment {
                    quote: quote.into(),
                    text: body.into(),
                }];
                let wire = encode_sent_comments(&single);
                assert!(
                    wire.starts_with("<lattice-comment>\n<quote>\n"),
                    "线格式不应携带旧外键属性：{wire}"
                );
                assert_eq!(
                    parse_sent_comments(&wire),
                    Some(single.clone()),
                    "单条自包含评论必须无损往返（含 CRLF）"
                );

                let multiple = vec![
                    single[0].clone(),
                    SentComment {
                        quote: "第二条引文\r\n".into(),
                        text: "第二条批注".into(),
                    },
                ];
                let multi_wire = encode_sent_comments(&multiple);
                assert_eq!(
                    parse_sent_comments(&multi_wire),
                    Some(multiple),
                    "多条自包含评论必须无损往返"
                );
            }
        }

        let wire = "<lattice-comment>\n<quote>\n原\n</quote>\n一\n\n二\n</lattice-comment>";
        assert_eq!(
            parse_sent_comments(wire)
                .map(|c| encode_sent_comments(&c))
                .as_deref(),
            Some(wire)
        );

        for bad in [
            "",
            "普通文本",
            "<lattice-comment n=\"1\">\n<quote>\n原\n</quote>\n一\n</lattice-comment>",
            "<lattice-comment>\r\n<quote>\n原\n</quote>\n一\n</lattice-comment>",
            "<lattice-comment>\n<quote> \n原\n</quote>\n一\n</lattice-comment>",
            "<lattice-comment>\n<quote>\n原\n</quote>\n \n\t\n</lattice-comment>",
            "<lattice-comment>\n<quote>\n</quote>\r\n</quote>\n一\n</lattice-comment>",
            "<lattice-comment>\n<quote>\n原\n</quote>\n</lattice-comment>\r\n</lattice-comment>",
            &format!("{wire}\n\n附加正文"),
            &format!("{wire}\n{wire}"),
        ] {
            assert_eq!(
                parse_sent_comments(bad),
                None,
                "非法或非规范评论线格式必须被拒绝：{bad:?}"
            );
        }
    }
}
