// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 检查 Markdown 中的相对链接：目标文件存在，指向 Markdown 标题的锚点存在。
//!
//! 只看 Git 跟踪的文件，本地未跟踪的文件不影响结论，所以本地与 CI 的结果一致。
//! 锚点按 GitHub 的规则由标题生成。读取文件与调用 git 放在 `run`，`check` 只做判断。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::Context as _;
use regex::Regex;

use crate::repo_root;

/// 一处失效的链接，`file` 为相对仓库根的路径。
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Problem {
    pub file: PathBuf,
    pub line: usize,
    pub kind: ProblemKind,
    pub target: String,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProblemKind {
    MissingFile,
    MissingAnchor,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.kind {
            ProblemKind::MissingFile => "目标不存在",
            ProblemKind::MissingAnchor => "锚点不存在",
        };
        write!(
            f,
            "{}:{}: {reason}：{}",
            self.file.display(),
            self.line,
            self.target
        )
    }
}

pub fn run() -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let tracked = tracked_files(&root)?;
    let markdown = read_markdown(&root, &tracked)?;
    let problems = check(&tracked, &markdown);
    if problems.is_empty() {
        println!("文档链接检查通过：{} 个 Markdown 文件", markdown.len());
        return Ok(());
    }
    for problem in &problems {
        println!("{problem}");
    }
    anyhow::bail!("文档链接检查失败：{} 处", problems.len())
}

fn tracked_files(root: &Path) -> anyhow::Result<BTreeSet<PathBuf>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z"])
        .output()
        .context("无法运行 git ls-files")?;
    if !output.status.success() {
        anyhow::bail!(
            "git ls-files 失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let listing = String::from_utf8(output.stdout).context("git ls-files 的输出不是 UTF-8")?;
    let mut files = BTreeSet::new();
    for path in listing.split('\0') {
        if !path.is_empty() {
            files.insert(PathBuf::from(path));
        }
    }
    Ok(files)
}

fn read_markdown(
    root: &Path,
    tracked: &BTreeSet<PathBuf>,
) -> anyhow::Result<BTreeMap<PathBuf, String>> {
    let mut markdown = BTreeMap::new();
    for file in tracked {
        if is_markdown(file) {
            let text = std::fs::read_to_string(root.join(file))
                .with_context(|| format!("读取 {} 失败", file.display()))?;
            markdown.insert(file.clone(), text);
        }
    }
    Ok(markdown)
}

/// 对 `markdown` 中的每个相对链接核对目标与锚点，返回排好序的问题列表。
pub fn check(tracked: &BTreeSet<PathBuf>, markdown: &BTreeMap<PathBuf, String>) -> Vec<Problem> {
    let directories = ancestor_directories(tracked);
    let mut anchors = HashMap::new();
    for (file, text) in markdown {
        anchors.insert(file.as_path(), heading_anchors(text));
    }

    let mut problems = Vec::new();
    for (file, text) in markdown {
        for (line, target) in links(text) {
            let Some(kind) = link_problem(file, &target, tracked, &directories, &anchors) else {
                continue;
            };
            problems.push(Problem {
                file: file.clone(),
                line,
                kind,
                target,
            });
        }
    }
    problems.sort();
    problems
}

fn link_problem(
    file: &Path,
    target: &str,
    tracked: &BTreeSet<PathBuf>,
    directories: &BTreeSet<PathBuf>,
    anchors: &HashMap<&Path, BTreeSet<String>>,
) -> Option<ProblemKind> {
    if URI_SCHEME.is_match(target) {
        return None;
    }
    let (path, fragment) = match target.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (target, None),
    };
    let destination = if path.is_empty() {
        file.to_path_buf()
    } else {
        match resolve(file, path) {
            Some(destination) => destination,
            None => return Some(ProblemKind::MissingFile),
        }
    };
    if !tracked.contains(&destination) && !directories.contains(&destination) {
        return Some(ProblemKind::MissingFile);
    }

    // 只核对指向 Markdown 文件的锚点；代码文件的行号锚点由 GitHub 页面解释。
    let fragment = fragment?;
    let headings = anchors.get(destination.as_path())?;
    if headings.contains(&fragment.to_lowercase()) {
        None
    } else {
        Some(ProblemKind::MissingAnchor)
    }
}

/// 按 `from` 所在目录解析相对路径，以 `/` 开头时从仓库根解析；越出仓库根时返回 `None`。
fn resolve(from: &Path, link: &str) -> Option<PathBuf> {
    let joined = match link.strip_prefix('/') {
        Some(from_root) => PathBuf::from(from_root),
        None => from.parent().unwrap_or(Path::new("")).join(link),
    };
    let mut parts = Vec::new();
    for component in joined.components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.iter().collect())
}

