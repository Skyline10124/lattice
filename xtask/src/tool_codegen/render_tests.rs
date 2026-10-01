// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Format-level tests for the manifest `[render]` section.
//!
//! The fixtures deliberately use the **real** schema shapes `schemars` emits for
//! the five production Tools rather than idealized hand-written ones: an
//! `Option<T>` is `type: [T, "null"]`, an internally tagged union puts the
//! branch payload in a `$ref` **sibling** of `properties: {tag}`, an adjacently
//! tagged one puts it behind a `content` property, and `EditOperation`'s union
//! lives inside an array's `items`. A resolver validated only against tidy
//! fixtures would reject correct manifests for every one of those.

use super::*;

// ---------------------------------------------------------------------------
// Schema fixtures (verbatim shapes from `schemars::schema_for!`)
// ---------------------------------------------------------------------------

/// `ObserveWorkspaceParams`: `#[serde(tag = "mode")]`, so each branch is
/// `{properties: {mode: {const}}, $ref: <payload>}`.
fn observe_params() -> serde_json::Value {
    serde_json::json!({
      "$defs": {
        "ReadFileParams": {
          "type": "object",
          "required": ["path"],
          "properties": {
            "path": { "type": "string" },
            "offset": { "type": ["integer", "null"], "format": "uint64", "minimum": 0 },
            "line_numbers": { "type": ["boolean", "null"] }
          }
        },
        "GrepParams": {
          "type": "object",
          "required": ["pattern"],
          "properties": {
            "pattern": { "type": "string" },
            "path": { "type": ["string", "null"] }
          }
        }
      },
      "oneOf": [
        {
          "type": "object",
          "required": ["mode"],
          "properties": { "mode": { "const": "read_file", "type": "string" } },
          "$ref": "#/$defs/ReadFileParams"
        },
        {
          "type": "object",
          "required": ["mode"],
          "properties": { "mode": { "const": "grep", "type": "string" } },
          "$ref": "#/$defs/GrepParams"
        }
      ],
      "title": "ObserveWorkspaceParams"
    })
}

/// `ObserveWorkspaceResult`: `#[serde(tag = "mode", content = "result")]`, so
/// the payload sits behind a `result` property inside each branch.
fn observe_result() -> serde_json::Value {
    serde_json::json!({
      "$defs": {
        "DirectoryEntry": {
          "type": "object",
          "required": ["name", "kind"],
          "properties": {
            "name": { "type": "string" },
            "kind": { "$ref": "#/$defs/DirectoryEntryKind" }
          }
        },
        "DirectoryEntryKind": {
          "type": "string",
          "enum": ["file", "directory", "symlink", "other"]
        },
        "GrepMatch": {
          "type": "object",
          "required": ["path", "line", "text"],
          "properties": {
            "path": { "type": "string" },
            "line": { "type": "integer", "format": "uint64", "minimum": 0 },
            "text": { "type": "string" }
          }
        },
        "GrepResult": {
          "type": "object",
          "required": ["matches"],
          "properties": {
            "matches": { "type": "array", "items": { "$ref": "#/$defs/GrepMatch" } },
            "truncated": { "type": ["boolean", "null"] }
          }
        },
        "ReadFileResult": {
          "type": "object",
          "required": ["content"],
          "properties": {
            "content": { "type": "string" },
            "content_hash": { "type": ["string", "null"] }
          }
        },
        "FindFilesResult": {
          "type": "object",
          "required": ["paths"],
          "properties": {
            "paths": { "type": "array", "items": { "type": "string" } },
            "truncated": { "type": ["boolean", "null"] }
          }
        },
        "ListDirectoryResult": {
          "type": "object",
          "required": ["entries"],
          "properties": {
            "entries": { "type": "array", "items": { "$ref": "#/$defs/DirectoryEntry" } },
            "truncated": { "type": ["boolean", "null"] }
          }
        }
      },
      "oneOf": [
        {
          "type": "object",
          "required": ["mode", "result"],
          "properties": {
            "mode": { "const": "read_file", "type": "string" },
            "result": { "$ref": "#/$defs/ReadFileResult" }
          }
        },
        {
          "type": "object",
          "required": ["mode", "result"],
          "properties": {
            "mode": { "const": "grep", "type": "string" },
            "result": { "$ref": "#/$defs/GrepResult" }
          }
        },
        {
          "type": "object",
          "required": ["mode", "result"],
          "properties": {
            "mode": { "const": "list_directory", "type": "string" },
            "result": { "$ref": "#/$defs/ListDirectoryResult" }
          }
        },
        {
          "type": "object",
          "required": ["mode", "result"],
          "properties": {
            "mode": { "const": "find_files", "type": "string" },
            "result": { "$ref": "#/$defs/FindFilesResult" }
          }
        }
      ],
      "title": "ObserveWorkspaceResult"
    })
}

