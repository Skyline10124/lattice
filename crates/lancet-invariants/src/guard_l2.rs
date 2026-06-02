use crate::invariants::InvariantCard;
use lancet_catalog::methods::envelope::L2Status;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AstCategory {
    FunctionDef,
    StructDef,
    EnumDef,
    ImplBlock,
    TraitDef,
    UseDecl,
    ModDecl,
    MacroInvocation,
    ConstStatic,
    TypeAlias,
    Other,
}

impl AstCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FunctionDef => "function_def",
            Self::StructDef => "struct_def",
            Self::EnumDef => "enum_def",
            Self::ImplBlock => "impl_block",
            Self::TraitDef => "trait_def",
            Self::UseDecl => "use_decl",
            Self::ModDecl => "mod_decl",
            Self::MacroInvocation => "macro_invocation",
            Self::ConstStatic => "const_static",
            Self::TypeAlias => "type_alias",
            Self::Other => "other",
        }
    }
}

impl std::fmt::Display for AstCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

pub fn node_kind_to_category(kind: &str) -> AstCategory {
    match kind {
        "function_item" => AstCategory::FunctionDef,
        "struct_item" => AstCategory::StructDef,
        "enum_item" => AstCategory::EnumDef,
        "impl_item" => AstCategory::ImplBlock,
        "trait_item" => AstCategory::TraitDef,
        "use_declaration" => AstCategory::UseDecl,
        "mod_item" => AstCategory::ModDecl,
        "macro_invocation" => AstCategory::MacroInvocation,
        "const_item" | "static_item" => AstCategory::ConstStatic,
        "type_item" => AstCategory::TypeAlias,
        _ => AstCategory::Other,
    }
}

fn parse_rust_file(source: &[u8]) -> Option<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}

/// Return deduplicated `AstCategory` values for top-level named children
/// of the root node whose byte range overlaps `[start_byte, end_byte)`.
///
/// KNOWN LIMITATION: This walks top-level children only. Nested items
/// (e.g., a `fn` inside an `impl` block) are not directly categorized —
/// only the parent `impl_item` is. This is correct for the current shallow AST
/// approach. Deep categorization is deferred.
pub fn categories_in_byte_range(
    source: &[u8],
    start_byte: usize,
    end_byte: usize,
) -> Vec<AstCategory> {
    let end_byte = end_byte.min(source.len());
    if start_byte >= end_byte {
        return vec![];
    }

    let tree = match parse_rust_file(source) {
        Some(t) => t,
        None => return vec![],
    };

    let root = tree.root_node();
    let mut categories = HashSet::new();

    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        let child_start = child.start_byte();
        let child_end = child.end_byte();
        if child_start < end_byte && child_end > start_byte {
            let kind = child.kind();
            let cat = node_kind_to_category(kind);
            categories.insert(cat);
        }
    }

    let mut result: Vec<AstCategory> = categories.into_iter().collect();
    result.sort_by_key(|c| c.as_str());
    result
}

#[derive(Clone, Debug)]
pub struct L2CheckResult {
    pub status: L2Status,
    pub violations: Vec<L2Violation>,
}

#[derive(Clone, Debug)]
pub struct L2Violation {
    pub category: AstCategory,
    pub reason: String,
    pub card_id: String,
}

pub fn check_l2(
    file_path: &str,
    source: &[u8],
    changed_byte_ranges: &[(usize, usize)],
    matched_cards: &[&InvariantCard],
) -> L2CheckResult {
    if !file_path.ends_with(".rs") {
        return L2CheckResult {
            status: L2Status::NotApplicable,
            violations: vec![],
        };
    }

    if parse_rust_file(source).is_none() {
        return L2CheckResult {
            status: L2Status::NotApplicable,
            violations: vec![],
        };
    }

    let mut found_categories = HashSet::new();

    for (start_byte, end_byte) in changed_byte_ranges {
        for category in categories_in_byte_range(source, *start_byte, *end_byte) {
            found_categories.insert(category);
        }
    }

    let found: Vec<AstCategory> = {
        let mut v: Vec<_> = found_categories.into_iter().collect();
        v.sort_by_key(|c| c.as_str());
        v
    };

    let mut violations = Vec::new();
    for card in matched_cards {
        for forbidden in &card.forbidden {
            let forbidden_cat = forbidden.category.as_str();
            if found.iter().any(|c| c.as_str() == forbidden_cat) {
                violations.push(L2Violation {
                    category: found
                        .iter()
                        .find(|c| c.as_str() == forbidden_cat)
                        .cloned()
                        .unwrap_or(AstCategory::Other),
                    reason: forbidden.reason.clone(),
                    card_id: card.id.clone(),
                });
            }
        }
    }

    L2CheckResult {
        status: L2Status::Covered {
            categories: found.iter().map(|c| c.as_str().to_string()).collect(),
        },
        violations,
    }
}

