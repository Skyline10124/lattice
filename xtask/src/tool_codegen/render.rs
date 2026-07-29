// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! The manifest `[render]` section and its `render.json` projection.
//!
//! `[render]` lets the Tool Descriptor's owner declare the **presentation
//! intent** of its own result: which field is code, which two fields are the
//! sides of one replacement, which enum is a lifecycle status, which columns an
//! array-of-object contributes, which bool means "truncated". Annotations bind
//! to fields only. They carry no rows, columns, positions, or nesting: the
//! presentation primitives belong to the consumer, so this vocabulary may
//! select fields, partition by discriminant value, and declare a field's domain
//! semantics — nothing else.
//!
//! `render.json` is a projection artifact, a sibling of `llm-projection.json`.
//! It is **not** part of `ToolDescriptor`, `descriptor.json`,
//! `descriptor-sha256`, or `expected-identity.json`. That boundary is hard:
//! manifest → descriptor → expected-identity → host composition is one
//! digest-pinned identity chain, and iterating on presentation must never
//! invalidate a Tool identity, an artifact attestation, or a composition pin.
//! Nothing in this module constructs or touches a `ToolDescriptor`; it reads the
//! manifest's two schemas and produces one file.
//!
//! A Tool that declares no `[render]` is still renderable — the consumer falls
//! back to deriving shape from `result_schema` — so absence is a legal state
//! and produces no file.
//!
//! Paths are **data paths**: they name where the value actually lives in the
//! result JSON, envelope included (`result.result.content` for an adjacently
//! tagged union branch). Requiring authors to write an envelope-stripped path
//! would only move the envelope knowledge to the consumer, which would then
//! have to re-add it to read the value.

use std::collections::BTreeSet;
use std::fmt;

use anyhow::{Context as _, anyhow, bail};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Manifest surface (strict: deny_unknown_fields everywhere)
// ---------------------------------------------------------------------------

/// The optional `[render]` section.
///
/// Both lists default to empty so an author can declare only arms or only
/// top-level annotations, but a section that ends up wholly empty is rejected
/// by [`RenderSection::check_not_degenerate`]: absence of `[render]` is the only
/// legal empty state, and a silently-empty section is the same fail-open shape
/// as the misspelled `[[capabilties]]` regression one nesting level down.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenderSection {
    #[serde(default)]
    pub(crate) annotations: Vec<Annotation>,
    #[serde(default)]
    pub(crate) arms: Vec<RenderArm>,
}

/// One branch of a discriminated union, selected by tag **value** alone.
///
/// The discriminant *property* is derived from the schema rather than typed by
/// the author: a manifest-supplied tag path would be a second source of truth
/// that can silently disagree with the real serde tag.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenderArm {
    /// The union this arm selects a branch of: `params` / `result` for a root
    /// union, or a path to a nested one (`params.edits[]`).
    pub(crate) at: FieldPath,
    /// The serde tag value naming the branch (`"read_file"`).
    pub(crate) case: String,
    #[serde(default)]
    pub(crate) annotations: Vec<Annotation>,
}

/// A field-level presentation annotation.
///
/// Internally tagged on `role` with per-variant required fields, so a
/// misspelled role is an `unknown variant` error and a key belonging to another
/// role is an `unknown field` error. Modelling this as `role: String` plus
/// optional per-role fields would accept `role = "summry"`, silently drop a
/// stray `columns` on a `code` annotation, and turn a missing `after` on a
/// `diff` into `None` — fail-open states `deny_unknown_fields` cannot see,
/// because every key would be a known field of one flat struct.
#[derive(Debug, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Annotation {
    /// The one field that summarizes the call inline.
    Summary { path: FieldPath },
    /// A field whose string content is source code or file content.
    Code { path: FieldPath },
    /// Two fields that are the before/after sides of one replacement.
    Diff { before: FieldPath, after: FieldPath },
    /// A field whose closed value set is a lifecycle status.
    Status { path: FieldPath },
    /// An element-wise path whose object elements contribute these columns.
    Columns {
        path: FieldPath,
        columns: Vec<String>,
    },
    /// A bool meaning "the value above was cut short".
    Truncated { path: FieldPath },
    /// An element-wise path whose string elements form a path tree.
    Tree { path: FieldPath },
}

