//! Several arcana processes (server, stdio MCP server, CLI) commit to one
//! vault repo. Each `VaultGit` handle below stands in for one process.
use arcana_core::config::GitConfig;
use arcana_core::git::VaultGit;
use std::path::{Path, PathBuf};

fn cfg() -> GitConfig {
    GitConfig {
        enabled: true,
        auto_commit: true,
        commit_interval_secs: 300,
        user_name: "human".into(),
        user_email: "human@example.com".into(),
        ai_name: "arcana-ai".into(),
        ai_email: "ai@arcana.local".into(),
    }
}

fn head_blob(root: &Path, rel: &str) -> Option<String> {
    let repo = git2::Repository::open(root).unwrap();
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    let entry = tree.get_path(Path::new(rel)).ok()?;
    let blob = repo.find_blob(entry.id()).unwrap();
    Some(String::from_utf8(blob.content().to_vec()).unwrap())
}

#[test]
fn commit_in_one_process_does_not_delete_another_process_ai_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (a, _) = VaultGit::open_or_init(root, &cfg()).unwrap();
    let (b, _) = VaultGit::open_or_init(root, &cfg()).unwrap();
    std::fs::write(root.join("human.md"), "mine\n").unwrap();
    b.commit_human_change(&[PathBuf::from("human.md")]).unwrap();

    std::fs::write(root.join("ai.md"), "ai text\n").unwrap();
    a.commit_ai_write(&[Path::new("ai.md")], "arcana: create ai.md")
        .unwrap();

    std::fs::write(root.join("human.md"), "mine, edited\n").unwrap();
    b.commit_human_change(&[PathBuf::from("human.md")]).unwrap();

    assert_eq!(head_blob(root, "ai.md").as_deref(), Some("ai text\n"));
    assert_eq!(
        b.adopt_untracked().unwrap(),
        None,
        "nothing should be re-adopted"
    );
    let p = b.provenance("ai.md").unwrap();
    assert_eq!((p.ai_lines, p.human_lines), (1, 0));
}

#[test]
fn commit_in_one_process_does_not_revert_another_process_update() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (a, _) = VaultGit::open_or_init(root, &cfg()).unwrap();
    std::fs::write(root.join("note.md"), "v1\n").unwrap();
    a.commit_ai_write(&[Path::new("note.md")], "create")
        .unwrap();
    let (b, _) = VaultGit::open_or_init(root, &cfg()).unwrap();

    std::fs::write(root.join("note.md"), "v2\n").unwrap();
    a.commit_ai_write(&[Path::new("note.md")], "update")
        .unwrap();

    std::fs::write(root.join("other.md"), "x\n").unwrap();
    b.commit_human_change(&[PathBuf::from("other.md")]).unwrap();

    assert_eq!(head_blob(root, "note.md").as_deref(), Some("v2\n"));
}

#[test]
fn ai_commit_contains_only_the_named_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (a, _) = VaultGit::open_or_init(root, &cfg()).unwrap();
    std::fs::write(root.join("unrelated.md"), "human, uncommitted\n").unwrap();
    std::fs::write(root.join("ai.md"), "ai\n").unwrap();
    a.commit_ai_write(&[Path::new("ai.md")], "create").unwrap();
    assert!(head_blob(root, "unrelated.md").is_none());
}

#[test]
fn human_commit_message_uses_vault_relative_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (g, _) = VaultGit::open_or_init(root, &cfg()).unwrap();
    std::fs::create_dir_all(root.join("notes")).unwrap();
    std::fs::write(root.join("notes/a.md"), "a\n").unwrap();
    g.commit_human_change(&[root.join("notes/a.md")]).unwrap();
    let repo = git2::Repository::open(root).unwrap();
    let msg = repo
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .message()
        .unwrap()
        .to_string();
    assert_eq!(msg, "vault: update notes/a.md");
}

#[test]
fn ledger_sync_commits_changes_no_process_remembered() {
    use arcana_core::attr::{AgentIdentity, Grant, Ledger};
    use arcana_core::config::LedgerConfig;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (git, _) = VaultGit::open_or_init(root, &cfg()).unwrap();
    let ledger = Ledger::open(root, &LedgerConfig::default()).unwrap();
    let agent = Grant::agent(AgentIdentity {
        agent: "a".into(),
        session: "s".into(),
    });
    ledger
        .agent_create(
            None,
            Some("old/n.md"),
            "T",
            &Default::default(),
            Some("Agent words here."),
            None,
            agent,
            Some(&git),
        )
        .unwrap();
    // Moved while nothing was watching (or the watcher restarted).
    std::fs::create_dir_all(root.join("new")).unwrap();
    std::fs::rename(root.join("old/n.md"), root.join("new/n.md")).unwrap();
    std::fs::write(root.join("mine.md"), "Typed in Obsidian.\n").unwrap();

    let (paths, err) = ledger.sync(&git).unwrap();
    assert!(err.is_none(), "{err:?}");
    assert!(!paths.is_empty());
    let left = git.changed_paths().unwrap();
    assert!(
        left.iter()
            .all(|p| p.starts_with(".arcana") && !p.starts_with(".arcana/attr")),
        "notes and sidecars committed; left: {left:?}"
    );
    let moved = ledger.state("new/n.md").unwrap();
    assert!(moved
        .attribution
        .tokens
        .iter()
        .all(|t| moved.attribution.author_of(t).is_agent()));
    let mine = ledger.state("mine.md").unwrap();
    assert!(mine
        .attribution
        .tokens
        .iter()
        .all(|t| mine.attribution.author_of(t).is_human()));
}
