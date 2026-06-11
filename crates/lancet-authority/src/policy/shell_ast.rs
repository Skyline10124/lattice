// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Static AST screening for inline shell command lines.
//!
//! When `CommandPolicy::allow_inline_shell` is enabled, an `exec.command_run`
//! invocation of the exact carrier form `["sh"|"bash", "-c", script]` is
//! expanded here into its constituent simple commands so that *each* of them
//! can be classified and capability-checked individually.
//!
//! Safety comes from a strict whitelist walk over the parsed AST, not from
//! the parser itself: only literal-word simple commands joined by `|`, `&&`,
//! `||`, and `;` are accepted. Every other construct — expansions, command
//! substitution, redirections, assignments, compound commands, background
//! execution, escapes — is rejected fail-closed with a precise reason. The
//! accepted commands are then re-emitted as a canonical script in which every
//! word is single-quoted, eliminating shell-dialect ambiguity at execution.
//!
//! brush-parser is pinned to an exact version in the workspace manifest; the
//! whitelist match arms below must be re-reviewed before any version bump.
//! Wildcard arms always reject, so unknown future AST variants fail closed.

use brush_parser::ast;
use brush_parser::word::{WordPiece, WordPieceWithSource};
use brush_parser::{Parser, ParserOptions};
use lancet_protocol::error::{CepErrorData, GuardErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};

use super::command::{command_matches_denied, CommandPolicy};

/// Hard ceilings for the profile-tunable inline-shell resource limits
/// (`CommandPolicy::inline_shell_max_*`). The profile may lower the limits
/// freely but cannot raise them past these — they bound parser work, audit
/// record size, and canonical-script growth regardless of configuration.
pub const SCRIPT_BYTES_CEILING: usize = 65_536;
pub const SUB_COMMANDS_CEILING: usize = 256;
pub const WORDS_PER_COMMAND_CEILING: usize = 1_024;

/// Effective per-call limits: profile values clamped to the hard ceilings.
#[derive(Clone, Copy, Debug)]
struct InlineShellLimits {
    script_bytes: usize,
    sub_commands: usize,
    words_per_command: usize,
}

impl InlineShellLimits {
    fn from_policy(policy: &CommandPolicy) -> Self {
        Self {
            script_bytes: policy
                .inline_shell_max_script_bytes
                .min(SCRIPT_BYTES_CEILING),
            sub_commands: policy.inline_shell_max_commands.min(SUB_COMMANDS_CEILING),
            words_per_command: policy
                .inline_shell_max_words_per_command
                .min(WORDS_PER_COMMAND_CEILING),
        }
    }
}

/// Carrier programs whose `-c` form is eligible for inline-shell expansion.
const SHELL_CARRIER_PROGRAMS: &[&str] = &[
    "sh",
    "bash",
    "/bin/sh",
    "/usr/bin/sh",
    "/bin/bash",
    "/usr/bin/bash",
];

/// First words that re-introduce dynamism a static walk cannot follow
/// (POSIX special builtins and shell keywords), rejected even when quoted.
const FIRST_WORD_DENYLIST: &[&str] = &[
    // Special builtins / escape hatches.
    "eval", "exec", "source", ".", "command", "builtin", "trap", "set", "unset", "export",
    "readonly", "alias", "unalias", "wait", "exit", "return", "break", "continue", ":", "times",
    "shift", "getopts", "local", "typeset", "declare", // Reserved words.
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "select", "time", "coproc", "{", "}", "!", "[[", "]]",
];

/// Connector joining two adjacent simple commands in an inline script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connector {
    /// `|` — stdout of the left command feeds stdin of the right.
    Pipe,
    /// `&&` — run right only if left succeeded.
    AndAnd,
    /// `||` — run right only if left failed.
    OrOr,
    /// `;` — run right unconditionally after left.
    Seq,
}

impl Connector {
    fn canonical_token(self) -> &'static str {
        match self {
            Self::Pipe => " | ",
            Self::AndAnd => " && ",
            Self::OrOr => " || ",
            Self::Seq => "; ",
        }
    }
}

/// A validated inline-shell expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellExpansion {
    /// The literal argv of each simple command, in execution order.
    pub sub_argvs: Vec<Vec<String>>,
    /// Connector between command `i` and command `i + 1`
    /// (`connectors.len() == sub_argvs.len() - 1`).
    pub connectors: Vec<Connector>,
    /// Canonical single-line re-emission with every word single-quoted.
    pub canonical_script: String,
}