/// `EditParams`: a `oneOf` union nested inside `edits`' array `items`.
fn edit_params() -> serde_json::Value {
    serde_json::json!({
      "$defs": {
        "EditOperation": {
          "oneOf": [
            {
              "type": "object",
              "required": ["op", "old", "new"],
              "properties": {
                "op": { "const": "replace_text", "type": "string" },
                "old": { "type": "string" },
                "new": { "type": "string" },
                "occurrence": { "type": ["integer", "null"], "format": "uint64" }
              }
            },
            {
              "type": "object",
              "required": ["op", "anchor", "content"],
              "properties": {
                "op": { "const": "insert_before", "type": "string" },
                "anchor": { "type": "string" },
                "content": { "type": "string" }
              }
            }
          ]
        }
      },
      "type": "object",
      "required": ["path", "expected_hash", "edits"],
      "properties": {
        "path": { "type": "string" },
        "expected_hash": { "type": "string" },
        "edits": { "type": "array", "items": { "$ref": "#/$defs/EditOperation" } }
      },
      "title": "EditParams"
    })
}

/// `FetchResult`: flat, with two `Option` fields.
fn fetch_result() -> serde_json::Value {
    serde_json::json!({
      "type": "object",
      "required": ["status", "body"],
      "properties": {
        "status": { "type": "integer", "format": "uint16", "minimum": 0, "maximum": 65535 },
        "body": { "type": "string" },
        "content_type": { "type": ["string", "null"] },
        "truncated": { "type": ["boolean", "null"] }
      },
      "title": "FetchResult"
    })
}