impl Annotation {
    /// The `role` token, as written in the manifest and emitted in
    /// `render.json`.
    pub(crate) fn role(&self) -> &'static str {
        match self {
            Annotation::Summary { .. } => "summary",
            Annotation::Code { .. } => "code",
            Annotation::Diff { .. } => "diff",
            Annotation::Status { .. } => "status",
            Annotation::Columns { .. } => "columns",
            Annotation::Truncated { .. } => "truncated",
            Annotation::Tree { .. } => "tree",
        }
    }

    /// Roles that fill a single consumer slot, so declaring one twice in the
    /// same scope is an unresolvable conflict rather than an enhancement.
    fn is_singleton(&self) -> bool {
        matches!(
            self,
            Annotation::Summary { .. } | Annotation::Status { .. } | Annotation::Truncated { .. }
        )
    }

    /// Every path this annotation binds, in declaration order.
    fn paths(&self) -> Vec<&FieldPath> {
        match self {
            Annotation::Summary { path }
            | Annotation::Code { path }
            | Annotation::Status { path }
            | Annotation::Truncated { path }
            | Annotation::Tree { path }
            | Annotation::Columns { path, .. } => vec![path],
            Annotation::Diff { before, after } => vec![before, after],
        }
    }
}

// ---------------------------------------------------------------------------
// Field paths
// ---------------------------------------------------------------------------

/// Which of the manifest's two schemas a path is rooted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathRoot {
    Params,
    Result,
}

impl PathRoot {
    fn as_str(self) -> &'static str {
        match self {
            PathRoot::Params => "params",
            PathRoot::Result => "result",
        }
    }
}

/// One path segment: a property name, optionally mapped element-wise.
///
/// `elementwise` is the `[]` suffix and means "apply to each element", so the
/// node a segment resolves to is the *element* schema and a role's type
/// predicate applies to that element. One rule, one spelling: `columns` needs
/// an element-wise path whose element is an object, `tree` needs one whose
/// element is a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment {
    pub(crate) name: String,
    pub(crate) elementwise: bool,
}

/// A validated `params`/`result`-rooted field path.
///
/// Parsed at manifest-deserialize time, so a malformed string fails as a
/// grammar error pointing at the string instead of surfacing later as a
/// confusing "dangling path" for something that never was a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldPath {
    pub(crate) root: PathRoot,
    pub(crate) segments: Vec<Segment>,
}

impl FieldPath {
    /// Grammar:
    ///
    /// ```text
    /// path    = ("params" | "result") ( "." segment )*
    /// segment = name [ "[]" ]
    /// name    = 1*( char except '.' '[' ']' and whitespace )
    /// ```
    ///
    /// A bare `params` / `result` is legal and denotes the schema root, which is
    /// how `[[render.arms]] at = "result"` selects a root union.
    fn parse(s: &str) -> Result<Self, String> {
        if s != s.trim() {
            return Err(format!(
                "render path {s:?} has leading or trailing whitespace"
            ));
        }
        let (root, rest) = if let Some(rest) = s.strip_prefix("params") {
            (PathRoot::Params, rest)
        } else if let Some(rest) = s.strip_prefix("result") {
            (PathRoot::Result, rest)
        } else {
            return Err(format!(
                "render path {s:?} must start with `params` or `result` (case-sensitive)"
            ));
        };
        // The prefix is positional: it must be the entire first segment, so
        // `paramsx.y` is rejected while `params.params.x` stays legal and means
        // "the `params` property of the params schema".
        if !rest.is_empty() && !rest.starts_with('.') {
            return Err(format!(
                "render path {s:?} must start with `params` or `result` followed by `.` or \
                 nothing"
            ));
        }
        let mut segments = Vec::new();
        if !rest.is_empty() {
            for raw in rest[1..].split('.') {
                segments.push(Self::parse_segment(raw, s)?);
            }
        }
        Ok(FieldPath { root, segments })
    }

    fn parse_segment(raw: &str, whole: &str) -> Result<Segment, String> {
        if raw.is_empty() {
            return Err(format!("render path {whole:?} has an empty segment"));
        }
        let (name, elementwise) = match raw.strip_suffix("[]") {
            Some(name) => (name, true),
            None => (raw, false),
        };
        if name.is_empty() {
            return Err(format!(
                "render path {whole:?} has a segment with `[]` but no property name"
            ));
        }
        if name.contains(['[', ']']) {
            return Err(format!(
                "render path {whole:?} segment {raw:?} is malformed: `[]` is valid once, as a \
                 segment suffix"
            ));
        }
        if name.chars().any(char::is_whitespace) {
            return Err(format!(
                "render path {whole:?} segment {raw:?} contains whitespace"
            ));
        }
        Ok(Segment {
            name: name.to_owned(),
            elementwise,
        })
    }

    /// Whether `self` starts with every segment of `prefix` (element-wise flags
    /// included) in the same schema.
    fn starts_with(&self, prefix: &FieldPath) -> bool {
        self.root == prefix.root
            && self.segments.len() >= prefix.segments.len()
            && self.segments[..prefix.segments.len()] == prefix.segments[..]
    }

    /// `self` with the first `n` segments dropped.
    fn skip(&self, n: usize) -> FieldPath {
        FieldPath {
            root: self.root,
            segments: self.segments[n..].to_vec(),
        }
    }