/// An `exec.command_run` invocation after inline-shell resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandLine {
    /// Plain argv execution; classification applies to the argv as-is.
    Direct(Vec<String>),
    /// Validated inline-shell composite; classification applies per sub-command.
    Shell(ShellExpansion),
}

/// Detect and expand an inline-shell carrier invocation.
///
/// Returns `Direct` (and therefore preserves the existing classification
/// path, including risky-pattern hard-denial of `sh -c`) when:
/// - `allow_inline_shell` is disabled, or
/// - the argv is not the exact 3-element carrier form, or
/// - the carrier argv itself matches `denied_commands` (operator denial has
///   the highest precedence and must not be bypassed by expansion).
///
/// Returns an `InlineShellRejected` error when the argv is an eligible
/// carrier but the script falls outside the accepted static subset.
pub fn resolve_command_line(
    argv: &[String],
    policy: &CommandPolicy,
) -> Result<CommandLine, CepError> {
    if !policy.allow_inline_shell || !is_shell_carrier(argv) || command_matches_denied(argv, policy)
    {
        return Ok(CommandLine::Direct(argv.to_vec()));
    }
    expand_script(&argv[2], InlineShellLimits::from_policy(policy)).map(CommandLine::Shell)
}

fn is_shell_carrier(argv: &[String]) -> bool {
    argv.len() == 3 && SHELL_CARRIER_PROGRAMS.contains(&argv[0].as_str()) && argv[1] == "-c"
}

/// Parser options pinned to the strictest POSIX-compatible dialect:
/// `sh_mode` disables every bash extension gate in the grammar (`|&`,
/// `[[ ]]`, `time`), and tilde detection stays on so `~` surfaces as a
/// non-literal word piece instead of passing through as text.
fn parser_options() -> ParserOptions {
    ParserOptions {
        enable_extended_globbing: false,
        posix_mode: true,
        sh_mode: true,
        tilde_expansion_at_word_start: true,
        tilde_expansion_after_colon: false,
        parser_impl: Default::default(),
    }
}

fn expand_script(script: &str, limits: InlineShellLimits) -> Result<ShellExpansion, CepError> {
    if script.len() > limits.script_bytes {
        return Err(rejected(format!(
            "script is {} bytes, limit is {}",
            script.len(),
            limits.script_bytes
        )));
    }
    if script.trim().is_empty() {
        return Err(rejected("script is empty"));
    }
    if let Some(c) = script.chars().find(|c| c.is_control()) {
        return Err(rejected(format!(
            "control character {:?} (newlines and tabs are not allowed; use spaces)",
            c
        )));
    }

    let options = parser_options();
    let program = Parser::new(script.as_bytes(), &options)
        .parse_program()
        .map_err(|e| rejected(format!("parse error: {e}")))?;

    let mut walker = Walker {
        options: &options,
        limits,
        sub_argvs: Vec::new(),
        connectors: Vec::new(),
    };
    walker.walk_program(&program)?;

    if walker.sub_argvs.is_empty() {
        return Err(rejected("script contains no commands"));
    }
    debug_assert_eq!(walker.connectors.len() + 1, walker.sub_argvs.len());

    let canonical_script = canonicalize(&walker.sub_argvs, &walker.connectors);
    Ok(ShellExpansion {
        sub_argvs: walker.sub_argvs,
        connectors: walker.connectors,
        canonical_script,
    })
}

struct Walker<'a> {
    options: &'a ParserOptions,
    limits: InlineShellLimits,
    sub_argvs: Vec<Vec<String>>,
    connectors: Vec<Connector>,
}

