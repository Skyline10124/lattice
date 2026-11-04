// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use serde_json::json;

#[test]
fn response_filters_visibility_and_rejects_invalid_or_ambiguous_listed_models() {
    let models = parse_response(
        br#"{"models":[
            {"visibility":"hidden"},
            {"visibility":"hidden","slug":"hidden-model","display_name":"Hidden model"},
            {"visibility":"list","slug":"org/model","display_name":"Account name","extra":true}
        ]}"#,
    )
    .unwrap();
    assert_eq!(
        models,
        vec![AccountModel {
            slug: "org/model".into(),
            display_name: "Account name".into()
        }]
    );
    for source in [
        json!({"models":[]}),
        json!({"models":[{"visibility":"hidden"}]}),
    ] {
        assert!(
            parse_response(&serde_json::to_vec(&source).unwrap())
                .unwrap()
                .is_empty()
        );
    }
    for source in [
        json!([]),
        json!({"data":[]}),
        json!({"models":null}),
        json!({"models":[[]]}),
        json!({"models":[{}]}),
        json!({"models":[{"visibility":1}]}),
        json!({"models":[{"visibility":"list","slug":"model"}]}),
        json!({"models":[{"visibility":"list","slug":"","display_name":"Name"}]}),
        json!({"models":[{"visibility":"list","slug":"bad\nID","display_name":"Name"}]}),
        json!({"models":[{"visibility":"list","slug":"x".repeat(4097),"display_name":"Name"}]}),
        json!({"models":[{"visibility":"list","slug":"model","display_name":""}]}),
        json!({"models":[{"visibility":"list","slug":"model","display_name":"bad\u{1b}name"}]}),
        json!({"models":[{"visibility":"list","slug":"same","display_name":"One"},{"visibility":"list","slug":"same","display_name":"Two"}]}),
    ] {
        assert!(
            parse_response(&serde_json::to_vec(&source).unwrap()).is_err(),
            "{source}"
        );
    }
    assert!(parse_response(b"{broken").is_err());
}

#[test]
fn cache_distinguishes_empty_missing_and_invalid_documents() {
    let root = tempfile::tempdir().unwrap();
    assert!(load(root.path()).is_err());
    let cache = root.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let path = cache.join(CACHE_FILE);
    let empty = json!({"version":1,"fetched_at":"2026-11-04T00:00:00Z","models":[]});
    std::fs::write(&path, empty.to_string()).unwrap();
    assert!(load(root.path()).unwrap().models.is_empty());
    let document = Document {
        version: 1,
        fetched_at: "2026-11-04T00:00:00Z".into(),
        models: vec![AccountModel {
            slug: "m".into(),
            display_name: "Account model".into(),
        }],
    };
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let catalog = load(root.path()).unwrap();
    assert_eq!(catalog.models.get("m").unwrap(), "Account model");
    assert_eq!(
        catalog.fetched_at,
        SystemTime::from(chrono::DateTime::parse_from_rfc3339(&document.fetched_at).unwrap())
    );
    for invalid in [
        "{bad".into(),
        json!({"version":2,"fetched_at":"2026-11-04T00:00:00Z","models":[]}).to_string(),
        json!({"version":1,"fetched_at":"bad","models":[]}).to_string(),
        json!({"version":1,"fetched_at":"2026-11-04T00:00:00Z","models":[{"slug":"m","display_name":"One"},{"slug":"m","display_name":"Two"}]}).to_string(),
        json!({"version":1,"fetched_at":"2026-11-04T00:00:00Z","models":[["m","Name"]]}).to_string(),
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert!(load(root.path()).is_err());
    }
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(fetch::MAX_RESPONSE_BYTES as u64 + 1).unwrap();
    assert!(matches!(load(root.path()), Err(MetadataError::TooLarge)));
}