    /// The prefix of `self` up to (not including) segment `n`, for error text.
    fn truncated_to(&self, n: usize) -> FieldPath {
        FieldPath {
            root: self.root,
            segments: self.segments[..n].to_vec(),
        }
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.root.as_str())?;
        for seg in &self.segments {
            write!(f, ".{}", seg.name)?;
            if seg.elementwise {
                f.write_str("[]")?;
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for FieldPath {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        FieldPath::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Schema view: the JSON Schema subset the resolver understands
// ---------------------------------------------------------------------------

/// How deep `$ref`/`allOf` indirection may be followed before the resolver
/// declares a cycle. A recursive type would otherwise loop forever; past the
/// bound this fails closed.
const MAX_REF_DEPTH: usize = 32;

/// A node of one manifest schema, carrying the document it came from so `$ref`
/// can be resolved.
#[derive(Clone, Copy)]
struct SchemaView<'a> {
    doc: &'a serde_json::Value,
    node: &'a serde_json::Value,
}

/// The primitive shape a role predicate requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Primitive {
    String,
    Boolean,
    Scalar,
}

impl<'a> SchemaView<'a> {
    fn root(doc: &'a serde_json::Value) -> Self {
        SchemaView { doc, node: doc }
    }

    fn with(self, node: &'a serde_json::Value) -> Self {
        SchemaView {
            doc: self.doc,
            node,
        }
    }

    /// Follow a `$ref` that carries no schema-bearing siblings, unwrap the
    /// nullable spelling of a named type, and flatten single-member `allOf`
    /// wrappers, until a concrete node is reached.
    ///
    /// "No schema-bearing siblings" rather than "is the whole node" because
    /// `schemars` emits a doc comment as a `description` **sibling** of `$ref`:
    /// a strict one-key rule would stop dereferencing at every documented field
    /// and report it as dangling. Annotation keywords do not constrain the
    /// instance, so a `$ref` beside them still denotes its target.
    ///
    /// A `properties`/`type`/`required` sibling *is* schema-bearing and is not
    /// collapsed: for an internally tagged enum, schemars emits the branch
    /// payload as a `$ref` sibling of `properties: {tag}` and both halves are
    /// real, so [`Self::merged_properties`] merges them instead.
    fn deref(self, at: &FieldPath) -> anyhow::Result<Self> {
        let mut cur = self;
        for _ in 0..MAX_REF_DEPTH {
            if let Some(r) = cur.node.get("$ref").and_then(|r| r.as_str())
                && cur.has_no_schema_siblings_of("$ref")
            {
                cur = cur.with(cur.resolve_ref(r, at)?);
                continue;
            }
            // `Option<NamedType>` is `anyOf: [{$ref: T}, {type: null}]`, unlike
            // `Option<primitive>`, which is `type: [T, "null"]`. Both are
            // nullability, not a union to dispatch on, so unwrap to `T`.
            if let Some(inner) = cur.nullable_inner()
                && cur.has_no_schema_siblings_of("anyOf")
            {
                cur = cur.with(inner);
                continue;
            }
            if let Some(all) = cur.node.get("allOf").and_then(|a| a.as_array())
                && all.len() == 1
                && cur.has_no_schema_siblings_of("allOf")
            {
                cur = cur.with(&all[0]);
                continue;
            }
            return Ok(cur);
        }
        bail!(
            "render path `{at}` exceeds the {MAX_REF_DEPTH}-level $ref/allOf depth bound \
             (recursive schema?)"
        )
    }

    /// Whether every key besides `keyword` is a JSON Schema *annotation* — one
    /// that describes the schema rather than constraining the instance. Listing
    /// the annotations rather than the constraints is deliberate: an unknown
    /// keyword counts as schema-bearing, so a construct this resolver does not
    /// model blocks the collapse instead of being silently ignored.
    fn has_no_schema_siblings_of(self, keyword: &str) -> bool {
        const ANNOTATIONS: &[&str] = &[
            "$comment",
            "$schema",
            "default",
            "deprecated",
            "description",
            "examples",
            "format",
            "readOnly",
            "title",
            "writeOnly",
        ];
        self.node.as_object().is_some_and(|o| {
            o.keys()
                .all(|k| k == keyword || ANNOTATIONS.contains(&k.as_str()))
        })
    }

    /// The non-null branch of a two-branch nullable `oneOf`/`anyOf`.
    fn nullable_inner(self) -> Option<&'a serde_json::Value> {
        for key in ["oneOf", "anyOf"] {
            if let Some(list) = self.node.get(key).and_then(|v| v.as_array())
                && list.len() == 2
            {
                let is_null =
                    |b: &serde_json::Value| b.get("type").and_then(|t| t.as_str()) == Some("null");
                if is_null(&list[0]) && !is_null(&list[1]) {
                    return Some(&list[1]);
                }
                if is_null(&list[1]) && !is_null(&list[0]) {
                    return Some(&list[0]);
                }
            }
        }
        None
    }

    /// Resolve a same-document `#/$defs/X` or `#/definitions/X` pointer.
    /// An external ref, a deeper pointer, or an undefined name is a hard error
    /// rather than a silent miss.
    fn resolve_ref(self, r: &str, at: &FieldPath) -> anyhow::Result<&'a serde_json::Value> {
        let name = r
            .strip_prefix("#/$defs/")
            .or_else(|| r.strip_prefix("#/definitions/"))
            .ok_or_else(|| {
                anyhow!(
                    "render path `{at}` reaches unsupported $ref {r:?}; only same-document \
                     `#/$defs/<name>` and `#/definitions/<name>` are supported"
                )
            })?;
        if name.contains('/') {
            bail!(
                "render path `{at}` reaches unsupported nested $ref {r:?}; only \
                 `#/$defs/<name>` is supported"
            );
        }
        self.doc
            .get("$defs")
            .or_else(|| self.doc.get("definitions"))
            .and_then(|d| d.get(name))
            .ok_or_else(|| anyhow!("render path `{at}` reaches $ref {r:?}, which is not defined"))
    }