impl Walker<'_> {
    fn walk_program(&mut self, program: &ast::Program) -> Result<(), CepError> {
        // A single control-character-free line parses to exactly one
        // complete command; anything else is unexpected — fail closed.
        match program.complete_commands.as_slice() {
            [list] => self.walk_compound_list(list),
            other => Err(rejected(format!(
                "expected a single statement list, found {}",
                other.len()
            ))),
        }
    }

    fn walk_compound_list(&mut self, list: &ast::CompoundList) -> Result<(), CepError> {
        for (index, ast::CompoundListItem(and_or, separator)) in list.0.iter().enumerate() {
            match separator {
                ast::SeparatorOperator::Sequence => {}
                ast::SeparatorOperator::Async => {
                    return Err(rejected("background execution '&'"));
                }
            }
            if index > 0 {
                self.push_connector(Connector::Seq)?;
            }
            self.walk_and_or_list(and_or)?;
        }
        Ok(())
    }

    fn walk_and_or_list(&mut self, list: &ast::AndOrList) -> Result<(), CepError> {
        self.walk_pipeline(&list.first)?;
        for item in &list.additional {
            match item {
                ast::AndOr::And(pipeline) => {
                    self.push_connector(Connector::AndAnd)?;
                    self.walk_pipeline(pipeline)?;
                }
                ast::AndOr::Or(pipeline) => {
                    self.push_connector(Connector::OrOr)?;
                    self.walk_pipeline(pipeline)?;
                }
            }
        }
        Ok(())
    }

    fn walk_pipeline(&mut self, pipeline: &ast::Pipeline) -> Result<(), CepError> {
        if pipeline.timed.is_some() {
            return Err(rejected("'time' pipeline prefix"));
        }
        if pipeline.bang {
            return Err(rejected("'!' pipeline negation"));
        }
        if pipeline.seq.is_empty() {
            return Err(rejected("empty pipeline"));
        }
        for (index, command) in pipeline.seq.iter().enumerate() {
            if index > 0 {
                self.push_connector(Connector::Pipe)?;
            }
            self.walk_command(command)?;
        }
        Ok(())
    }

    fn walk_command(&mut self, command: &ast::Command) -> Result<(), CepError> {
        match command {
            ast::Command::Simple(simple) => self.walk_simple_command(simple),
            ast::Command::Compound(_, _) => Err(rejected(
                "compound command (subshell, grouping, if/for/while/case, arithmetic)",
            )),
            ast::Command::Function(_) => Err(rejected("function definition")),
            ast::Command::ExtendedTest(_, _) => Err(rejected("extended test '[[ ]]'")),
            #[allow(unreachable_patterns)]
            _ => Err(rejected("unsupported command construct")),
        }
    }

    fn walk_simple_command(&mut self, simple: &ast::SimpleCommand) -> Result<(), CepError> {
        if self.sub_argvs.len() >= self.limits.sub_commands {
            return Err(rejected(format!(
                "more than {} commands in one script",
                self.limits.sub_commands
            )));
        }

        if let Some(item) = simple.prefix.as_ref().and_then(|prefix| prefix.0.first()) {
            return Err(rejected(describe_prefix_suffix_item(item)));
        }

        let name_word = simple
            .word_or_name
            .as_ref()
            .ok_or_else(|| rejected("command without a name"))?;

        let mut argv = vec![self.literal_word(name_word)?];

        if let Some(suffix) = &simple.suffix {
            for item in &suffix.0 {
                // After the command name, `a=b` is a plain word in POSIX;
                // brush still tags it AssignmentWord (for bash declaration
                // builtins, which the first-word denylist rejects anyway),
                // so route it through the same literal-word validation.
                let word = match item {
                    ast::CommandPrefixOrSuffixItem::Word(word) => word,
                    ast::CommandPrefixOrSuffixItem::AssignmentWord(_, word) => word,
                    other => return Err(rejected(describe_prefix_suffix_item(other))),
                };
                if argv.len() >= self.limits.words_per_command {
                    return Err(rejected(format!(
                        "more than {} words in one command",
                        self.limits.words_per_command
                    )));
                }
                argv.push(self.literal_word(word)?);
            }
        }

        let first = argv[0].as_str();
        if FIRST_WORD_DENYLIST.contains(&first) {
            return Err(rejected(format!(
                "shell builtin or keyword '{first}' as command name"
            )));
        }

        self.sub_argvs.push(argv);
        Ok(())
    }

    fn push_connector(&mut self, connector: Connector) -> Result<(), CepError> {
        if self.sub_argvs.is_empty() {
            return Err(rejected("connector without a preceding command"));
        }
        self.connectors.push(connector);
        Ok(())
    }

    /// Resolve a word to its literal string value, rejecting any piece that
    /// the shell would expand or reinterpret at execution time.
    fn literal_word(&self, word: &ast::Word) -> Result<String, CepError> {
        let pieces = brush_parser::word::parse(&word.value, self.options)
            .map_err(|e| rejected(format!("unparseable word '{}': {e}", word.value)))?;
        let mut value = String::new();
        for WordPieceWithSource { piece, .. } in &pieces {
            value.push_str(&self.literal_piece(piece, &word.value)?);
        }
        Ok(value)
    }

    fn literal_piece(&self, piece: &WordPiece, raw: &str) -> Result<String, CepError> {
        match piece {
            WordPiece::Text(text) => {
                if let Some(c) = text.chars().find(|c| matches!(c, '*' | '?' | '[' | ']')) {
                    return Err(rejected(format!(
                        "unquoted glob character '{c}' in word '{raw}' (quote it to pass literally)"
                    )));
                }
                Ok(text.clone())
            }
            WordPiece::SingleQuotedText(text) => Ok(text.clone()),
            WordPiece::EscapeSequence(sequence) => unescape(sequence, raw),
            WordPiece::DoubleQuotedSequence(inner) => {
                let mut value = String::new();
                for WordPieceWithSource { piece, .. } in inner {
                    match piece {
                        WordPiece::Text(text) => value.push_str(text),
                        WordPiece::EscapeSequence(sequence) => {
                            value.push_str(&unescape(sequence, raw)?);
                        }
                        other => return Err(rejected(describe_word_piece(other, raw))),
                    }
                }
                Ok(value)
            }
            other => Err(rejected(describe_word_piece(other, raw))),
        }
    }
}

