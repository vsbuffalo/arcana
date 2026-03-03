use std::path::PathBuf;

use arcana_core::{ArcanaConfig, Frontmatter, SearchFilters, SearchQuery, Vault};

fn fixture_vault_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/small_vault")
}

fn open_test_vault() -> Vault {
    let config = ArcanaConfig::default().with_vault_path(fixture_vault_path());
    Vault::open_in_memory(config).expect("failed to open test vault")
}

#[test]
fn full_index_and_stats() {
    let vault = open_test_vault();
    let stats = vault.index().unwrap();

    // We have 9 .md files in the fixture (excluding .obsidian)
    assert_eq!(stats.notes_scanned, 9);
    assert_eq!(stats.notes_added, 9);
    assert_eq!(stats.notes_removed, 0);
    assert_eq!(stats.notes_unchanged, 0);

    let vault_stats = vault.stats().unwrap();
    assert_eq!(vault_stats.total_notes, 9);
    assert!(vault_stats.total_tags > 0);
    assert!(vault_stats.total_links > 0);
}

#[test]
fn incremental_reindex_skips_unchanged() {
    let vault = open_test_vault();

    // First index
    let stats1 = vault.index().unwrap();
    assert_eq!(stats1.notes_added, 9);

    // Second index — everything should be unchanged
    let stats2 = vault.index().unwrap();
    assert_eq!(stats2.notes_added, 0);
    assert_eq!(stats2.notes_updated, 0);
    assert_eq!(stats2.notes_unchanged, 9);
}

#[test]
fn keyword_search() {
    let vault = open_test_vault();
    vault.index().unwrap();

    let results = vault
        .search(&SearchQuery {
            text: "async".to_string(),
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();

    assert!(!results.is_empty());
    // The rust-async note should be in results
    assert!(results.iter().any(|r| r.path.contains("rust-async")));
}

#[test]
fn body_content_search() {
    let vault = open_test_vault();
    vault.index().unwrap();

    let results = vault
        .search(&SearchQuery {
            text: "futures".to_string(),
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();

    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r.path.contains("rust-async")));
}

#[test]
fn tag_filter_search() {
    let vault = open_test_vault();
    vault.index().unwrap();

    let results = vault
        .search(&SearchQuery {
            text: "rust".to_string(),
            limit: Some(10),
            filters: SearchFilters {
                tags: vec!["programming".to_string()],
                ..Default::default()
            },
        })
        .unwrap();

    assert!(!results.is_empty());
    // Should find rust-related notes with "programming" tag
    for r in &results {
        assert!(
            r.path.contains("rust-async") || r.path.contains("rust-error"),
            "unexpected result: {}",
            r.path
        );
    }
}

#[test]
fn path_prefix_filter() {
    let vault = open_test_vault();
    vault.index().unwrap();

    let results = vault
        .list(
            &SearchFilters {
                path_prefix: Some("research/".to_string()),
                ..Default::default()
            },
            100,
        )
        .unwrap();

    assert_eq!(results.len(), 3); // rust-async, rust-error-handling, sqlite-fts5
    for r in &results {
        assert!(r.path.starts_with("research/"), "unexpected: {}", r.path);
    }
}

#[test]
fn search_empty_results() {
    let vault = open_test_vault();
    vault.index().unwrap();

    let results = vault
        .search(&SearchQuery {
            text: "xyznonexistent123".to_string(),
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();

    assert!(results.is_empty());
}

#[test]
fn search_special_characters() {
    let vault = open_test_vault();
    vault.index().unwrap();

    // These should not crash — the query sanitizer handles them
    for query in &["C++", "what's new", "self-attention", "\"quoted\"", "term*"] {
        let result = vault.search(&SearchQuery {
            text: query.to_string(),
            limit: Some(5),
            ..Default::default()
        });
        assert!(result.is_ok(), "query '{}' failed: {:?}", query, result);
    }
}

#[test]
fn read_note() {
    let vault = open_test_vault();
    let note = vault.read_note("research/rust-async.md").unwrap();

    assert_eq!(
        note.frontmatter.title.as_deref(),
        Some("Rust Async Programming")
    );
    assert!(note.frontmatter.tags.contains(&"rust".to_string()));
    assert!(note.body.contains("Async/await"));
}

#[test]
fn read_note_not_found() {
    let vault = open_test_vault();
    let result = vault.read_note("nonexistent.md");
    assert!(result.is_err());
}

#[test]
fn create_then_search() {
    let dir = tempfile::tempdir().unwrap();

    // Copy fixture vault to temp dir so we can modify it
    let fixture = fixture_vault_path();
    copy_dir(&fixture, dir.path());

    let config = ArcanaConfig::default().with_vault_path(dir.path().to_path_buf());
    let vault = Vault::open_in_memory(config).unwrap();
    vault.index().unwrap();

    // Create a new note
    let fm = Frontmatter {
        title: Some("Quantum Computing".to_string()),
        tags: vec!["quantum".to_string(), "physics".to_string()],
        ..Default::default()
    };

    vault
        .create_note(
            "research/quantum.md",
            "# Quantum Computing\n\nQubits and superposition are fundamental concepts.\n",
            Some(fm),
        )
        .unwrap();

    // Search for it
    let results = vault
        .search(&SearchQuery {
            text: "quantum".to_string(),
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();

    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r.path.contains("quantum")));
}

#[test]
fn ranking_title_over_body() {
    let vault = open_test_vault();
    vault.index().unwrap();

    // "SQLite" appears in the title of sqlite-fts5.md and in body of project-alpha.md
    let results = vault
        .search(&SearchQuery {
            text: "sqlite".to_string(),
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();

    assert!(!results.is_empty());
    // The note with "SQLite" in the title should rank higher
    if results.len() >= 2 {
        assert!(
            results[0].path.contains("sqlite"),
            "expected sqlite note first, got: {}",
            results[0].path
        );
    }
}

#[test]
fn no_frontmatter_note_indexed() {
    let vault = open_test_vault();
    vault.index().unwrap();

    let results = vault
        .search(&SearchQuery {
            text: "plain markdown".to_string(),
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();

    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r.path.contains("no-frontmatter")));
}

#[test]
fn extra_frontmatter_preserved() {
    let vault = open_test_vault();
    let note = vault.read_note("projects/project-beta.md").unwrap();

    assert_eq!(
        note.frontmatter.extra.get("custom_field"),
        Some(&serde_yaml::Value::String("some-value".to_string()))
    );
    assert_eq!(
        note.frontmatter.extra.get("priority"),
        Some(&serde_yaml::Value::String("high".to_string()))
    );
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    for entry in walkdir::WalkDir::new(src).min_depth(1) {
        let entry = entry.unwrap();
        let rel = entry.path().strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target).ok();
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}