    /// The union branches at this node, if it is one. `[T, null]` is schemars'
    /// nullable spelling, not a union to dispatch on, so it is not reported.
    fn branches(self) -> Option<&'a Vec<serde_json::Value>> {
        if self.nullable_inner().is_some() {
            return None;
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(list) = self.node.get(key).and_then(|v| v.as_array()) {
                return Some(list);
            }
        }
        None
    }

    /// This node's properties, merged with those its `$ref` siblings and
    /// `allOf` members contribute. This is what makes an internally tagged
    /// branch walkable: `{properties: {mode}, $ref: Payload}` exposes `mode`
    /// **and** every property of `Payload`.
    fn merged_properties(
        self,
        at: &FieldPath,
    ) -> anyhow::Result<Vec<(String, &'a serde_json::Value)>> {
        let mut out: Vec<(String, &'a serde_json::Value)> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        self.collect_properties(at, 0, &mut out, &mut seen)?;
        Ok(out)
    }

    fn collect_properties(
        self,
        at: &FieldPath,
        depth: usize,
        out: &mut Vec<(String, &'a serde_json::Value)>,
        seen: &mut BTreeSet<String>,
    ) -> anyhow::Result<()> {
        if depth > MAX_REF_DEPTH {
            bail!(
                "render path `{at}` exceeds the {MAX_REF_DEPTH}-level $ref/allOf depth bound \
                 (recursive schema?)"
            );
        }
        if let Some(props) = self.node.get("properties").and_then(|p| p.as_object()) {
            for (k, v) in props {
                if seen.insert(k.clone()) {
                    out.push((k.clone(), v));
                }
            }
        }
        if let Some(r) = self.node.get("$ref").and_then(|r| r.as_str()) {
            let target = self.resolve_ref(r, at)?;
            self.with(target)
                .collect_properties(at, depth + 1, out, seen)?;
        }
        if let Some(all) = self.node.get("allOf").and_then(|a| a.as_array()) {
            for member in all {
                self.with(member)
                    .collect_properties(at, depth + 1, out, seen)?;
            }
        }
        Ok(())
    }

    /// The element schema of an array node.
    fn items(self, at: &FieldPath, seg: &str) -> anyhow::Result<Self> {
        let items = self.node.get("items").ok_or_else(|| {
            anyhow!(
                "render path `{at}` maps segment `{seg}[]` element-wise, but that field is not \
                 an array with an `items` schema"
            )
        })?;
        if items.is_array() {
            bail!(
                "render path `{at}` maps segment `{seg}[]` element-wise, but that field uses \
                 tuple-form `items`, which is not supported"
            );
        }
        Ok(self.with(items))
    }

    /// The declared type set with `"null"` dropped. Optionality is orthogonal
    /// to a role — a consumer skipping an absent value is #116's Layer-1
    /// concern, not a reason to reject the annotation — and `Option<T>` reaches
    /// here as `type: [T, "null"]` or as `anyOf: [{T}, {null}]`.
    fn non_null_types(self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        match self.node.get("type") {
            Some(serde_json::Value::String(s)) => {
                out.insert(s.clone());
            }
            Some(serde_json::Value::Array(list)) => {
                for t in list.iter().filter_map(|t| t.as_str()) {
                    if t != "null" {
                        out.insert(t.to_owned());
                    }
                }
            }
            _ => {}
        }
        if out.is_empty() {
            for key in ["oneOf", "anyOf"] {
                if let Some(list) = self.node.get(key).and_then(|v| v.as_array()) {
                    for b in list {
                        if let Some(serde_json::Value::String(s)) = b.get("type")
                            && s != "null"
                        {
                            out.insert(s.clone());
                        }
                    }
                }
            }
        }
        out
    }

    /// A closed value set: `enum`, or the `const` schemars emits for a tag.
    fn closed_values(self) -> Option<Vec<String>> {
        if let Some(list) = self.node.get("enum").and_then(|v| v.as_array())
            && !list.is_empty()
        {
            return Some(list.iter().map(value_token).collect());
        }
        self.node.get("const").map(|c| vec![value_token(c)])
    }
}

fn value_token(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// The two schemas a manifest's paths resolve against.
pub(crate) struct RenderSchemas<'a> {
    pub(crate) params: &'a serde_json::Value,
    pub(crate) result: &'a serde_json::Value,
}

impl<'a> RenderSchemas<'a> {
    fn root_of(&self, root: PathRoot) -> SchemaView<'a> {
        match root {
            PathRoot::Params => SchemaView::root(self.params),
            PathRoot::Result => SchemaView::root(self.result),
        }
    }
}