/// Resolve a backslash escape to its literal character. The parser emits
/// escape pieces only where the backslash is semantically an escape (any
/// character unquoted; just `$`, `` ` ``, `"`, `\` inside double quotes), and
/// in both contexts POSIX defines the result as the literal next character.
fn unescape(sequence: &str, raw: &str) -> Result<String, CepError> {
    match sequence.strip_prefix('\\') {
        Some(rest) if !rest.is_empty() => Ok(rest.to_string()),
        _ => Err(rejected(format!(
            "malformed escape sequence {sequence:?} in word '{raw}'"
        ))),
    }
}

fn describe_prefix_suffix_item(item: &ast::CommandPrefixOrSuffixItem) -> String {
    match item {
        ast::CommandPrefixOrSuffixItem::IoRedirect(_) => "I/O redirection".to_string(),
        ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _) => {
            format!("environment assignment '{}'", assignment.name)
        }
        ast::CommandPrefixOrSuffixItem::Word(word) => {
            format!("unexpected word '{word}' before command name")
        }
        ast::CommandPrefixOrSuffixItem::ProcessSubstitution(_, _) => {
            "process substitution".to_string()
        }
        #[allow(unreachable_patterns)]
        _ => "unsupported command element".to_string(),
    }
}

fn describe_word_piece(piece: &WordPiece, raw: &str) -> String {
    let construct = match piece {
        WordPiece::ParameterExpansion(_) => "parameter expansion",
        WordPiece::CommandSubstitution(_) | WordPiece::BackquotedCommandSubstitution(_) => {
            "command substitution"
        }
        WordPiece::ArithmeticExpression(_) => "arithmetic expansion",
        WordPiece::TildeExpansion(_) => "tilde expansion",
        WordPiece::EscapeSequence(_) => "backslash escape",
        WordPiece::AnsiCQuotedText(_) => "ANSI-C quoting",
        WordPiece::GettextDoubleQuotedSequence(_) => "gettext quoting",
        WordPiece::Text(_)
        | WordPiece::SingleQuotedText(_)
        | WordPiece::DoubleQuotedSequence(_) => "unexpected nested quoting",
        #[allow(unreachable_patterns)]
        _ => "unsupported word construct",
    };
    format!("{construct} in word '{raw}'")
}

/// Re-emit the validated commands as a canonical single-line script in which
/// every word is single-quoted (`'` escaped as `'\''`). Any POSIX shell
/// parses the result identically, so the executed command set is exactly the
/// validated one.
fn canonicalize(sub_argvs: &[Vec<String>], connectors: &[Connector]) -> String {
    let mut script = String::new();
    for (index, argv) in sub_argvs.iter().enumerate() {
        if index > 0 {
            script.push_str(connectors[index - 1].canonical_token());
        }
        for (word_index, word) in argv.iter().enumerate() {
            if word_index > 0 {
                script.push(' ');
            }
            script.push('\'');
            script.push_str(&word.replace('\'', "'\\''"));
            script.push('\'');
        }
    }
    script
}

