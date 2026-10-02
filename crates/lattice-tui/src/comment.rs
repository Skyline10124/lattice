// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_api::local::TurnId;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Comment {
    pub(crate) n: usize,
    pub(crate) location: Location,
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
    pub(crate) fn matches(&self, comment: &Comment) -> bool {
        self.location == comment.location
            && self.source.get(self.location.range.clone()) == Some(comment.quote.as_str())
    }
    pub(crate) fn into_comment(self, n: usize) -> Comment {
        let quote = self
            .source
            .get(self.location.range.clone())
            .unwrap_or("")
            .to_owned();
        Comment {
            n,
            location: self.location,
            quote,
            text: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Message {
    pub(crate) text: String,
    pub(crate) comments: Vec<Comment>,
}

impl Message {
    pub(crate) fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.comments.is_empty()
    }
    pub(crate) fn encode(&self) -> String {
        let mut parts: Vec<_> = self.comments.iter().map(|comment| {
            let Fragment { turn, step, part } = comment.location.fragment;
            let range = &comment.location.range;
            format!(
                "<lattice-comment n=\"{}\" turn=\"{turn}\" step=\"{step}\" part=\"{part}\" range=\"{}..{}\">\n<quote>\n{}\n</quote>\n{}\n</lattice-comment>",
                comment.n, range.start, range.end, escape(&comment.quote), escape(&comment.text)
            )
        }).collect();
        if !self.text.trim().is_empty() {
            parts.push(self.text.clone());
        }
        parts.join("\n\n")
    }

    /// 任一开头元素损坏时，调用方显示整条原文。
    pub(crate) fn parse(source: &str) -> Option<Self> {
        let mut rest = source;
        let mut comments = Vec::new();
        loop {
            let (header, after) = rest.split_once('\n')?;
            let attributes = header.strip_prefix("<lattice-comment n=\"")?;
            let (n, attributes) = attributes.split_once("\" turn=\"")?;
            let (turn, attributes) = attributes.split_once("\" step=\"")?;
            let (step, attributes) = attributes.split_once("\" part=\"")?;
            let (part, range) = attributes.split_once("\" range=\"")?;
            let (start, end) = range.strip_suffix("\">")?.split_once("..")?;
            let n = number(n)?;
            if n == 0 {
                return None;
            }
            let turn_id: TurnId = turn.parse().ok()?;
            if turn_id.to_string() != turn {
                return None;
            }
            let range = number(start)?..number(end)?;
            if range.start > range.end {
                return None;
            }
            let after = after.strip_prefix("<quote>\n")?;
            let (quote, after) = section(after, "</quote>")?;
            let (text, after) = section(after.strip_prefix('\n')?, "</lattice-comment>")?;
            if text.trim().is_empty() {
                return None;
            }
            comments.push(Comment {
                n,
                location: Location {
                    fragment: Fragment {
                        turn: turn_id,
                        step: number(step)?.try_into().ok()?,
                        part: number(part)?,
                    },
                    range,
                },
                quote,
                text,
            });
            if after.is_empty() {
                rest = "";
                break;
            }
            rest = after.strip_prefix("\n\n")?;
            if !rest.starts_with("<lattice-comment") {
                break;
            }
        }
        let parsed = Self {
            text: rest.into(),
            comments,
        };
        // 拒绝多余属性、非规范转义与分隔。
        (parsed.encode() == source).then_some(parsed)
    }
}

fn number(text: &str) -> Option<usize> {
    let value: usize = text.parse().ok()?;
    (value.to_string() == text).then_some(value)
}

fn reserved(line: &str) -> bool {
    matches!(
        line.trim_start_matches('\\'),
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
            // 写出时补一个结构换行；split/join 保留载荷本身的末尾换行。
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
    fn structured_comments_round_trip_preserves_paragraphs_quotes_and_escapes() {
        let turn = lattice_api::local::new_turn_id();
        for quote in ["原文", "原文\n\n末段\n", "</quote>\n\\</lattice-comment>\n"] {
            for (comment, body) in [
                ("第一段\n\n第二段", ""),
                ("第一段", "第二段"),
                (
                    "</quote>\n\\</lattice-comment>\n末尾\n",
                    "**正文**\n\n- 内容",
                ),
            ] {
                let message = Message {
                    text: body.into(),
                    comments: vec![Comment {
                        n: 1,
                        location: Location {
                            fragment: Fragment {
                                turn,
                                step: 3,
                                part: 0,
                            },
                            range: 0..quote.len(),
                        },
                        quote: quote.into(),
                        text: comment.into(),
                    }],
                };
                let wire = message.encode();
                assert!(wire.starts_with(&format!("<lattice-comment n=\"1\" turn=\"{turn}\" step=\"3\" part=\"0\" range=\"0..{}\">\n<quote>\n", quote.len())));
                assert_eq!(Message::parse(&wire), Some(message.clone()));
                let mut multiple = message.clone();
                let mut second = message.comments[0].clone();
                second.n = 2;
                multiple.comments.push(second);
                assert_eq!(Message::parse(&multiple.encode()), Some(multiple));
            }
        }
        let wire = format!(
            "<lattice-comment n=\"1\" turn=\"{turn}\" step=\"3\" part=\"0\" range=\"0..3\">\n<quote>\n原\n</quote>\n一\n\n二\n</lattice-comment>"
        );
        assert_eq!(Message::parse(&wire).unwrap().encode(), wire);
        for bad in [
            wire.replace("n=\"1\"", "n=\"01\""),
            wire.replace("step=\"3\" part=\"0\"", "part=\"0\" step=\"3\""),
            wire.replace("<quote>", "<quote> "),
            wire.replace("一\n\n二", " \n\t"),
            format!("{wire}\n\n<lattice-comment broken>"),
            format!("{wire}\nbody"),
            wire.replace("range=", "extra=\"x\" range="),
        ] {
            assert_eq!(Message::parse(&bad), None, "{bad}");
        }
        let mut whitespace = Message::parse(&wire).unwrap();
        whitespace.text = " \n\t".into();
        assert_eq!(whitespace.encode(), wire);
    }
}