/// Walk `path`'s segments from `base`, reporting `whole` in errors.
///
/// `offset` is how many of `whole`'s leading segments `base` already consumed,
/// so error text can name the real path even inside an arm. A union met while
/// walking is a hard error naming the arm syntax: silently picking a branch
/// would make the annotation mean something the author never wrote.
fn walk<'a>(
    base: SchemaView<'a>,
    path: &FieldPath,
    whole: &FieldPath,
    offset: usize,
) -> anyhow::Result<SchemaView<'a>> {
    let mut cur = base.deref(whole)?;
    for (i, seg) in path.segments.iter().enumerate() {
        if cur.branches().is_some() {
            let prefix = whole.truncated_to(offset + i);
            bail!(
                "render path `{whole}` passes through the discriminated union at `{prefix}`; \
                 declare this annotation inside a `[[render.arms]]` section with \
                 `at = \"{prefix}\"` and that branch's `case`"
            );
        }
        let props = cur.merged_properties(whole)?;
        let next = props
            .iter()
            .find(|(k, _)| *k == seg.name)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                let mut known: Vec<&str> = props.iter().map(|(k, _)| k.as_str()).collect();
                known.sort_unstable();
                anyhow!(
                    "render path `{whole}` is dangling: `{}` has no property `{}` (known: {})",
                    whole.truncated_to(offset + i),
                    seg.name,
                    if known.is_empty() {
                        "none".to_owned()
                    } else {
                        known.join(", ")
                    }
                )
            })?;
        cur = cur.with(next).deref(whole)?;
        if seg.elementwise {
            cur = cur.items(whole, &seg.name)?.deref(whole)?;
        }
    }
    Ok(cur)
}

/// Resolve a path from its schema root.
fn resolve<'a>(schemas: &RenderSchemas<'a>, path: &FieldPath) -> anyhow::Result<SchemaView<'a>> {
    walk(schemas.root_of(path.root), path, path, 0)
}

/// The union at `at`, its derived discriminant property, and its branches.
struct Union<'a> {
    view: SchemaView<'a>,
    branches: &'a Vec<serde_json::Value>,
    discriminant: String,
}

/// Resolve the union an arm points at and derive its discriminant property: the
/// single property carrying a `const` in every branch. Deriving it removes the
/// second source of truth an author-typed tag path would create.
fn resolve_union<'a>(schemas: &RenderSchemas<'a>, at: &FieldPath) -> anyhow::Result<Union<'a>> {
    let view = resolve(schemas, at)?;
    let branches = view.branches().ok_or_else(|| {
        anyhow!(
            "`[[render.arms]]` at = \"{at}\" does not name a discriminated union; arms are only \
             valid on a `oneOf`/`anyOf` schema node"
        )
    })?;
    let mut candidates: Option<BTreeSet<String>> = None;
    for branch in branches {
        let bview = view.with(branch);
        let mut here = BTreeSet::new();
        for (k, v) in bview.merged_properties(at)? {
            if bview.with(v).deref(at)?.node.get("const").is_some() {
                here.insert(k);
            }
        }
        candidates = Some(match candidates {
            None => here,
            Some(prev) => prev.intersection(&here).cloned().collect(),
        });
    }
    let candidates = candidates.unwrap_or_default();
    let discriminant = match candidates.len() {
        1 => candidates.into_iter().next().expect("one candidate"),
        0 => bail!(
            "`[[render.arms]]` at = \"{at}\": the union has no single discriminant property (no \
             property carries a `const` in every branch), so its branches cannot be selected by \
             value"
        ),
        _ => bail!(
            "`[[render.arms]]` at = \"{at}\": the union has more than one candidate discriminant \
             property ({}), so branch selection would be ambiguous",
            candidates.into_iter().collect::<Vec<_>>().join(", ")
        ),
    };
    Ok(Union {
        view,
        branches,
        discriminant,
    })
}