pub fn l2_status_for_method(method: &str) -> L2Status {
    match method {
        "mutate.create" | "mutate.patch" => {
            // Caller invokes check_l2; this just gates applicability
            L2Status::Covered { categories: vec![] }
        }
        other => L2Status::NotImplemented {
            method: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invariants::ForbiddenEntry;

    #[test]
    fn test_node_kind_to_category_mapping() {
        assert_eq!(
            node_kind_to_category("function_item"),
            AstCategory::FunctionDef
        );
        assert_eq!(node_kind_to_category("struct_item"), AstCategory::StructDef);
        assert_eq!(node_kind_to_category("enum_item"), AstCategory::EnumDef);
        assert_eq!(node_kind_to_category("impl_item"), AstCategory::ImplBlock);
        assert_eq!(node_kind_to_category("trait_item"), AstCategory::TraitDef);
        assert_eq!(
            node_kind_to_category("use_declaration"),
            AstCategory::UseDecl
        );
        assert_eq!(node_kind_to_category("mod_item"), AstCategory::ModDecl);
        assert_eq!(
            node_kind_to_category("macro_invocation"),
            AstCategory::MacroInvocation
        );
        assert_eq!(
            node_kind_to_category("const_item"),
            AstCategory::ConstStatic
        );
        assert_eq!(
            node_kind_to_category("static_item"),
            AstCategory::ConstStatic
        );
        assert_eq!(node_kind_to_category("type_item"), AstCategory::TypeAlias);
        assert_eq!(node_kind_to_category("unknown_thing"), AstCategory::Other);
    }

    #[test]
    fn test_parse_rust_file_valid() {
        let source = b"fn foo() -> i32 { 42 }";
        let tree = parse_rust_file(source);
        assert!(tree.is_some());
    }

    #[test]
    fn test_categories_in_range_function_def() {
        let source = b"fn foo() -> i32 { 42 }\nstruct Bar {}\n";
        let cats = categories_in_byte_range(source, 0, 20);
        assert!(cats.contains(&AstCategory::FunctionDef));
    }

    #[test]
    fn test_categories_in_range_struct_def() {
        let source = b"fn foo() -> i32 { 42 }\nstruct Bar {}\n";
        let cats = categories_in_byte_range(source, 25, 40);
        assert!(cats.contains(&AstCategory::StructDef));
    }

    #[test]
    fn test_check_l2_non_rs_file_not_applicable() {
        let result = check_l2("config.toml", b"[foo]\nbar = 1\n", &[(0, 10)], &[]);
        assert!(matches!(result.status, L2Status::NotApplicable));
    }

    #[test]
    fn test_check_l2_forbidden_struct_violation() {
        let source = b"struct Foo { x: i32 }\n";
        let card = InvariantCard {
            id: "CARD-1".into(),
            title: "No structs".into(),
            anchor: crate::invariants::CardAnchor::Glob("*.rs".into()),
            forbidden: vec![ForbiddenEntry {
                category: "struct_def".into(),
                reason: "No new structs allowed".into(),
            }],
            opaque: vec![],
        };
        let cards = vec![&card];
        let result = check_l2("src/lib.rs", source, &[(0, 25)], &cards);
        assert!(!result.violations.is_empty());
        assert_eq!(result.violations[0].category, AstCategory::StructDef);
    }

    #[test]
    fn test_check_l2_no_violation() {
        let source = b"fn hello() {}\n";
        let card = InvariantCard {
            id: "CARD-2".into(),
            title: "No structs".into(),
            anchor: crate::invariants::CardAnchor::Glob("*.rs".into()),
            forbidden: vec![ForbiddenEntry {
                category: "struct_def".into(),
                reason: "No new structs".into(),
            }],
            opaque: vec![],
        };
        let cards = vec![&card];
        let result = check_l2("src/lib.rs", source, &[(0, 15)], &cards);
        assert!(result.violations.is_empty());
        if let L2Status::Covered { categories } = &result.status {
            assert!(categories.iter().any(|c| c == "function_def"));
        }
    }

    #[test]
    fn test_l2_status_for_non_mutation_method() {
        let status = l2_status_for_method("observe.workspace");
        assert!(matches!(status, L2Status::NotImplemented { .. }));
    }

    #[test]
    fn test_l2_status_for_mutation_method() {
        let status = l2_status_for_method("mutate.create");
        assert!(matches!(status, L2Status::Covered { .. }));
    }
}