fn ancestor_directories(tracked: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    let mut directories = BTreeSet::new();
    for file in tracked {
        for ancestor in file.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            directories.insert(ancestor.to_path_buf());
        }
    }
    directories
}

/// 代码块之外的行，带 1 起的行号。代码块以 ``` 或 ~~~ 开始和结束。
fn prose_lines(text: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            lines.push((index + 1, line));
        }
    }
    lines
}

/// 正文中的链接与图片目标，行内代码中的内容不算链接。
fn links(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (line_number, line) in prose_lines(text) {
        let without_code = INLINE_CODE.replace_all(line, "");
        for captures in LINK.captures_iter(&without_code) {
            found.push((line_number, captures[1].to_owned()));
        }
    }
    found
}

/// 按 GitHub 的规则生成标题锚点；重复的标题依次加 `-1`、`-2`。
fn heading_anchors(text: &str) -> BTreeSet<String> {
    let mut anchors = BTreeSet::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (_, line) in prose_lines(text) {
        let Some(captures) = HEADING.captures(line) else {
            continue;
        };
        let base = slug(&captures[1]);
        let count = seen.entry(base.clone()).or_insert(0);
        let anchor = if *count == 0 {
            base
        } else {
            format!("{base}-{count}")
        };
        *count += 1;
        anchors.insert(anchor);
    }
    anchors
}

/// 去掉行内代码的反引号与链接的目标，转小写，删除字母、数字、空格、连字符与下划线以外的字符，空格换成连字符。
fn slug(title: &str) -> String {
    let text = INLINE_CODE_CONTENT.replace_all(title, "$1");
    let text = LINK_TEXT.replace_all(&text, "$1");
    let lowered = text.to_lowercase();
    NOT_SLUG_CHAR.replace_all(&lowered, "").replace(' ', "-")
}

fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "md")
}

static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)"#).expect("链接正则有效")
});
static HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ {0,3}#{1,6}\s+(.*?)(?:\s+#+)?\s*$").expect("标题正则有效"));
static INLINE_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`[^`]*`").expect("行内代码正则有效"));
static INLINE_CODE_CONTENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([^`]*)`").expect("行内代码内容正则有效"));
static LINK_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").expect("链接文字正则有效"));
static NOT_SLUG_CHAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{M}\p{N}\p{Pc} -]").expect("锚点字符正则有效"));
static URI_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").expect("URI 协议正则有效"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_follows_github_rules() {
        let cases = [
            ("会话的 profile 副本", "会话的-profile-副本"),
            ("`auth.json`", "authjson"),
            ("用量、缓存与费用", "用量缓存与费用"),
            ("Step 执行与结果保存", "step-执行与结果保存"),
            ("[链接](x.md) 标题", "链接-标题"),
            ("snake_case 名字", "snake_case-名字"),
        ];
        for (title, expected) in cases {
            assert_eq!(slug(title), expected, "标题：{title}");
        }
    }

    #[test]
    fn reports_missing_targets_and_anchors() {
        let tracked: BTreeSet<PathBuf> = [
            "README.md",
            "docs/guide.md",
            "docs/notes/a.md",
            "src/lib.rs",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let guide = "\
# 指南

## 重复
## 重复

[同文件](#指南) [第二个重复](#重复-1) [不存在的锚点](#没有)
[目录](notes/) [上级](../README.md#项目) [代码行号](../src/lib.rs#L3)
[外部](https://example.com/x) [邮件](mailto:a@b.c)
[缺文件](missing.md) [越出仓库](../../outside.md)
`[行内代码](ignored.md)`

```
[代码块](ignored.md)
```
";
        let markdown: BTreeMap<PathBuf, String> = [
            ("README.md", "# 项目\n\n[指南](docs/guide.md#重复)\n"),
            ("docs/guide.md", guide),
            ("docs/notes/a.md", "[根路径](/docs/guide.md#指南)\n"),
        ]
        .into_iter()
        .map(|(path, text)| (PathBuf::from(path), text.to_owned()))
        .collect();

        let reported: Vec<String> = check(&tracked, &markdown)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            reported,
            [
                "docs/guide.md:6: 锚点不存在：#没有",
                "docs/guide.md:9: 目标不存在：../../outside.md",
                "docs/guide.md:9: 目标不存在：missing.md",
            ],
            "只报出缺失的文件与锚点；外部链接、目录、行号锚点、行内代码与代码块不检查"
        );
    }
}