fn rejected(detail: impl std::fmt::Display) -> CepError {
    CepError::new(
        CepErrorCode::GuardDenied,
        format!(
            "inline shell rejected: {detail}. Only literal-word simple commands joined by \
             '|', '&&', '||', ';' are allowed; use plain argv for anything else."
        ),
        CepErrorData::Guard(GuardErrorData {
            method: "exec.command_run".into(),
            reason: format!("InlineShellRejected: {detail}"),
        }),
        Retriability::Permanent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn permissive_policy() -> CommandPolicy {
        let mut policy = CommandPolicy::defaults();
        policy.allow_inline_shell = true;
        policy.denied_commands = vec![vec!["rm".into()]];
        policy
    }

    fn carrier(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    fn expand(script: &str) -> Result<ShellExpansion, CepError> {
        match resolve_command_line(&carrier(script), &permissive_policy())? {
            CommandLine::Shell(expansion) => Ok(expansion),
            CommandLine::Direct(_) => panic!("expected Shell expansion for {script:?}"),
        }
    }

    fn reject_reason(script: &str) -> String {
        let err = expand(script).expect_err(&format!("expected rejection for {script:?}"));
        err.message.clone()
    }

    // ── Carrier detection and routing ───────────────────────────────────

    #[test]
    fn non_carrier_argv_stays_direct() {
        let policy = permissive_policy();
        let argv: Vec<String> = vec!["cargo".into(), "check".into()];
        assert_eq!(
            resolve_command_line(&argv, &policy).unwrap(),
            CommandLine::Direct(argv.clone())
        );
    }

    #[test]
    fn carrier_with_positional_args_stays_direct() {
        let policy = permissive_policy();
        let argv: Vec<String> = vec!["sh".into(), "-c".into(), "echo hi".into(), "arg0".into()];
        assert!(matches!(
            resolve_command_line(&argv, &policy).unwrap(),
            CommandLine::Direct(_)
        ));
    }

    #[test]
    fn disabled_inline_shell_stays_direct() {
        let mut policy = permissive_policy();
        policy.allow_inline_shell = false;
        assert!(matches!(
            resolve_command_line(&carrier("echo hi"), &policy).unwrap(),
            CommandLine::Direct(_)
        ));
    }

    #[test]
    fn denied_carrier_stays_direct_for_hard_denial() {
        // Operator denial of the carrier outranks expansion.
        let mut policy = permissive_policy();
        policy.denied_commands = vec![vec!["sh".into(), "-c".into()]];
        assert!(matches!(
            resolve_command_line(&carrier("echo hi"), &policy).unwrap(),
            CommandLine::Direct(_)
        ));
    }

    // ── Accepted subset ─────────────────────────────────────────────────

    #[test]
    fn single_command_expands() {
        let expansion = expand("echo hello").unwrap();
        assert_eq!(expansion.sub_argvs, vec![vec!["echo", "hello"]]);
        assert!(expansion.connectors.is_empty());
        assert_eq!(expansion.canonical_script, "'echo' 'hello'");
    }

    #[test]
    fn and_chain_expands() {
        let expansion = expand("cargo check && cargo test").unwrap();
        assert_eq!(
            expansion.sub_argvs,
            vec![vec!["cargo", "check"], vec!["cargo", "test"]]
        );
        assert_eq!(expansion.connectors, vec![Connector::AndAnd]);
        assert_eq!(
            expansion.canonical_script,
            "'cargo' 'check' && 'cargo' 'test'"
        );
    }

    #[test]
    fn pipeline_and_or_and_seq_expand() {
        let expansion = expand("echo a | wc -c; cargo fmt --check || echo failed").unwrap();
        assert_eq!(
            expansion.sub_argvs,
            vec![
                vec!["echo", "a"],
                vec!["wc", "-c"],
                vec!["cargo", "fmt", "--check"],
                vec!["echo", "failed"],
            ]
        );
        assert_eq!(
            expansion.connectors,
            vec![Connector::Pipe, Connector::Seq, Connector::OrOr]
        );
    }

    #[test]
    fn quoted_words_resolve_to_literals() {
        let expansion = expand(r#"grep 'a b' "c d" file.txt"#).unwrap();
        assert_eq!(
            expansion.sub_argvs,
            vec![vec!["grep", "a b", "c d", "file.txt"]]
        );
    }

    #[test]
    fn cjk_and_empty_string_words_pass() {
        let expansion = expand("echo '宽度测试' ''").unwrap();
        assert_eq!(expansion.sub_argvs, vec![vec!["echo", "宽度测试", ""]]);
    }

    #[test]
    fn single_quote_escaping_in_canonical_script() {
        let expansion = expand(r#"echo "it's""#).unwrap();
        assert_eq!(expansion.sub_argvs, vec![vec!["echo", "it's"]]);
        assert_eq!(expansion.canonical_script, r#"'echo' 'it'\''s'"#);
    }

    #[test]
    fn plain_equals_argument_is_not_an_assignment() {
        let expansion = expand("cargo build --features=full a=b").unwrap();
        assert_eq!(
            expansion.sub_argvs,
            vec![vec!["cargo", "build", "--features=full", "a=b"]]
        );
    }

    #[test]
    fn trailing_semicolon_is_accepted() {
        let expansion = expand("echo a;").unwrap();
        assert_eq!(expansion.sub_argvs, vec![vec!["echo", "a"]]);
        assert!(expansion.connectors.is_empty());
    }

    // ── Rejected constructs (adversarial corpus) ────────────────────────

    #[test]
    fn rejects_command_substitution() {
        assert!(reject_reason("echo $(rm -rf /)").contains("command substitution"));
        assert!(reject_reason("echo `id`").contains("command substitution"));
    }

    #[test]
    fn rejects_parameter_expansion() {
        assert!(reject_reason("echo $HOME").contains("parameter expansion"));
        assert!(reject_reason("echo ${PATH}").contains("parameter expansion"));
        assert!(reject_reason(r#"echo "$HOME""#).contains("parameter expansion"));
    }

    #[test]
    fn rejects_arithmetic_expansion() {
        assert!(reject_reason("echo $((1+1))").contains("rejected"));
    }

    #[test]
    fn rejects_redirections() {
        assert!(reject_reason("echo a > /tmp/x").contains("redirection"));
        assert!(reject_reason("wc -c < Cargo.toml").contains("redirection"));
        assert!(reject_reason("cargo check 2>&1").contains("redirection"));
    }

    #[test]
    fn rejects_background_and_double_semicolon() {
        assert!(reject_reason("sleep 5 &").contains("background"));
        let double_semi = expand("echo a ;; echo b");
        assert!(double_semi.is_err(), "';;' must not parse as a plain list");
    }

    #[test]
    fn rejects_env_assignment_prefix() {
        assert!(reject_reason("FOO=bar cargo check").contains("environment assignment"));
    }

    #[test]
    fn rejects_shell_builtin_escape_hatches() {
        assert!(reject_reason("eval echo hi").contains("'eval'"));
        assert!(reject_reason("'eval' echo hi").contains("'eval'"));
        assert!(reject_reason("exec cargo check").contains("'exec'"));
        assert!(reject_reason("set -x").contains("'set'"));
        assert!(reject_reason(". ./env.sh").contains("rejected"));
    }

    #[test]
    fn rejects_compound_commands_and_functions() {
        assert!(reject_reason("if true; then echo a; fi").contains("rejected"));
        assert!(reject_reason("(echo a)").contains("compound command"));
        assert!(reject_reason("{ echo a; }").contains("rejected"));
        assert!(reject_reason("f() { echo a; }").contains("rejected"));
        assert!(reject_reason("for x in a b; do echo $x; done").contains("rejected"));
    }

    #[test]
    fn rejects_unquoted_glob_and_tilde() {
        assert!(reject_reason("rm -rf *").contains("glob"));
        assert!(reject_reason("ls foo?").contains("glob"));
        assert!(reject_reason("ls [ab]").contains("glob"));
        assert!(reject_reason("ls ~/x").contains("tilde"));
    }

    #[test]
    fn quoted_glob_characters_are_literal_and_accepted() {
        let expansion = expand("grep '*' file.txt").unwrap();
        assert_eq!(expansion.sub_argvs, vec![vec!["grep", "*", "file.txt"]]);
    }

    #[test]
    fn backslash_escapes_resolve_to_literal_characters() {
        // POSIX: an unquoted backslash preserves the literal value of the
        // next character. Escaped glob characters are literal, not globs.
        let expansion = expand(r"echo a\ b \*").unwrap();
        assert_eq!(expansion.sub_argvs, vec![vec!["echo", "a b", "*"]]);
        let quoted = expand(r#"echo "a\$b""#).unwrap();
        assert_eq!(quoted.sub_argvs, vec![vec!["echo", "a$b"]]);
    }

    #[test]
    fn rejects_pipeline_negation() {
        assert!(reject_reason("! cargo check").contains("rejected"));
    }

    #[test]
    fn rejects_control_characters() {
        assert!(reject_reason("echo a\necho b").contains("control character"));
        assert!(reject_reason("echo\ta").contains("control character"));
    }

    #[test]
    fn rejects_unterminated_quotes() {
        assert!(expand("echo 'unterminated").is_err());
        assert!(expand(r#"echo "unterminated"#).is_err());
    }

    #[test]
    fn rejects_empty_script_and_size_limits() {
        let policy = permissive_policy();
        assert!(reject_reason("   ").contains("empty"));
        let huge = format!("echo {}", "a".repeat(policy.inline_shell_max_script_bytes));
        assert!(reject_reason(&huge).contains("bytes"));
        let many = vec!["echo a"; policy.inline_shell_max_commands + 1].join(" && ");
        assert!(reject_reason(&many).contains("commands in one script"));
        let mut wide = String::from("echo");
        for i in 0..policy.inline_shell_max_words_per_command {
            wide.push_str(&format!(" w{i}"));
        }
        assert!(reject_reason(&wide).contains("words in one command"));
    }

    #[test]
    fn limits_are_profile_tunable() {
        let mut policy = permissive_policy();
        policy.inline_shell_max_commands = 2;
        let argv = carrier("echo a && echo b && echo c");
        let err = resolve_command_line(&argv, &policy).unwrap_err();
        assert!(err.message.contains("more than 2 commands"));

        let ok = carrier("echo a && echo b");
        assert!(matches!(
            resolve_command_line(&ok, &policy).unwrap(),
            CommandLine::Shell(_)
        ));
    }

    #[test]
    fn limits_clamp_to_hard_ceilings() {
        let mut policy = permissive_policy();
        policy.inline_shell_max_script_bytes = usize::MAX;
        policy.inline_shell_max_commands = usize::MAX;
        policy.inline_shell_max_words_per_command = usize::MAX;
        let limits = InlineShellLimits::from_policy(&policy);
        assert_eq!(limits.script_bytes, SCRIPT_BYTES_CEILING);
        assert_eq!(limits.sub_commands, SUB_COMMANDS_CEILING);
        assert_eq!(limits.words_per_command, WORDS_PER_COMMAND_CEILING);

        let huge = format!("echo {}", "a".repeat(SCRIPT_BYTES_CEILING));
        let err = resolve_command_line(&carrier(&huge), &policy).unwrap_err();
        assert!(err.message.contains("bytes"));
    }

    #[test]
    fn nested_shell_carrier_expands_and_inner_script_is_one_word() {
        // The nested `sh -c '...'` stays a literal three-word command;
        // classify_command_line hard-denies it via the risky-pattern check.
        let expansion = expand("sh -c 'echo hi'").unwrap();
        assert_eq!(expansion.sub_argvs, vec![vec!["sh", "-c", "echo hi"]]);
    }

    // ── Canonicalization properties ─────────────────────────────────────

    #[test]
    fn canonical_script_round_trips() {
        for script in [
            "echo hello",
            "cargo check && cargo test",
            "echo a | wc -c; echo done || echo failed",
            r#"grep 'a b' "c d" file.txt"#,
            r#"echo "it's" 'CJK 宽度'"#,
        ] {
            let first = expand(script).unwrap();
            let second = expand(&first.canonical_script).unwrap();
            assert_eq!(first.sub_argvs, second.sub_argvs, "script: {script}");
            assert_eq!(first.connectors, second.connectors, "script: {script}");
        }
    }

    proptest! {
        /// Arbitrary input must never panic — it may only succeed or reject.
        #[test]
        fn arbitrary_input_never_panics(script in "\\PC*") {
            let _ = expand(&script);
        }

        /// Words that survive validation must round-trip through the
        /// canonical script byte-for-byte, including quotes and CJK.
        #[test]
        fn accepted_words_round_trip(words in proptest::collection::vec("[a-zA-Z0-9_./:='\" 宽度-]{1,12}", 1..5)) {
            let script = format!("echo {}", words.join(" "));
            if let Ok(first) = expand(&script) {
                let second = expand(&first.canonical_script).unwrap();
                prop_assert_eq!(first.sub_argvs, second.sub_argvs);
            }
        }
    }
}