/// The branch of `union` whose discriminant equals `case`.
fn select_branch<'a>(
    union: &Union<'a>,
    at: &FieldPath,
    case: &str,
) -> anyhow::Result<SchemaView<'a>> {
    let mut selected: Option<SchemaView<'a>> = None;
    let mut cases: Vec<String> = Vec::new();
    for branch in union.branches {
        let bview = union.view.with(branch);
        let props = bview.merged_properties(at)?;
        let tag = props
            .iter()
            .find(|(k, _)| *k == union.discriminant)
            .map(|(_, v)| bview.with(v))
            .expect("the discriminant is present in every branch");
        let value = tag
            .deref(at)?
            .node
            .get("const")
            .map(value_token)
            .expect("the discriminant carries a const");
        cases.push(value.clone());
        if value == case {
            if selected.is_some() {
                bail!(
                    "`[[render.arms]]` at = \"{at}\", case = \"{case}\": two branches carry the \
                     same `{}` value, so the arm is ambiguous",
                    union.discriminant
                );
            }
            selected = Some(bview);
        }
    }
    selected.ok_or_else(|| {
        anyhow!(
            "`[[render.arms]]` at = \"{at}\" has case = \"{case}\", which is not a `{}` value of \
             that union (valid: {})",
            union.discriminant,
            cases.join(", ")
        )
    })
}

/// The scope an annotation resolves in: either a schema root, or a selected
/// union branch plus the arm path that reached it.
enum Scope<'a> {
    Root,
    Arm {
        at: &'a FieldPath,
        branch: SchemaView<'a>,
    },
}

/// Resolve one path inside `scope`.
///
/// Inside an arm, a path rooted in the arm's own schema must be written in full
/// and must start with the arm's `at` — the same string then means the same
/// field everywhere. A path rooted in the *other* schema resolves from that
/// schema's root, so `params.*` annotations stay usable in a `result` arm.
fn resolve_in<'a>(
    schemas: &RenderSchemas<'a>,
    scope: &Scope<'a>,
    path: &FieldPath,
) -> anyhow::Result<SchemaView<'a>> {
    match scope {
        Scope::Arm { at, branch } if at.root == path.root => {
            if !path.starts_with(at) {
                bail!(
                    "render path `{path}` is declared inside `[[render.arms]]` at = \"{at}\", so \
                     it must start with `{at}`"
                );
            }
            if path.segments.len() == at.segments.len() {
                bail!(
                    "render path `{path}` names the union itself, not a field inside the \
                     selected branch"
                );
            }
            let rest = path.skip(at.segments.len());
            walk(*branch, &rest, path, at.segments.len())
        }
        _ => resolve(schemas, path),
    }
}

/// Resolve an annotation's paths and check its role predicate.
fn check_annotation(
    schemas: &RenderSchemas<'_>,
    scope: &Scope<'_>,
    scope_label: &str,
    ann: &Annotation,
) -> anyhow::Result<()> {
    let role = ann.role();
    match ann {
        Annotation::Summary { path } => {
            let node = resolve_in(schemas, scope, path)?;
            require_primitive(&node, Primitive::Scalar, role, path, scope_label)?;
        }
        Annotation::Code { path } => {
            let node = resolve_in(schemas, scope, path)?;
            require_primitive(&node, Primitive::String, role, path, scope_label)?;
        }
        Annotation::Truncated { path } => {
            let node = resolve_in(schemas, scope, path)?;
            require_primitive(&node, Primitive::Boolean, role, path, scope_label)?;
        }
        Annotation::Diff { before, after } => {
            for (which, path) in [("before", before), ("after", after)] {
                let node = resolve_in(schemas, scope, path)?;
                require_primitive(&node, Primitive::String, role, path, scope_label)
                    .with_context(|| format!("`diff` {which} side"))?;
            }
        }
        Annotation::Status { path } => {
            // `status` declares "this field is the call's lifecycle status", not
            // "this field is an enum". A closed value set is the common case
            // (`TodoStatus`, a serde tag) and lets a consumer exhaustively map
            // values to chrome, but an open scalar is a real status too:
            // `fetch.web`'s HTTP code is the most natural status that call has,
            // and requiring an `enum` would leave it with no spelling at all.
            // A non-scalar is still rejected — a status is one value, not a
            // structure.
            let node = resolve_in(schemas, scope, path)?;
            if node.closed_values().is_none() {
                require_primitive(&node, Primitive::Scalar, role, path, scope_label)?;
            }
        }
        Annotation::Tree { path } => {
            require_elementwise(path, role, scope_label)?;
            let node = resolve_in(schemas, scope, path)?;
            require_primitive(&node, Primitive::String, role, path, scope_label)?;
        }
        Annotation::Columns { path, columns } => {
            require_elementwise(path, role, scope_label)?;
            if columns.is_empty() {
                bail!("{scope_label}: `columns` path `{path}` declares an empty `columns` list");
            }
            let mut seen = BTreeSet::new();
            for c in columns {
                if !seen.insert(c) {
                    bail!("{scope_label}: `columns` path `{path}` lists column `{c}` twice");
                }
            }
            let node = resolve_in(schemas, scope, path)?;
            let types = node.non_null_types();
            if !types.is_empty() && !types.contains("object") {
                bail!(
                    "{scope_label}: `columns` path `{path}` must map element-wise to objects, \
                     but its element type is {}",
                    types.into_iter().collect::<Vec<_>>().join("|")
                );
            }
            let props = node.merged_properties(path)?;
            for c in columns {
                if !props.iter().any(|(k, _)| k == c) {
                    let mut known: Vec<&str> = props.iter().map(|(k, _)| k.as_str()).collect();
                    known.sort_unstable();
                    bail!(
                        "{scope_label}: `columns` path `{path}` lists column `{c}`, which is not \
                         a property of the element (known: {})",
                        if known.is_empty() {
                            "none".to_owned()
                        } else {
                            known.join(", ")
                        }
                    );
                }
            }
        }
    }
    Ok(())
}