/// `TodoWriteResult`: array of objects whose `status` is a `$ref` to an enum.
fn todo_result() -> serde_json::Value {
    serde_json::json!({
      "$defs": {
        "TodoItem": {
          "type": "object",
          "required": ["content", "status"],
          "properties": {
            "content": { "type": "string" },
            "status": { "$ref": "#/$defs/TodoStatus" }
          }
        },
        "TodoStatus": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
      },
      "type": "object",
      "required": ["todos"],
      "properties": {
        "todos": { "type": "array", "items": { "$ref": "#/$defs/TodoItem" } }
      },
      "title": "TodoWriteResult"
    })
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({ "type": "object" })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_section(toml_src: &str) -> Result<RenderSection, toml::de::Error> {
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wrapper {
        render: RenderSection,
    }
    toml::from_str::<Wrapper>(toml_src).map(|w| w.render)
}

fn check(
    toml_src: &str,
    params: &serde_json::Value,
    result: &serde_json::Value,
) -> anyhow::Result<()> {
    let section = parse_section(toml_src).map_err(|e| anyhow!("{e}"))?;
    validate_render(&section, &RenderSchemas { params, result })
}

fn check_err(toml_src: &str, params: &serde_json::Value, result: &serde_json::Value) -> String {
    check(toml_src, params, result)
        .expect_err("expected a validation error")
        .to_string()
}

// ---------------------------------------------------------------------------
// Strictness: the manifest surface fails closed
// ---------------------------------------------------------------------------

/// A misspelled role must be an `unknown variant` error. With `role: String`
/// plus optional per-role fields this parses fine and reaches codegen as a
/// "valid" annotation nobody can render.
#[test]
fn misspelled_role_is_rejected_not_silently_accepted() {
    let err = parse_section(
        r#"
[[render.annotations]]
role = "summry"
path = "result.echoed"
"#,
    )
    .expect_err("a misspelled role must fail closed");
    let msg = err.to_string();
    assert!(msg.contains("summry"), "error must name the typo: {msg}");
    assert!(
        msg.contains("summary"),
        "error should list the valid roles: {msg}"
    );
}

/// A key belonging to another role must be an `unknown field` error, not a
/// silently dropped value.
#[test]
fn cross_role_key_is_rejected() {
    let err = parse_section(
        r#"
[[render.annotations]]
role = "code"
path = "result.echoed"
columns = ["a"]
"#,
    )
    .expect_err("a key from another role must fail closed");
    assert!(
        err.to_string().contains("columns"),
        "error must name the offending key: {err}"
    );
}

/// A role's required field must be missing-field, not `None`.
#[test]
fn missing_role_field_is_rejected() {
    for (src, missing) in [
        (
            r#"
[[render.annotations]]
role = "diff"
before = "params.old"
"#,
            "after",
        ),
        (
            r#"
[[render.annotations]]
role = "summary"
"#,
            "path",
        ),
        (
            r#"
[[render.annotations]]
role = "columns"
path = "result.matches[]"
"#,
            "columns",
        ),
    ] {
        let err = parse_section(src).expect_err("a missing required field must fail closed");
        assert!(
            err.to_string().contains(missing),
            "error must name `{missing}`: {err}"
        );
    }
}

/// A misspelled inner table name must not deserialize into an empty list. This
/// is the `[[capabilties]]` fail-open shape one nesting level down.
#[test]
fn misspelled_inner_tables_are_rejected_not_silently_empty() {
    for typo in [
        r#"
[[render.annotation]]
role = "summary"
path = "result.echoed"
"#,
        r#"
[[render.arm]]
at = "result"
case = "read_file"
"#,
        r#"
[[render.arms]]
at = "result"
case = "read_file"
[[render.arms.annotation]]
role = "summary"
path = "result.result.content"
"#,
    ] {
        assert!(
            parse_section(typo).is_err(),
            "a misspelled inner table must fail closed: {typo}"
        );
    }
}

/// Single-slot roles may be declared once per scope, and not in two scopes at
/// once: the consumer has one summary/status/truncated slot to fill.
#[test]
fn duplicate_single_slot_roles_are_rejected() {
    let err = check_err(
        r#"
[[render.annotations]]
role = "summary"
path = "result.body"
[[render.annotations]]
role = "summary"
path = "result.content_type"
"#,
        &empty_object(),
        &fetch_result(),
    );
    assert!(err.contains("summary"), "{err}");
    assert!(err.contains("single slot"), "{err}");

    // The top-level declaration here binds a params field, so it is the scope
    // conflict on the *same side* that is rejected — not an incidental union
    // traversal.
    let err = check_err(
        r#"
[[render.annotations]]
role = "summary"
path = "params.expected_hash"
[[render.arms]]
at = "params.edits[]"
case = "replace_text"
[[render.arms.annotations]]
role = "summary"
path = "params.edits[].old"
"#,
        &edit_params(),
        &empty_object(),
    );
    assert!(
        err.contains("top level"),
        "a single-slot role in two scopes must be rejected: {err}"
    );
}

/// The params and result sides are separate slots. #116 renders an error card
/// from `params.*` alone, so a flat-schema Tool must be able to name both its
/// request and its response without one displacing the other.
#[test]
fn single_slot_roles_are_counted_per_side() {
    check(
        r#"
[[render.annotations]]
role = "summary"
path = "params.expected_hash"
[[render.annotations]]
role = "summary"
path = "result.body"
"#,
        &edit_params(),
        &fetch_result(),
    )
    .expect("one params summary and one result summary are two slots, not a conflict");

    // But two on the same side still conflict.
    let err = check_err(
        r#"
[[render.annotations]]
role = "summary"
path = "params.path"
[[render.annotations]]
role = "summary"
path = "params.expected_hash"
"#,
        &edit_params(),
        &empty_object(),
    );
    assert!(err.contains("params"), "{err}");
    assert!(err.contains("single slot"), "{err}");
}

/// Repeatable roles are genuinely repeatable: a Tool with two code fields is a
/// real shape, not a conflict.
#[test]
fn repeatable_roles_may_appear_twice() {
    check(
        r#"
[[render.annotations]]
role = "code"
path = "result.body"
[[render.annotations]]
role = "code"
path = "result.content_type"
"#,
        &empty_object(),
        &fetch_result(),
    )
    .expect("two `code` annotations are legal");
}

// ---------------------------------------------------------------------------
// Path grammar
// ---------------------------------------------------------------------------

/// Malformed path strings fail as grammar errors at parse time, so an author
/// sees "this string is not a path" rather than a puzzling "dangling path".
#[test]
fn malformed_paths_are_grammar_errors() {
    for bad in [
        "",
        ".",
        "echoed",
        "results.echoed",
        "paramsx.y",
        "Result.echoed",
        "result.",
        "result..echoed",
        "result.[]",
        "[]",
        "result.a[][]",
        "result.a[]b",
        "result. echoed",
        " result.echoed",
        "result.echoed ",
    ] {
        let src = format!(
            r#"
[[render.annotations]]
role = "summary"
path = "{bad}"
"#
        );
        assert!(
            parse_section(&src).is_err(),
            "path {bad:?} must be rejected by the grammar"
        );
    }
}

/// A bare `params` / `result` is the schema root, which is how a root union is
/// named — but it is not a field, so it cannot carry a field annotation.
#[test]
fn bare_root_is_a_union_target_not_a_field() {
    check(
        r#"
[[render.arms]]
at = "result"
case = "grep"
[[render.arms.annotations]]
role = "truncated"
path = "result.result.truncated"
"#,
        &observe_params(),
        &observe_result(),
    )
    .expect("`at = \"result\"` names the root union");

    let err = check_err(
        r#"
[[render.annotations]]
role = "summary"
path = "result"
"#,
        &empty_object(),
        &fetch_result(),
    );
    assert!(
        err.contains("no declared") || err.contains("must resolve"),
        "the schema root is not a scalar field: {err}"
    );
}

// ---------------------------------------------------------------------------
// Resolution against the real schemars shapes
// ---------------------------------------------------------------------------

/// A dangling path is a codegen error that names the offending segment and
/// lists what is actually there (AC 4).
#[test]
fn dangling_path_is_a_codegen_error() {
    let err = check_err(
        r#"
[[render.annotations]]
role = "summary"
path = "result.ghost"
"#,
        &empty_object(),
        &fetch_result(),
    );
    assert!(err.contains("dangling"), "{err}");
    assert!(err.contains("ghost"), "{err}");
    assert!(
        err.contains("body") && err.contains("status"),
        "error should list the known properties: {err}"
    );
}

/// `Option<T>` must be annotatable. `truncated` is the field the role is named
/// after and its real schema type is `["boolean", "null"]`; a single-valued
/// type predicate would reject every optional field in the catalog.
#[test]
fn optional_fields_are_annotatable() {
    check(
        r#"
[[render.annotations]]
role = "truncated"
path = "result.truncated"
[[render.annotations]]
role = "code"
path = "result.content_type"
"#,
        &empty_object(),
        &fetch_result(),
    )
    .expect("`Option<bool>` is a boolean and `Option<String>` is a string");
}

/// An internally tagged union puts the branch payload in a `$ref` **sibling**
/// of `properties: {tag}`. Without merging siblings, no params field of
/// `observe.workspace` is reachable at all.
#[test]
fn internally_tagged_branch_payload_is_reachable() {
    check(
        r#"
[[render.arms]]
at = "params"
case = "read_file"
[[render.arms.annotations]]
role = "summary"
path = "params.path"
"#,
        &observe_params(),
        &observe_result(),
    )
    .expect("a `$ref` sibling of `properties` must be merged, not ignored");
}

/// `status` declares "this field is the call's lifecycle status", not "this
/// field is an enum". `fetch.web`'s HTTP code is an open integer and is the most
/// natural status that call has; requiring a closed value set would leave it
/// with no spelling at all. A closed set stays the common case and is what lets
/// a consumer dispatch exhaustively.
#[test]
fn status_accepts_an_open_scalar_and_a_reffed_enum() {
    check(
        r#"
[[render.annotations]]
role = "status"
path = "result.status"
"#,
        &empty_object(),
        &fetch_result(),
    )
    .expect("an HTTP status code is a status");

    check(
        r#"
[[render.annotations]]
role = "status"
path = "result.todos[].status"
"#,
        &empty_object(),
        &todo_result(),
    )
    .expect("a `$ref`ed enum is a status");
}

/// A `$ref`ed enum resolves through for `status`, and a `const` tag counts as a
/// closed value set too.
#[test]
fn status_accepts_a_reffed_enum_and_a_const_tag() {
    check(
        r#"
[[render.annotations]]
role = "columns"
path = "result.todos[]"
columns = ["content", "status"]
[[render.arms]]
at = "result"
case = "read_file"
[[render.arms.annotations]]
role = "status"
path = "result.mode"
"#,
        &empty_object(),
        &todo_result(),
    )
    .expect_err("the arm targets a union the todo result does not have");

    check(
        r#"
[[render.annotations]]
role = "columns"
path = "result.todos[]"
columns = ["content", "status"]
"#,
        &empty_object(),
        &todo_result(),
    )
    .expect("columns over `$ref`ed items resolve");

    check(
        r#"
[[render.arms]]
at = "result"
case = "list_directory"
[[render.arms.annotations]]
role = "columns"
path = "result.result.entries[]"
columns = ["name", "kind"]
[[render.arms.annotations]]
role = "truncated"
path = "result.result.truncated"
"#,
        &observe_params(),
        &observe_result(),
    )
    .expect("nested `$ref` items and an optional bool resolve inside an arm");
}

/// Walking *into* a union without an arm is an error that names the arm syntax:
/// silently picking a branch would make the annotation mean something the author
/// never wrote.
#[test]
fn union_without_an_arm_points_at_the_arm_syntax() {
    let err = check_err(
        r#"
[[render.annotations]]
role = "code"
path = "result.result.content"
"#,
        &observe_params(),
        &observe_result(),
    );
    assert!(err.contains("discriminated union"), "{err}");
    assert!(
        err.contains("[[render.arms]]") && err.contains("at = \"result\""),
        "error must tell the author what to write: {err}"
    );
}

/// A union nested inside an array element is addressable, which is what
/// `mutate.edit`'s `diff` needs: the two sides live in a `replace_text` element
/// of `params.edits`.
#[test]
fn nested_union_inside_array_items_is_addressable() {
    check(
        r#"
[[render.arms]]
at = "params.edits[]"
case = "replace_text"
[[render.arms.annotations]]
role = "diff"
before = "params.edits[].old"
after = "params.edits[].new"
"#,
        &edit_params(),
        &empty_object(),
    )
    .expect("a per-element union arm must be expressible");

    // The other branch has different fields, so the same paths must not resolve
    // there — otherwise arm selection would be decorative.
    let err = check_err(
        r#"
[[render.arms]]
at = "params.edits[]"
case = "insert_before"
[[render.arms.annotations]]
role = "diff"
before = "params.edits[].old"
after = "params.edits[].new"
"#,
        &edit_params(),
        &empty_object(),
    );
    assert!(
        err.contains("dangling") && err.contains("old"),
        "arm selection must actually narrow the schema: {err}"
    );
}

/// An unknown case is an error listing the valid ones.
#[test]
fn unknown_arm_case_lists_the_valid_cases() {
    let err = check_err(
        r#"
[[render.arms]]
at = "result"
case = "read_flie"
[[render.arms.annotations]]
role = "code"
path = "result.result.content"
"#,
        &observe_params(),
        &observe_result(),
    );
    assert!(err.contains("read_flie"), "{err}");
    assert!(
        err.contains("read_file") && err.contains("grep"),
        "error must list valid cases: {err}"
    );
}

/// Two arms for the same (union, case) pair would produce two identically keyed
/// sections in `render.json`.
#[test]
fn duplicate_arms_are_rejected() {
    let err = check_err(
        r#"
[[render.arms]]
at = "result"
case = "read_file"
[[render.arms.annotations]]
role = "code"
path = "result.result.content"
[[render.arms]]
at = "result"
case = "read_file"
[[render.arms.annotations]]
role = "summary"
path = "result.result.content_hash"
"#,
        &observe_params(),
        &observe_result(),
    );
    assert!(err.contains("two"), "{err}");
    assert!(err.contains("read_file"), "{err}");
}

/// An arm on a node that is not a union is an error, not a silent no-op.
#[test]
fn arm_on_a_non_union_is_rejected() {
    let err = check_err(
        r#"
[[render.arms]]
at = "result.body"
case = "x"
[[render.arms.annotations]]
role = "code"
path = "result.body"
"#,
        &empty_object(),
        &fetch_result(),
    );
    assert!(err.contains("does not name a discriminated union"), "{err}");
}

/// Inside an arm, a path must be written in full and start with the arm's `at`,
/// so the same string always means the same field.
#[test]
fn arm_annotation_paths_must_be_anchored_at_the_arm() {
    let err = check_err(
        r#"
[[render.arms]]
at = "params.edits[]"
case = "replace_text"
[[render.arms.annotations]]
role = "code"
path = "params.old"
"#,
        &edit_params(),
        &empty_object(),
    );
    assert!(
        err.contains("must start with"),
        "an unanchored arm path must be rejected: {err}"
    );
}

/// A `params.*` annotation stays usable inside a `result` arm: #116 applies
/// params annotations even on the error card, where every `result.*` path is
/// dangling by construction.
#[test]
fn params_annotations_resolve_inside_a_result_arm() {
    check(
        r#"
[[render.arms]]
at = "result"
case = "grep"
[[render.arms.annotations]]
role = "columns"
path = "result.result.matches[]"
columns = ["path", "line", "text"]
[[render.arms.annotations]]
role = "summary"
path = "params.pattern"
"#,
        &observe_params(),
        &observe_result(),
    )
    .expect_err("`params.pattern` is behind the params union, so it needs its own arm");

    check(
        r#"
[[render.arms]]
at = "result"
case = "grep"
[[render.arms.annotations]]
role = "columns"
path = "result.result.matches[]"
columns = ["path", "line", "text"]
[[render.arms.annotations]]
role = "summary"
path = "params.expected_hash"
"#,
        &edit_params(),
        &observe_result(),
    )
    .expect("a params path resolves from the params root inside a result arm");
}

// ---------------------------------------------------------------------------
// Role predicates
// ---------------------------------------------------------------------------

#[test]
fn role_predicates_reject_the_wrong_shape() {
    // `code` wants a string, `status` is not a string field.
    let err = check_err(
        r#"
[[render.annotations]]
role = "code"
path = "result.status"
"#,
        &empty_object(),
        &fetch_result(),
    );
    assert!(err.contains("must resolve to a string"), "{err}");

    // `truncated` wants a boolean.
    let err = check_err(
        r#"
[[render.annotations]]
role = "truncated"
path = "result.body"
"#,
        &empty_object(),
        &fetch_result(),
    );
    assert!(err.contains("must resolve to a boolean"), "{err}");

    // `summary` wants a scalar, not an array.
    let err = check_err(
        r#"
[[render.annotations]]
role = "summary"
path = "result.todos"
"#,
        &empty_object(),
        &todo_result(),
    );
    assert!(err.contains("must resolve to a scalar"), "{err}");

    // `status` wants one value: an array is not a status.
    let err = check_err(
        r#"
[[render.annotations]]
role = "status"
path = "result.todos"
"#,
        &empty_object(),
        &todo_result(),
    );
    assert!(err.contains("must resolve to a scalar"), "{err}");

    // A `status` over a `$ref`ed enum is fine.
    check(
        r#"
[[render.arms]]
at = "result"
case = "list_directory"
[[render.arms.annotations]]
role = "status"
path = "result.mode"
"#,
        &observe_params(),
        &observe_result(),
    )
    .expect("a `const` tag is a closed value set");
}

/// `columns` and `tree` share one spelling: the last segment carries `[]` and
/// the predicate applies to the element.
#[test]
fn elementwise_roles_require_the_bracket_suffix() {
    let err = check_err(
        r#"
[[render.annotations]]
role = "columns"
path = "result.todos"
columns = ["content"]
"#,
        &empty_object(),
        &todo_result(),
    );
    assert!(err.contains("element-wise"), "{err}");

    let err = check_err(
        r#"
[[render.arms]]
at = "result"
case = "find_files"
[[render.arms.annotations]]
role = "tree"
path = "result.result.paths"
"#,
        &observe_params(),
        &observe_result(),
    );
    assert!(err.contains("element-wise"), "{err}");

    check(
        r#"
[[render.arms]]
at = "result"
case = "find_files"
[[render.arms.annotations]]
role = "tree"
path = "result.result.paths[]"
[[render.arms.annotations]]
role = "truncated"
path = "result.result.truncated"
"#,
        &observe_params(),
        &observe_result(),
    )
    .expect("`tree` over an array of strings resolves");
}

/// `[]` on a non-array is an error naming the segment.
#[test]
fn elementwise_on_a_non_array_is_rejected() {
    let err = check_err(
        r#"
[[render.annotations]]
role = "tree"
path = "result.body[]"
"#,
        &empty_object(),
        &fetch_result(),
    );
    assert!(
        err.contains("not \nan array") || err.contains("not an array"),
        "{err}"
    );
    assert!(err.contains("body"), "{err}");
}

/// `tree` wants string elements; an array of objects is a `columns` shape.
#[test]
fn tree_rejects_object_elements() {
    let err = check_err(
        r#"
[[render.annotations]]
role = "tree"
path = "result.todos[]"
"#,
        &empty_object(),
        &todo_result(),
    );
    assert!(err.contains("must resolve to a string"), "{err}");
}

#[test]
fn columns_rejects_unknown_empty_and_duplicate_columns() {
    let err = check_err(
        r#"
[[render.annotations]]
role = "columns"
path = "result.todos[]"
columns = ["content", "ghost"]
"#,
        &empty_object(),
        &todo_result(),
    );
    assert!(err.contains("ghost"), "{err}");
    assert!(err.contains("not a property"), "{err}");

    let err = check_err(
        r#"
[[render.annotations]]
role = "columns"
path = "result.todos[]"
columns = []
"#,
        &empty_object(),
        &todo_result(),
    );
    assert!(err.contains("empty"), "{err}");

    let err = check_err(
        r#"
[[render.annotations]]
role = "columns"
path = "result.todos[]"
columns = ["content", "content"]
"#,
        &empty_object(),
        &todo_result(),
    );
    assert!(err.contains("twice"), "{err}");
}

/// `columns` over an array of scalars is a `tree`/single-column shape, not a
/// table with named columns.
#[test]
fn columns_rejects_scalar_elements() {
    let err = check_err(
        r#"
[[render.arms]]
at = "result"
case = "find_files"
[[render.arms.annotations]]
role = "columns"
path = "result.result.paths[]"
columns = ["path"]
"#,
        &observe_params(),
        &observe_result(),
    );
    assert!(err.contains("element-wise to objects"), "{err}");
}

/// `diff` checks both sides, and says which one failed.
#[test]
fn diff_checks_both_sides() {
    let err = check_err(
        r#"
[[render.annotations]]
role = "diff"
before = "params.expected_hash"
after = "params.edits"
"#,
        &edit_params(),
        &empty_object(),
    );
    assert!(
        err.contains("after"),
        "error must name the failing side: {err}"
    );
}

// ---------------------------------------------------------------------------
// Unsupported schema constructs fail closed
// ---------------------------------------------------------------------------

/// An unresolvable `$ref` is an error, not a silent dangling path.
#[test]
fn unsupported_or_undefined_refs_fail_closed() {
    for (schema, needle) in [
        (
            serde_json::json!({
              "type": "object",
              "properties": { "x": { "$ref": "https://example.test/schema#/X" } }
            }),
            "unsupported $ref",
        ),
        (
            serde_json::json!({
              "type": "object",
              "properties": { "x": { "$ref": "#/$defs/Missing" } }
            }),
            "not defined",
        ),
        (
            serde_json::json!({
              "type": "object",
              "properties": { "x": { "$ref": "#/$defs/A/properties/b" } }
            }),
            "nested $ref",
        ),
    ] {
        let err = check_err(
            r#"
[[render.annotations]]
role = "code"
path = "result.x"
"#,
            &empty_object(),
            &schema,
        );
        assert!(err.contains(needle), "expected {needle:?}, got: {err}");
    }
}

/// A recursive `$ref` must hit the depth bound rather than loop forever.
#[test]
fn recursive_ref_hits_the_depth_bound() {
    let schema = serde_json::json!({
      "$defs": { "Loop": { "$ref": "#/$defs/Loop" } },
      "type": "object",
      "properties": { "x": { "$ref": "#/$defs/Loop" } }
    });
    let err = check_err(
        r#"
[[render.annotations]]
role = "code"
path = "result.x"
"#,
        &empty_object(),
        &schema,
    );
    assert!(err.contains("depth bound"), "{err}");
}

/// A node with no `type` cannot be shape-checked, so the annotation is rejected
/// instead of being accepted on faith.
#[test]
fn untyped_node_is_rejected() {
    let schema = serde_json::json!({
      "type": "object",
      "properties": { "x": { "description": "anything" } }
    });
    let err = check_err(
        r#"
[[render.annotations]]
role = "code"
path = "result.x"
"#,
        &empty_object(),
        &schema,
    );
    assert!(err.contains("no declared"), "{err}");
}

/// Tuple-form `items` is not the element-wise shape this vocabulary describes.
#[test]
fn tuple_items_are_rejected() {
    let schema = serde_json::json!({
      "type": "object",
      "properties": {
        "pair": { "type": "array", "items": [{ "type": "string" }, { "type": "string" }] }
      }
    });
    let err = check_err(
        r#"
[[render.annotations]]
role = "tree"
path = "result.pair[]"
"#,
        &empty_object(),
        &schema,
    );
    assert!(err.contains("tuple-form"), "{err}");
}

/// A union whose branches share no single `const` property cannot be selected by
/// value, and an ambiguous one must not silently pick a discriminant.
#[test]
fn undiscriminated_and_ambiguous_unions_are_rejected() {
    let untagged = serde_json::json!({
      "oneOf": [
        { "type": "object", "properties": { "a": { "type": "string" } } },
        { "type": "object", "properties": { "b": { "type": "string" } } }
      ]
    });
    let err = check_err(
        r#"
[[render.arms]]
at = "result"
case = "a"
[[render.arms.annotations]]
role = "code"
path = "result.a"
"#,
        &empty_object(),
        &untagged,
    );
    assert!(err.contains("no single discriminant"), "{err}");

    let two_tags = serde_json::json!({
      "oneOf": [
        {
          "type": "object",
          "properties": { "kind": { "const": "x" }, "op": { "const": "p" },
                          "v": { "type": "string" } }
        },
        {
          "type": "object",
          "properties": { "kind": { "const": "y" }, "op": { "const": "q" },
                          "v": { "type": "string" } }
        }
      ]
    });
    let err = check_err(
        r#"
[[render.arms]]
at = "result"
case = "x"
[[render.arms.annotations]]
role = "code"
path = "result.v"
"#,
        &empty_object(),
        &two_tags,
    );
    assert!(err.contains("more than one candidate"), "{err}");
}

/// `Option<T>` spelled as `anyOf: [{T}, {null}]` is nullability, not a union to
/// dispatch on, so it must not be mistaken for one.
#[test]
fn nullable_anyof_is_not_treated_as_a_union() {
    let schema = serde_json::json!({
      "type": "object",
      "properties": {
        "maybe": { "anyOf": [{ "type": "string" }, { "type": "null" }] }
      }
    });
    check(
        r#"
[[render.annotations]]
role = "code"
path = "result.maybe"
"#,
        &empty_object(),
        &schema,
    )
    .expect("a nullable anyOf is a string field");
}

/// A doc comment on a field makes schemars emit `description` as a **sibling**
/// of `$ref`. A strict "the `$ref` is the whole node" rule would stop
/// dereferencing there and report every documented field as dangling — and
/// `mutate.edit`'s `expected_hash` and `observe.workspace`'s descriptions are
/// exactly that shape.
#[test]
fn a_ref_beside_a_description_still_dereferences() {
    let schema = serde_json::json!({
      "$defs": {
        "Sha": { "type": "string" },
        "Kind": { "type": "string", "enum": ["a", "b"] },
        "Row": {
          "type": "object",
          "required": ["name"],
          "properties": { "name": { "type": "string" } }
        }
      },
      "type": "object",
      "properties": {
        "sha": { "description": "A documented newtype field.", "$ref": "#/$defs/Sha" },
        "kind": { "title": "Kind", "description": "doc", "$ref": "#/$defs/Kind" },
        "rows": {
          "description": "doc",
          "type": "array",
          "items": { "description": "doc", "$ref": "#/$defs/Row" }
        }
      }
    });
    check(
        r#"
[[render.annotations]]
role = "code"
path = "result.sha"
[[render.annotations]]
role = "status"
path = "result.kind"
[[render.annotations]]
role = "columns"
path = "result.rows[]"
columns = ["name"]
"#,
        &empty_object(),
        &schema,
    )
    .expect("annotation keywords beside a `$ref` must not block dereferencing");
}

/// `Option<NamedType>` is `anyOf: [{$ref: T}, {type: null}]`, unlike
/// `Option<primitive>`, which is `type: [T, "null"]`. Both are nullability and
/// must resolve to `T`.
#[test]
fn nullable_ref_unwraps_to_its_target() {
    let schema = serde_json::json!({
      "$defs": {
        "Sha": { "type": "string" },
        "Kind": { "type": "string", "enum": ["a", "b"] },
        "Row": {
          "type": "object",
          "required": ["name"],
          "properties": { "name": { "type": "string" } }
        }
      },
      "type": "object",
      "properties": {
        "sha": { "anyOf": [{ "$ref": "#/$defs/Sha" }, { "type": "null" }] },
        "kind": { "anyOf": [{ "$ref": "#/$defs/Kind" }, { "type": "null" }] },
        "rows": {
          "anyOf": [
            { "type": "array", "items": { "$ref": "#/$defs/Row" } },
            { "type": "null" }
          ]
        }
      }
    });
    check(
        r#"
[[render.annotations]]
role = "code"
path = "result.sha"
[[render.annotations]]
role = "status"
path = "result.kind"
[[render.annotations]]
role = "columns"
path = "result.rows[]"
columns = ["name"]
"#,
        &empty_object(),
        &schema,
    )
    .expect("an Option<NamedType> must resolve to the named type");
}

/// `Option<Vec<T>>` is `type: ["array","null"]` with a sibling `items`, so the
/// element-wise walk must still find the element schema through the nullable
/// type set.
#[test]
fn nullable_array_still_walks_element_wise() {
    let schema = serde_json::json!({
      "$defs": {
        "Row": {
          "type": "object",
          "required": ["a"],
          "properties": { "a": { "type": "string" } }
        }
      },
      "type": "object",
      "properties": {
        "rows": { "type": ["array", "null"], "items": { "$ref": "#/$defs/Row" } },
        "names": { "type": ["array", "null"], "items": { "type": "string" } }
      }
    });
    check(
        r#"
[[render.annotations]]
role = "columns"
path = "result.rows[]"
columns = ["a"]
[[render.annotations]]
role = "tree"
path = "result.names[]"
"#,
        &empty_object(),
        &schema,
    )
    .expect("a nullable array is still an array");
}

/// The sibling rule must stay narrow: an *unknown* keyword beside a `$ref` is
/// treated as schema-bearing, so a construct this resolver does not model blocks
/// the collapse instead of being silently ignored.
#[test]
fn an_unknown_keyword_beside_a_ref_blocks_the_collapse() {
    let schema = serde_json::json!({
      "$defs": { "Sha": { "type": "string" } },
      "type": "object",
      "properties": {
        "sha": { "unevaluatedProperties": false, "$ref": "#/$defs/Sha" }
      }
    });
    let err = check_err(
        r#"
[[render.annotations]]
role = "code"
path = "result.sha"
"#,
        &empty_object(),
        &schema,
    );
    assert!(
        err.contains("no declared"),
        "an unmodelled keyword must fail closed rather than be ignored: {err}"
    );
}
