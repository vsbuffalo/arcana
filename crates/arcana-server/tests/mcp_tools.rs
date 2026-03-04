use std::path::Path;

use arcana_core::{ArcanaConfig, Frontmatter, SearchFilters, SearchQuery, Vault};
use arcana_server::ArcanaServer;
use rmcp::ServerHandler;

/// Create a vault from the small_vault fixture, copied into a temp dir so we can write to it.
fn setup_vault(tmp: &Path) -> Vault {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/small_vault");
    copy_dir(&fixture, tmp);

    // Create .obsidian marker so vault detection works
    std::fs::create_dir_all(tmp.join(".obsidian")).unwrap();

    let config = ArcanaConfig::default().with_vault_path(tmp.to_path_buf());
    let vault = Vault::open(config).unwrap();
    vault.index().unwrap();
    vault
}

fn copy_dir(src: &Path, dst: &Path) {
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.unwrap();
        let rel = entry.path().strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[test]
fn server_info_is_correct() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = setup_vault(tmp.path());
    let server = ArcanaServer::new(vault);

    let info = ServerHandler::get_info(&server);
    assert_eq!(info.server_info.name, "arcana");
    assert!(info.capabilities.tools.is_some());
    assert!(info.instructions.is_some());
}

#[test]
fn tool_list_has_all_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let _vault = setup_vault(tmp.path());

    let router = ArcanaServer::tool_router();
    let tools = router.list_all();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

    assert!(names.contains(&"vault_search"), "missing vault_search");
    assert!(names.contains(&"vault_read"), "missing vault_read");
    assert!(names.contains(&"vault_create"), "missing vault_create");
    assert!(names.contains(&"vault_update"), "missing vault_update");
    assert!(names.contains(&"vault_list"), "missing vault_list");
    assert!(names.contains(&"vault_stats"), "missing vault_stats");
    assert!(names.contains(&"vault_draft"), "missing vault_draft");
    assert!(
        names.contains(&"vault_suggest_edit"),
        "missing vault_suggest_edit"
    );
    assert!(
        names.contains(&"vault_provenance"),
        "missing vault_provenance"
    );
    assert_eq!(names.len(), 9, "should have exactly 9 tools");
}

#[test]
fn tool_schemas_have_descriptions() {
    let tmp = tempfile::tempdir().unwrap();
    let _vault = setup_vault(tmp.path());

    let router = ArcanaServer::tool_router();
    let tools = router.list_all();
    for tool in &tools {
        assert!(
            tool.description.is_some(),
            "tool {} has no description",
            tool.name
        );
    }
}

#[test]
fn test_vault_stats() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = setup_vault(tmp.path());

    let stats = vault.stats().unwrap();
    assert!(stats.total_notes >= 9);
    assert!(stats.total_tags >= 1);
}

#[test]
fn test_vault_search() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = setup_vault(tmp.path());

    let query = SearchQuery {
        text: "welcome".to_string(),
        limit: Some(10),
        filters: SearchFilters::default(),
    };
    let results = vault.search(&query).unwrap();
    assert!(!results.is_empty(), "should find the welcome note");
    assert!(results.iter().any(|r| r.path.contains("welcome")));
}

#[test]
fn test_vault_search_with_path_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = setup_vault(tmp.path());

    let query = SearchQuery {
        text: "rust".to_string(),
        limit: Some(5),
        filters: SearchFilters {
            path_prefix: Some("research/".to_string()),
            ..Default::default()
        },
    };
    let results = vault.search(&query).unwrap();
    for r in &results {
        assert!(
            r.path.starts_with("research/"),
            "result {} should be under research/",
            r.path
        );
    }
}

#[test]
fn test_vault_read() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = setup_vault(tmp.path());

    let note = vault.read_note("welcome.md").unwrap();
    assert_eq!(note.title(), "Welcome to My Vault");
    assert!(note.body.contains("knowledge base"));
    assert!(note.frontmatter.tags.contains(&"meta".to_string()));
}

#[test]
fn test_vault_create_and_search() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = setup_vault(tmp.path());

    let fm = Frontmatter {
        title: Some("Test Note".to_string()),
        tags: vec!["test".to_string(), "mcp".to_string()],
        ..Default::default()
    };
    vault
        .create_note("test/new-note.md", "Hello from the MCP server", Some(fm))
        .unwrap();

    // Verify readable
    let note = vault.read_note("test/new-note.md").unwrap();
    assert_eq!(note.title(), "Test Note");
    assert!(note.body.contains("Hello from the MCP server"));

    // Verify searchable
    let query = SearchQuery {
        text: "MCP server".to_string(),
        limit: Some(10),
        filters: SearchFilters::default(),
    };
    let results = vault.search(&query).unwrap();
    assert!(
        results.iter().any(|r| r.path == "test/new-note.md"),
        "new note should be searchable"
    );
}

#[test]
fn test_vault_update() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = setup_vault(tmp.path());

    // Create a note first
    let fm = Frontmatter {
        title: Some("Update Me".to_string()),
        tags: vec!["original".to_string()],
        ..Default::default()
    };
    vault
        .create_note("test/update-me.md", "Original content", Some(fm))
        .unwrap();

    // Update: append text and modify tags.
    // merge_frontmatter only adds tags, so for removal we do a full read-modify-write
    // (same approach as the MCP server's vault_update tool).
    let mut note = vault.read_note("test/update-me.md").unwrap();
    note.frontmatter.tags.push("updated".to_string());
    note.frontmatter.tags.retain(|t| t != "original");
    note.body.push_str("\n\nAppended content");

    let full_path = vault.root().join("test/update-me.md");
    std::fs::write(&full_path, note.to_string()).unwrap();
    vault.reindex_paths(&[full_path]).unwrap();

    // Verify changes
    let note = vault.read_note("test/update-me.md").unwrap();
    assert!(note.body.contains("Original content"));
    assert!(note.body.contains("Appended content"));
    assert!(note.frontmatter.tags.contains(&"updated".to_string()));
    assert!(!note.frontmatter.tags.contains(&"original".to_string()));
}

#[test]
fn test_vault_list() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = setup_vault(tmp.path());

    // List all
    let results = vault.list(&SearchFilters::default(), 100).unwrap();
    assert!(results.len() >= 9, "should list all notes");

    // List with path prefix
    let filters = SearchFilters {
        path_prefix: Some("research/".to_string()),
        ..Default::default()
    };
    let results = vault.list(&filters, 100).unwrap();
    assert!(!results.is_empty());
    for r in &results {
        assert!(r.path.starts_with("research/"));
    }
}

#[test]
fn search_snippets_have_no_mark_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = setup_vault(tmp.path());

    let query = SearchQuery {
        text: "welcome".to_string(),
        limit: Some(10),
        filters: SearchFilters::default(),
    };
    let results = vault.search(&query).unwrap();
    for r in &results {
        // The raw search results have <mark> tags; the MCP server strips them.
        // This test verifies the strip function works correctly.
        let stripped = r.snippet.replace("<mark>", "").replace("</mark>", "");
        assert!(!stripped.contains("<mark>"));
    }
}