fn require_elementwise(path: &FieldPath, role: &str, scope_label: &str) -> anyhow::Result<()> {
    if path.segments.last().map(|s| s.elementwise) != Some(true) {
        bail!(
            "{scope_label}: `{role}` path `{path}` must map element-wise; write the last segment \
             with a `[]` suffix"
        );
    }
    Ok(())
}

fn require_primitive(
    node: &SchemaView<'_>,
    want: Primitive,
    role: &str,
    path: &FieldPath,
    scope_label: &str,
) -> anyhow::Result<()> {
    let types = node.non_null_types();
    if types.is_empty() {
        bail!(
            "{scope_label}: `{role}` path `{path}` resolves to a schema node with no declared \
             `type`, so its shape cannot be checked"
        );
    }
    let ok = match want {
        Primitive::String => types.len() == 1 && types.contains("string"),
        Primitive::Boolean => types.len() == 1 && types.contains("boolean"),
        Primitive::Scalar => types
            .iter()
            .all(|t| matches!(t.as_str(), "string" | "number" | "integer" | "boolean")),
    };
    if !ok {
        let want = match want {
            Primitive::String => "a string",
            Primitive::Boolean => "a boolean",
            Primitive::Scalar => "a scalar (string, number, integer, or boolean)",
        };
        bail!(
            "{scope_label}: `{role}` path `{path}` must resolve to {want}, but it resolves to {}",
            types.into_iter().collect::<Vec<_>>().join("|")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Section validation
// ---------------------------------------------------------------------------

impl RenderSection {
    /// Reject the shapes that would otherwise generate silently.
    fn check_not_degenerate(&self) -> anyhow::Result<()> {
        if self.annotations.is_empty() && self.arms.is_empty() {
            bail!(
                "manifest `[render]` section is empty; delete it (a Tool without `[render]` is a \
                 legal state and produces no render.json) or declare at least one annotation or \
                 arm"
            );
        }
        for arm in &self.arms {
            if arm.annotations.is_empty() {
                bail!(
                    "manifest `[[render.arms]]` at = \"{}\", case = \"{}\" declares no \
                     annotations; every arm must carry at least one",
                    arm.at,
                    arm.case
                );
            }
        }
        Ok(())
    }
}

/// Validate the whole `[render]` section against the manifest's schemas: every
/// path is resolved, and a dangling path, a type-predicate mismatch, an unknown
/// arm case, or a duplicated single-slot role is a codegen error.
pub(crate) fn validate_render(
    section: &RenderSection,
    schemas: &RenderSchemas<'_>,
) -> anyhow::Result<()> {
    section.check_not_degenerate()?;

    const TOP: &str = "manifest `[render]`";
    check_singletons(&section.annotations, TOP)?;
    for ann in &section.annotations {
        check_annotation(schemas, &Scope::Root, TOP, ann)?;
    }

    let mut seen_arms: BTreeSet<(String, String)> = BTreeSet::new();
    // Arms on *different* unions can both be selected by one call, so their
    // single-slot roles compete for the same slot. Arms on the same union are
    // mutually exclusive by construction and do not.
    let mut arm_slots: BTreeSet<(String, &'static str, &'static str)> = BTreeSet::new();
    for arm in &section.arms {
        if !seen_arms.insert((arm.at.to_string(), arm.case.clone())) {
            bail!(
                "manifest declares two `[[render.arms]]` for at = \"{}\", case = \"{}\"",
                arm.at,
                arm.case
            );
        }
        for ann in arm.annotations.iter().filter(|a| a.is_singleton()) {
            let side = ann.paths()[0].root.as_str();
            let key = (arm.at.to_string(), ann.role(), side);
            if arm_slots.contains(&key) {
                continue; // same union: the branches are mutually exclusive.
            }
            if let Some((other, ..)) = arm_slots
                .iter()
                .find(|(at, role, s)| *role == ann.role() && *s == side && at != &key.0)
            {
                bail!(
                    "manifest `[[render.arms]]` at = \"{}\", case = \"{}\": role `{}` is also \
                     declared for `{side}` under at = \"{other}\"; those arms can both be \
                     selected by one call, so they would compete for a single slot",
                    arm.at,
                    arm.case,
                    ann.role()
                );
            }
            arm_slots.insert(key);
        }
        let union = resolve_union(schemas, &arm.at)?;
        let branch = select_branch(&union, &arm.at, &arm.case)?;
        let label = format!(
            "manifest `[[render.arms]]` at = \"{}\", case = \"{}\"",
            arm.at, arm.case
        );
        check_singletons(&arm.annotations, &label)?;
        let scope = Scope::Arm {
            at: &arm.at,
            branch,
        };
        for ann in &arm.annotations {
            // A single-slot role must not fill the same side's slot both at top
            // level and inside an arm: for one call, both would be live at once.
            if ann.is_singleton() {
                let side = ann.paths()[0].root;
                if section.annotations.iter().any(|top| {
                    top.role() == ann.role() && top.is_singleton() && top.paths()[0].root == side
                }) {
                    bail!(
                        "{label}: role `{}` is also declared at the top level of `[render]` for \
                         `{}`; a single-slot role may fill each side's slot in one scope only",
                        ann.role(),
                        side.as_str()
                    );
                }
            }
            check_annotation(schemas, &scope, &label, ann)?;
        }
    }
    Ok(())
}

/// Single-slot roles are counted **per schema root**, not per scope.
///
/// The params and result sides of a card are separate surfaces: #116 renders an
/// error card from `params.*` alone, skipping every `result.*` annotation as
/// dangling by construction. So one `summary` over the call's inputs and one
/// over its output are two different slots, and collapsing them would make a
/// flat-schema Tool choose between naming its request and naming its response.
fn check_singletons(annotations: &[Annotation], scope_label: &str) -> anyhow::Result<()> {
    let mut seen: BTreeSet<(&'static str, &'static str)> = BTreeSet::new();
    for ann in annotations {
        if !ann.is_singleton() {
            continue;
        }
        // A single-slot role binds exactly one path, so `paths()[0]` names its
        // side unambiguously.
        let side = ann.paths()[0].root.as_str();
        if !seen.insert((ann.role(), side)) {
            bail!(
                "{scope_label}: role `{}` is declared more than once for `{side}`, but it fills a \
                 single slot per side",
                ann.role()
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// render.json
// ---------------------------------------------------------------------------

/// The `render.json` format version. Bump when the projection's shape changes,
/// so a consumer can refuse a file it does not understand.
pub(crate) const RENDER_SCHEMA_VERSION: u32 = 1;

/// Render the projection. Deterministic by construction: `serde_json` maps sort
/// their keys (no `preserve_order` feature in this workspace), annotations and
/// arms keep declaration order, 2-space indent, one trailing newline. The tool
/// *name* is carried so the file is interpretable on its own; the tool version
/// is not, so an unrelated version bump is not render drift.
pub(crate) fn render_projection(
    tool_name: &str,
    section: &RenderSection,
    schemas: &RenderSchemas<'_>,
) -> anyhow::Result<String> {
    let mut arms = Vec::with_capacity(section.arms.len());
    for arm in &section.arms {
        let union = resolve_union(schemas, &arm.at)?;
        arms.push(serde_json::json!({
            "at": arm.at.to_string(),
            "discriminant": union.discriminant,
            "case": arm.case,
            "annotations": annotations_json(&arm.annotations),
        }));
    }
    let projection = serde_json::json!({
        "tool": tool_name,
        "render-schema-version": RENDER_SCHEMA_VERSION,
        "annotations": annotations_json(&section.annotations),
        "arms": arms,
    });
    let mut out = serde_json::to_string_pretty(&projection).expect("projection serializes");
    out.push('\n');
    Ok(out)
}

fn annotations_json(annotations: &[Annotation]) -> Vec<serde_json::Value> {
    annotations
        .iter()
        .map(|ann| match ann {
            Annotation::Diff { before, after } => serde_json::json!({
                "role": "diff",
                "before": before.to_string(),
                "after": after.to_string(),
            }),
            Annotation::Columns { path, columns } => serde_json::json!({
                "role": "columns",
                "path": path.to_string(),
                "columns": columns,
            }),
            other => {
                let path = other.paths()[0];
                serde_json::json!({
                    "role": other.role(),
                    "path": path.to_string(),
                })
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
