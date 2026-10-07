use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use git2::{DiffOptions, ErrorCode, Oid, Repository, Signature, Sort, StatusOptions, StatusShow};
use serde::Serialize;
use tracing::{debug, info, warn};

use crate::config::GitConfig;
use crate::errors::{ArcanaError, Result};

const IDENTITY_ERROR: &str = "\
git user identity not configured. Fix with any of:

  arcana --name \"Your Name\" --email \"you@example.com\" ...

  export ARCANA_USER_NAME=\"Your Name\"
  export ARCANA_USER_EMAIL=\"you@example.com\"

  # .arcana/config.toml
  [git]
  user_name = \"Your Name\"
  user_email = \"you@example.com\"

  git config --global user.name \"Your Name\"
  git config --global user.email \"you@example.com\"";

/// Check that a human identity is available from either git config or the arcana config.
fn check_identity(repo: &Repository, config: &GitConfig) -> Result<()> {
    if repo.signature().is_ok() {
        return Ok(());
    }
    if !config.user_name.is_empty() && !config.user_email.is_empty() {
        return Ok(());
    }
    Err(ArcanaError::Config(IDENTITY_ERROR.to_string()))
}

/// Provenance author identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvenanceAuthor {
    Human,
    Ai,
    Unknown,
}

impl ProvenanceAuthor {
    pub fn from_email(email: &str) -> Self {
        if email.ends_with("@arcana.local") {
            ProvenanceAuthor::Ai
        } else {
            ProvenanceAuthor::Human
        }
    }
}

/// Per-line provenance from git blame.
#[derive(Debug, Clone, Serialize)]
pub struct LineProvenance {
    pub line_no: usize,
    pub author: ProvenanceAuthor,
    pub author_name: String,
    pub author_email: String,
    pub commit_id: String,
    pub content: String,
}

/// Aggregate provenance for a note.
#[derive(Debug, Clone, Serialize)]
pub struct NoteProvenance {
    pub path: String,
    pub total_lines: usize,
    pub human_lines: usize,
    pub ai_lines: usize,
    pub human_pct: f64,
    pub ai_pct: f64,
}

/// A single commit entry from the log.
#[derive(Debug, Clone, Serialize)]
pub struct CommitInfo {
    pub id: String,
    pub author_name: String,
    pub author_email: String,
    pub author: ProvenanceAuthor,
    pub message: String,
    pub timestamp: i64,
}

/// Information about what happened during git initialization.
#[derive(Debug, Clone, Default)]
pub struct InitInfo {
    pub newly_created: bool,
    pub adopted_files: usize,
}

/// Git integration for dual-author provenance.
pub struct VaultGit {
    repo: Repository,
    config: GitConfig,
    /// Paths recently written by AI, cleared when commit_human_change drains them.
    pending_ai_writes: Mutex<HashSet<PathBuf>>,
}

impl VaultGit {
    /// Open the existing repo or initialize a new one in the vault root.
    pub fn open_or_init(vault_root: &Path, config: &GitConfig) -> Result<(Self, InitInfo)> {
        let (repo, newly_created) = match Repository::open(vault_root) {
            Ok(repo) => {
                debug!("opened existing git repo at {}", vault_root.display());
                (repo, false)
            }
            Err(e) if e.code() == ErrorCode::NotFound => {
                info!("initializing git repo at {}", vault_root.display());
                let repo = Repository::init(vault_root).map_err(git_err)?;

                // Create .gitignore
                let gitignore = vault_root.join(".gitignore");
                if !gitignore.exists() {
                    std::fs::write(
                        &gitignore,
                        ".arcana/index.db\n\
                         .arcana/index.db-wal\n\
                         .arcana/index.db-shm\n\
                         .arcana/cache/\n\
                         .arcana/chat_history\n\
                         .claude/\n\
                         .obsidian/workspace*.json\n",
                    )?;
                }

                // Validate identity before first commit
                check_identity(&repo, config)?;

                // Initial commit
                let sig = repo.signature().unwrap_or_else(|_| {
                    Signature::now(&config.user_name, &config.user_email)
                        .expect("valid signature (already validated)")
                });

                {
                    let mut index = repo.index().map_err(git_err)?;
                    index.add_path(Path::new(".gitignore")).map_err(git_err)?;
                    index.write().map_err(git_err)?;
                    let tree_oid = index.write_tree().map_err(git_err)?;
                    let tree = repo.find_tree(tree_oid).map_err(git_err)?;

                    repo.commit(
                        Some("HEAD"),
                        &sig,
                        &sig,
                        "arcana: init vault",
                        &tree,
                        &[], // no parents for initial commit
                    )
                    .map_err(git_err)?;
                }

                (repo, true)
            }
            Err(e) => return Err(git_err(e)),
        };

        let git = Self {
            repo,
            config: config.clone(),
            pending_ai_writes: Mutex::new(HashSet::new()),
        };

        let mut init_info = InitInfo {
            newly_created,
            adopted_files: 0,
        };

        if newly_created {
            // Adopt any existing untracked files as human-authored.
            // Use index.add_all to recurse into directories and respect .gitignore.
            let mut index = git.repo.index().map_err(git_err)?;
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .map_err(git_err)?;

            // Count how many new entries were staged (excluding .gitignore which is already committed)
            let mut opts = StatusOptions::new();
            opts.show(StatusShow::Index);
            let statuses = git.repo.statuses(Some(&mut opts)).map_err(git_err)?;
            let count = statuses
                .iter()
                .filter(|s| s.status().contains(git2::Status::INDEX_NEW))
                .count();

            if count > 0 {
                index.write().map_err(git_err)?;
                let tree_oid = index.write_tree().map_err(git_err)?;
                let tree = git.repo.find_tree(tree_oid).map_err(git_err)?;
                let sig = git.human_signature()?;
                let parent = git.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
                let parents: Vec<&git2::Commit<'_>> = parent.as_ref().into_iter().collect();
                let msg = format!("arcana: adopt {count} existing notes");
                git.repo
                    .commit(Some("HEAD"), &sig, &sig, &msg, &tree, &parents)
                    .map_err(git_err)?;
                init_info.adopted_files = count;
                info!("adopted {count} existing files as human-authored");
            }
        }

        Ok((git, init_info))
    }

    fn ai_signature(&self) -> std::result::Result<Signature<'_>, git2::Error> {
        Signature::now(&self.config.ai_name, &self.config.ai_email)
    }

    fn human_signature(&self) -> Result<Signature<'_>> {
        // Try git config first, then fall back to configured values
        if let Ok(sig) = self.repo.signature() {
            return Ok(sig);
        }
        if !self.config.user_name.is_empty() && !self.config.user_email.is_empty() {
            return Signature::now(&self.config.user_name, &self.config.user_email)
                .map_err(git_err);
        }
        Err(ArcanaError::Config(IDENTITY_ERROR.to_string()))
    }

    /// Stage and commit paths as an AI write.
    pub fn commit_ai_write(&self, paths: &[&Path], message: &str) -> Result<Oid> {
        let oid = self.stage_and_commit(paths, message, Author::Ai)?;

        // Track these paths so commit_human_change won't re-attribute them
        let mut pending = self.pending_ai_writes.lock().unwrap();
        let workdir = self.repo.workdir().unwrap_or(Path::new("."));
        for path in paths {
            let abs = if path.is_relative() {
                workdir.join(path)
            } else {
                path.to_path_buf()
            };
            pending.insert(abs);
        }

        Ok(oid)
    }

    /// Stage and commit paths as a human change.
    /// Filters out paths that are in `pending_ai_writes`.
    pub fn commit_human_change(&self, paths: &[PathBuf]) -> Result<Option<Oid>> {
        let mut pending = self.pending_ai_writes.lock().unwrap();
        let workdir = self.repo.workdir().unwrap_or(Path::new("."));

        let human_paths: Vec<&Path> = paths
            .iter()
            .filter(|p| {
                let abs = if p.is_relative() {
                    workdir.join(p)
                } else {
                    p.to_path_buf()
                };
                if pending.remove(&abs) {
                    debug!("skipping AI-written path: {}", abs.display());
                    false
                } else {
                    true
                }
            })
            .map(|p| p.as_path())
            .collect();

        if human_paths.is_empty() {
            return Ok(None);
        }

        let rels: Vec<PathBuf> = human_paths.iter().map(|p| self.relative(p)).collect();
        let names: Vec<&str> = rels.iter().filter_map(|p| p.to_str()).collect();
        let message = match names.len() {
            0 => "vault: update notes".to_string(),
            1 => format!("vault: update {}", names[0]),
            2 => format!("vault: update {}, {}", names[0], names[1]),
            n => format!("vault: update {}, {} (+{} more)", names[0], names[1], n - 2),
        };

        let oid = self.stage_and_commit(&human_paths, &message, Author::Human)?;
        Ok(Some(oid))
    }

    /// Remove a stale `.git/index.lock` if it exists and is older than `max_age`.
    ///
    /// In a single-process system where all git access is serialized by a mutex,
    /// a lock file older than a few seconds is definitionally stale — left behind
    /// by a crash or SIGKILL. Safe to remove.
    fn clear_stale_index_lock(&self, max_age: std::time::Duration) -> Result<()> {
        let workdir = self.repo.workdir().unwrap_or(Path::new("."));
        let lock_path = workdir.join(".git/index.lock");
        if lock_path.exists() {
            let stale = lock_path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > max_age);
            if stale {
                warn!("removing stale index.lock (age > {}s)", max_age.as_secs());
                std::fs::remove_file(&lock_path)?;
            }
        }
        Ok(())
    }

    /// Commit exactly `paths` with the given author class. Used by the
    /// attribution ledger, which records who wrote each word itself; the git
    /// author is a summary, not the source of truth.
    pub fn commit_paths(&self, paths: &[&Path], message: &str, human: bool) -> Result<Oid> {
        let author = if human { Author::Human } else { Author::Ai };
        self.stage_and_commit(paths, message, author)
    }

    /// Adopt untracked `.md` files as human-authored and commit them.
    ///
    /// This catches files created outside arcana (by Obsidian, manual editing, etc.)
    /// that were never `git add`ed. Returns the number of files adopted, or None
    /// if there was nothing to adopt.
    pub fn adopt_untracked(&self) -> Result<Option<usize>> {
        let mut opts = StatusOptions::new();
        opts.show(StatusShow::Workdir);
        opts.include_untracked(true);
        opts.recurse_untracked_dirs(true);

        let statuses = self.repo.statuses(Some(&mut opts)).map_err(git_err)?;
        let untracked: Vec<PathBuf> = statuses
            .iter()
            .filter(|s| s.status().contains(git2::Status::WT_NEW))
            .filter_map(|s| s.path().map(PathBuf::from))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .collect();

        if untracked.is_empty() {
            return Ok(None);
        }

        let count = untracked.len();
        let refs: Vec<&Path> = untracked.iter().map(|p| p.as_path()).collect();
        let msg = format!(
            "vault: adopt {count} untracked note{}",
            if count == 1 { "" } else { "s" }
        );
        self.stage_and_commit(&refs, &msg, Author::Human)?;
        info!("adopted {count} untracked files as human-authored");
        Ok(Some(count))
    }

    /// Commit exactly `paths`, as they are on disk now, on top of the current HEAD.
    ///
    /// Several arcana processes share this repo (the server, a stdio MCP
    /// server, the CLI), and libgit2's `repo.index()` is a per-process copy of
    /// `.git/index` that is never re-read. Building the tree from that copy let
    /// one process's commit silently revert or delete files another process had
    /// committed. So the tree is built from HEAD's tree plus the named paths
    /// only, in a throwaway in-memory index, and the commit is retried if
    /// another process moved HEAD in between (libgit2 refuses to update HEAD
    /// when its tip is no longer the first parent).
    fn stage_and_commit(&self, paths: &[&Path], message: &str, author: Author) -> Result<Oid> {
        const MAX_ATTEMPTS: usize = 5;

        let workdir = self
            .repo
            .workdir()
            .ok_or_else(|| ArcanaError::Config("bare repository not supported".into()))?;
        let rels: Vec<PathBuf> = paths.iter().map(|p| self.relative(p)).collect();

        let sig = match author {
            Author::Ai => self.ai_signature().map_err(git_err)?,
            Author::Human => self.human_signature()?,
        };

        for attempt in 1..=MAX_ATTEMPTS {
            let parent = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
            let tree_oid = self.tree_with_paths(workdir, parent.as_ref(), &rels)?;

            if let Some(p) = &parent {
                if p.tree_id() == tree_oid {
                    debug!("nothing to commit for: {message}");
                    return Ok(p.id());
                }
            }

            let tree = self.repo.find_tree(tree_oid).map_err(git_err)?;
            let parents: Vec<&git2::Commit<'_>> = parent.as_ref().into_iter().collect();
            match self
                .repo
                .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            {
                Ok(oid) => {
                    debug!("committed {}: {}", &oid.to_string()[..8], message);
                    if let Err(e) = self.sync_disk_index(workdir, &rels) {
                        // History is already correct; a stale index only affects `git status`.
                        warn!("committed {oid} but failed to update .git/index: {e}");
                    }
                    return Ok(oid);
                }
                Err(e) if e.code() == ErrorCode::Modified && attempt < MAX_ATTEMPTS => {
                    debug!("HEAD moved during commit (attempt {attempt}), rebuilding tree");
                }
                Err(e) => return Err(git_err(e)),
            }
        }
        unreachable!("loop returns on success, error, or final attempt")
    }

    /// Vault-relative form of `path` (accepts absolute paths under the workdir).
    fn relative(&self, path: &Path) -> PathBuf {
        if path.is_relative() {
            return path.to_path_buf();
        }
        let workdir = self.repo.workdir().unwrap_or(Path::new("."));
        if let Ok(rel) = path.strip_prefix(workdir) {
            return rel.to_path_buf();
        }
        // libgit2's workdir is canonical (e.g. /private/tmp on macOS) while the
        // caller's path may go through a symlink. Canonicalize the parent, since
        // the file itself may have been deleted.
        path.parent()
            .and_then(|parent| parent.canonicalize().ok())
            .zip(path.file_name())
            .and_then(|(parent, name)| {
                parent
                    .join(name)
                    .strip_prefix(workdir)
                    .ok()
                    .map(Path::to_path_buf)
            })
            .unwrap_or_else(|| path.to_path_buf())
    }

    /// Write the tree of `parent` with `rels` replaced by their on-disk content
    /// (or removed, if they no longer exist). Touches no shared state.
    fn tree_with_paths(
        &self,
        workdir: &Path,
        parent: Option<&git2::Commit<'_>>,
        rels: &[PathBuf],
    ) -> Result<Oid> {
        let mut index = git2::Index::new().map_err(git_err)?;
        if let Some(p) = parent {
            index
                .read_tree(&p.tree().map_err(git_err)?)
                .map_err(git_err)?;
        }
        for rel in rels {
            let abs = workdir.join(rel);
            if abs.is_file() {
                let content = std::fs::read(&abs)?;
                let id = self.repo.blob(&content).map_err(git_err)?;
                index
                    .add(&blob_entry(rel, id, content.len()))
                    .map_err(git_err)?;
            } else {
                match index.remove_path(rel) {
                    Ok(()) => {}
                    Err(e) if e.code() == ErrorCode::NotFound => {}
                    Err(e) => return Err(git_err(e)),
                }
            }
        }
        index.write_tree_to(&self.repo).map_err(git_err)
    }

    /// Bring `.git/index` up to date for `rels` so `git status` agrees with
    /// HEAD. Re-reads the index from disk first; never writes back a stale copy.
    fn sync_disk_index(&self, workdir: &Path, rels: &[PathBuf]) -> Result<()> {
        self.clear_stale_index_lock(std::time::Duration::from_secs(30))?;
        let mut index = self.repo.index().map_err(git_err)?;
        index.read(true).map_err(git_err)?;
        for rel in rels {
            if workdir.join(rel).is_file() {
                index.add_path(rel).map_err(git_err)?;
            } else {
                match index.remove_path(rel) {
                    Ok(()) => {}
                    Err(e) if e.code() == ErrorCode::NotFound => {}
                    Err(e) => return Err(git_err(e)),
                }
            }
        }
        index.write().map_err(git_err)
    }

    /// Git log for a specific file, or all commits if path is None.
    pub fn log(&self, path: Option<&str>, limit: usize) -> Result<Vec<CommitInfo>> {
        let mut revwalk = self.repo.revwalk().map_err(git_err)?;
        revwalk.push_head().map_err(git_err)?;
        revwalk.set_sorting(Sort::TIME).map_err(git_err)?;

        let mut commits = Vec::new();

        for oid_result in revwalk {
            if commits.len() >= limit {
                break;
            }

            let oid = oid_result.map_err(git_err)?;
            let commit = self.repo.find_commit(oid).map_err(git_err)?;

            // If filtering by path, check if this commit touches the file
            if let Some(file_path) = path {
                if !self.commit_touches_path(&commit, file_path)? {
                    continue;
                }
            }

            let author = commit.author();
            let email = author.email().unwrap_or("");
            commits.push(CommitInfo {
                id: oid.to_string(),
                author_name: author.name().unwrap_or("").to_string(),
                author_email: email.to_string(),
                author: ProvenanceAuthor::from_email(email),
                message: commit.message().unwrap_or("").to_string(),
                timestamp: commit.time().seconds(),
            });
        }

        Ok(commits)
    }

    fn commit_touches_path(&self, commit: &git2::Commit<'_>, file_path: &str) -> Result<bool> {
        let tree = commit.tree().map_err(git_err)?;

        let parent_tree = if commit.parent_count() > 0 {
            Some(commit.parent(0).map_err(git_err)?.tree().map_err(git_err)?)
        } else {
            None
        };

        let mut diff_opts = DiffOptions::new();
        diff_opts.pathspec(file_path);

        let diff = self
            .repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut diff_opts))
            .map_err(git_err)?;

        Ok(diff.deltas().count() > 0)
    }

    /// Diff uncommitted changes for a path (or all if None).
    pub fn diff(&self, path: Option<&str>) -> Result<String> {
        let mut opts = DiffOptions::new();
        if let Some(p) = path {
            opts.pathspec(p);
        }

        let head_tree = self.repo.head().ok().and_then(|h| h.peel_to_tree().ok());

        let diff = self
            .repo
            .diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))
            .map_err(git_err)?;

        let mut buf = Vec::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' => "+",
                '-' => "-",
                ' ' => " ",
                _ => "",
            };
            buf.extend_from_slice(prefix.as_bytes());
            buf.extend_from_slice(line.content());
            true
        })
        .map_err(git_err)?;

        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    /// Line-level blame for a file.
    pub fn blame(&self, path: &str) -> Result<Vec<LineProvenance>> {
        // `path` reaches `workdir.join(path)` and is read from disk below; reject
        // absolute paths and `..` traversal before touching the filesystem.
        crate::vault_path::validate_rel(path)?;
        let blame = self
            .repo
            .blame_file(Path::new(path), None)
            .map_err(git_err)?;
        let workdir = self.repo.workdir().unwrap_or(Path::new("."));
        let full_path = workdir.join(path);
        let content = std::fs::read_to_string(&full_path)?;
        let lines: Vec<&str> = content.lines().collect();

        let mut result = Vec::with_capacity(lines.len());
        for (i, line_content) in lines.iter().enumerate() {
            let hunk = blame.get_line(i + 1); // blame is 1-indexed
            if let Some(hunk) = hunk {
                let sig = hunk.final_signature();
                let email = sig.email().unwrap_or("");
                result.push(LineProvenance {
                    line_no: i + 1,
                    author: ProvenanceAuthor::from_email(email),
                    author_name: sig.name().unwrap_or("").to_string(),
                    author_email: email.to_string(),
                    commit_id: hunk.final_commit_id().to_string(),
                    content: line_content.to_string(),
                });
            }
        }

        Ok(result)
    }

    /// Aggregate provenance stats for a note.
    pub fn provenance(&self, path: &str) -> Result<NoteProvenance> {
        let lines = self.blame(path)?;
        let total = lines.len();
        let human = lines
            .iter()
            .filter(|l| l.author == ProvenanceAuthor::Human)
            .count();
        let ai = lines
            .iter()
            .filter(|l| l.author == ProvenanceAuthor::Ai)
            .count();

        Ok(NoteProvenance {
            path: path.to_string(),
            total_lines: total,
            human_lines: human,
            ai_lines: ai,
            human_pct: if total > 0 {
                human as f64 / total as f64 * 100.0
            } else {
                0.0
            },
            ai_pct: if total > 0 {
                ai as f64 / total as f64 * 100.0
            } else {
                0.0
            },
        })
    }

    /// A file's content at a commit, if it existed there.
    pub fn file_at(&self, path: &str, commit_id: &str) -> Result<Option<Vec<u8>>> {
        crate::vault_path::validate_rel(path)?;
        let oid = self
            .repo
            .revparse_single(commit_id)
            .map_err(|e| ArcanaError::Config(format!("invalid commit {commit_id}: {e}")))?
            .peel_to_commit()
            .map_err(git_err)?
            .id();
        let tree = self
            .repo
            .find_commit(oid)
            .and_then(|c| c.tree())
            .map_err(git_err)?;
        match tree.get_path(Path::new(path)) {
            Ok(entry) => Ok(Some(
                self.repo
                    .find_blob(entry.id())
                    .map_err(git_err)?
                    .content()
                    .to_vec(),
            )),
            Err(_) => Ok(None),
        }
    }

    /// Restore a file to a specific commit's version. Creates a new commit.
    pub fn restore(&self, path: &str, commit_id: &str) -> Result<Oid> {
        // `path` reaches `workdir.join(path)` and is written below; reject
        // absolute paths and `..` traversal before touching the filesystem.
        crate::vault_path::validate_rel(path)?;
        let oid = Oid::from_str(commit_id)
            .map_err(|e| ArcanaError::Config(format!("invalid commit id: {e}")))?;
        let commit = self.repo.find_commit(oid).map_err(git_err)?;
        let tree = commit.tree().map_err(git_err)?;

        let entry = tree
            .get_path(Path::new(path))
            .map_err(|_| ArcanaError::NoteNotFound(format!("{path} at commit {commit_id}")))?;

        let blob = self.repo.find_blob(entry.id()).map_err(git_err)?;
        let content = blob.content();

        let workdir = self.repo.workdir().unwrap_or(Path::new("."));
        let full_path = workdir.join(path);
        std::fs::write(&full_path, content)?;

        let msg = format!(
            "arcana: restore {path} to {}",
            &commit_id[..8.min(commit_id.len())]
        );
        self.stage_and_commit(&[Path::new(path)], &msg, Author::Human)
    }

    /// Check if repo has any uncommitted changes.
    /// Vault-relative paths that differ from HEAD in the working tree:
    /// modified, deleted, or new (untracked, recursing into new folders).
    /// Ignored files are excluded.
    pub fn changed_paths(&self) -> Result<Vec<PathBuf>> {
        let mut opts = StatusOptions::new();
        opts.show(StatusShow::IndexAndWorkdir);
        opts.include_untracked(true);
        opts.recurse_untracked_dirs(true);
        let statuses = self.repo.statuses(Some(&mut opts)).map_err(git_err)?;
        Ok(statuses
            .iter()
            .filter_map(|s| s.path().map(PathBuf::from))
            .collect())
    }

    pub fn has_changes(&self) -> Result<bool> {
        let mut opts = StatusOptions::new();
        opts.show(StatusShow::IndexAndWorkdir);
        opts.include_untracked(true);
        let statuses = self.repo.statuses(Some(&mut opts)).map_err(git_err)?;
        Ok(!statuses.is_empty())
    }
}

enum Author {
    Ai,
    Human,
}

/// Index entry for a regular file blob; stat fields are irrelevant to tree writing.
fn blob_entry(rel: &Path, id: Oid, len: usize) -> git2::IndexEntry {
    let zero = git2::IndexTime::new(0, 0);
    git2::IndexEntry {
        ctime: zero,
        mtime: zero,
        dev: 0,
        ino: 0,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        file_size: len as u32,
        id,
        flags: 0,
        flags_extended: 0,
        path: rel.to_string_lossy().replace('\\', "/").into_bytes(),
    }
}

fn git_err(e: git2::Error) -> ArcanaError {
    ArcanaError::Git(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GitConfig;

    fn test_config() -> GitConfig {
        GitConfig {
            enabled: true,
            auto_commit: true,
            commit_interval_secs: 300,
            user_name: "test-user".to_string(),
            user_email: "test@example.com".to_string(),
            ai_name: "arcana-ai".to_string(),
            ai_email: "ai@arcana.local".to_string(),
        }
    }

    #[test]
    fn open_or_init_creates_repo() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (git, init_info) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        assert!(init_info.newly_created);
        assert_eq!(init_info.adopted_files, 0);

        // Should have .gitignore
        assert!(dir.path().join(".gitignore").exists());

        // Should have initial commit
        let log = git.log(None, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert!(log[0].message.contains("init vault"));
    }

    #[test]
    fn open_or_init_errors_without_identity() {
        // libgit2 falls back to the global git identity; this test can only
        // observe the error on a machine without one.
        let global = git2::Config::open_default().ok();
        if global.is_some_and(|c| c.get_string("user.email").is_ok()) {
            eprintln!("skipped: a global git identity is configured");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let config = GitConfig {
            enabled: true,
            auto_commit: true,
            commit_interval_secs: 300,
            user_name: String::new(),
            user_email: String::new(),
            ai_name: "arcana-ai".to_string(),
            ai_email: "ai@arcana.local".to_string(),
        };
        match VaultGit::open_or_init(dir.path(), &config) {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("git user identity"),
                    "expected identity error, got: {msg}"
                );
            }
            Ok(_) => panic!("expected error for missing identity, got Ok"),
        }
    }

    #[test]
    fn open_or_init_reopens_existing() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        VaultGit::open_or_init(dir.path(), &config).unwrap();
        // Open again — should not fail
        let (git, init_info) = VaultGit::open_or_init(dir.path(), &config).unwrap();
        assert!(!init_info.newly_created);
        assert_eq!(init_info.adopted_files, 0);
        let log = git.log(None, 10).unwrap();
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn open_or_init_adopts_existing_files() {
        let dir = tempfile::tempdir().unwrap();

        // Create some .md files before git init
        std::fs::write(dir.path().join("note-a.md"), "alpha\n").unwrap();
        std::fs::write(dir.path().join("note-b.md"), "beta\n").unwrap();
        std::fs::write(dir.path().join("note-c.md"), "gamma\n").unwrap();

        let config = test_config();
        let (git, init_info) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        assert!(init_info.newly_created);
        assert_eq!(init_info.adopted_files, 3);

        // Should have init commit + adopt commit
        let log = git.log(None, 10).unwrap();
        assert_eq!(log.len(), 2);
        assert!(log[0].message.contains("adopt 3 existing notes"));
        assert_eq!(log[0].author, ProvenanceAuthor::Human);

        // Blame should work on adopted files
        let blame = git.blame("note-a.md").unwrap();
        assert_eq!(blame.len(), 1);
        assert_eq!(blame[0].author, ProvenanceAuthor::Human);
    }

    #[test]
    fn ai_commit_and_blame() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (git, _) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        // Write a file and commit as AI
        let note_path = dir.path().join("test.md");
        std::fs::write(&note_path, "line 1\nline 2\n").unwrap();

        let rel = Path::new("test.md");
        git.commit_ai_write(&[rel], "arcana: create test.md")
            .unwrap();

        // Blame should show AI author
        let blame_result = git.blame("test.md").unwrap();
        assert_eq!(blame_result.len(), 2);
        assert_eq!(blame_result[0].author, ProvenanceAuthor::Ai);
        assert_eq!(blame_result[0].author_email, "ai@arcana.local");
    }

    #[test]
    fn human_commit_filters_ai_writes() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (git, _) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        // AI writes a file
        let note = dir.path().join("ai-note.md");
        std::fs::write(&note, "ai content\n").unwrap();
        git.commit_ai_write(&[Path::new("ai-note.md")], "arcana: create ai-note.md")
            .unwrap();

        // Human edits a different file
        let human_note = dir.path().join("human-note.md");
        std::fs::write(&human_note, "human content\n").unwrap();

        // Both paths passed to commit_human_change
        let result = git
            .commit_human_change(&[PathBuf::from("ai-note.md"), PathBuf::from("human-note.md")])
            .unwrap();

        // Should have committed (human-note.md is new)
        assert!(result.is_some());

        // Check provenance
        let prov = git.provenance("human-note.md").unwrap();
        assert_eq!(prov.human_pct, 100.0);
    }

    #[test]
    fn provenance_aggregate() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (git, _) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        // AI creates file with multiple lines
        std::fs::write(
            dir.path().join("mixed.md"),
            "ai line 1\nai line 2\nai line 3\nai line 4\n",
        )
        .unwrap();
        git.commit_ai_write(&[Path::new("mixed.md")], "arcana: create mixed.md")
            .unwrap();

        // Simulate watcher consuming the pending AI write (clears the pending set)
        git.commit_human_change(&[PathBuf::from("mixed.md")])
            .unwrap();

        // Now human appends new lines (the pending set is cleared, so this is a true human edit)
        std::fs::write(
            dir.path().join("mixed.md"),
            "ai line 1\nai line 2\nai line 3\nai line 4\nhuman line 1\nhuman line 2\nhuman line 3\nhuman line 4\n",
        )
        .unwrap();
        git.commit_human_change(&[PathBuf::from("mixed.md")])
            .unwrap();

        let prov = git.provenance("mixed.md").unwrap();
        assert_eq!(prov.total_lines, 8);
        assert_eq!(prov.ai_lines, 4);
        assert_eq!(prov.human_lines, 4);
        assert!((prov.ai_pct - 50.0).abs() < 0.01);
    }

    #[test]
    fn log_filters_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (git, _) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        std::fs::write(dir.path().join("a.md"), "aaa\n").unwrap();
        git.commit_ai_write(&[Path::new("a.md")], "create a")
            .unwrap();

        std::fs::write(dir.path().join("b.md"), "bbb\n").unwrap();
        git.commit_ai_write(&[Path::new("b.md")], "create b")
            .unwrap();

        let log_a = git.log(Some("a.md"), 10).unwrap();
        assert_eq!(log_a.len(), 1);
        assert!(log_a[0].message.contains("create a"));

        let log_all = git.log(None, 10).unwrap();
        assert!(log_all.len() >= 3); // init + 2 commits
    }

    #[test]
    fn diff_shows_uncommitted() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (git, _) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        std::fs::write(dir.path().join("note.md"), "original\n").unwrap();
        git.commit_ai_write(&[Path::new("note.md")], "create note")
            .unwrap();

        // Modify without committing
        std::fs::write(dir.path().join("note.md"), "modified\n").unwrap();

        let diff = git.diff(Some("note.md")).unwrap();
        assert!(diff.contains("-original"));
        assert!(diff.contains("+modified"));
    }

    #[test]
    fn restore_to_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (git, _) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        // Create initial version
        std::fs::write(dir.path().join("note.md"), "version 1\n").unwrap();
        let first_oid = git
            .commit_ai_write(&[Path::new("note.md")], "create note")
            .unwrap();

        // Update to version 2
        std::fs::write(dir.path().join("note.md"), "version 2\n").unwrap();
        git.commit_ai_write(&[Path::new("note.md")], "update note")
            .unwrap();

        // Restore to version 1
        git.restore("note.md", &first_oid.to_string()).unwrap();

        let content = std::fs::read_to_string(dir.path().join("note.md")).unwrap();
        assert_eq!(content, "version 1\n");
    }

    #[test]
    fn provenance_author_from_email() {
        assert_eq!(
            ProvenanceAuthor::from_email("ai@arcana.local"),
            ProvenanceAuthor::Ai
        );
        assert_eq!(
            ProvenanceAuthor::from_email("user@example.com"),
            ProvenanceAuthor::Human
        );
    }

    #[test]
    fn stale_lock_is_cleared_before_commit() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (git, _) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        // Create a stale lock file with an old mtime
        let lock_path = dir.path().join(".git/index.lock");
        std::fs::write(&lock_path, "").unwrap();
        // Set mtime to 60 seconds ago
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let old_filetime = std::fs::FileTimes::new().set_modified(old_time);
        std::fs::File::options()
            .write(true)
            .open(&lock_path)
            .unwrap()
            .set_times(old_filetime)
            .unwrap();

        // Commit should succeed despite the lock file
        std::fs::write(dir.path().join("test.md"), "content\n").unwrap();
        let result = git.commit_ai_write(&[Path::new("test.md")], "test commit");
        assert!(
            result.is_ok(),
            "commit should succeed after clearing stale lock"
        );
        assert!(!lock_path.exists(), "stale lock should have been removed");
    }

    #[test]
    fn fresh_lock_is_not_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (_git, _) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        // Create a fresh lock file (just now)
        let lock_path = dir.path().join(".git/index.lock");
        std::fs::write(&lock_path, "").unwrap();

        // clear_stale_index_lock with 30s threshold should NOT remove it
        _git.clear_stale_index_lock(std::time::Duration::from_secs(30))
            .unwrap();
        assert!(lock_path.exists(), "fresh lock should not be removed");

        // Clean up so the test doesn't leave a lock behind
        std::fs::remove_file(&lock_path).unwrap();
    }

    #[test]
    fn adopt_untracked_finds_new_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        let (git, _) = VaultGit::open_or_init(dir.path(), &config).unwrap();

        // Create untracked .md files after init
        std::fs::write(dir.path().join("new-note.md"), "hello\n").unwrap();
        std::fs::write(dir.path().join("another.md"), "world\n").unwrap();
        // Non-md file should be ignored
        std::fs::write(dir.path().join("ignore.txt"), "skip\n").unwrap();

        let result = git.adopt_untracked().unwrap();
        assert_eq!(result, Some(2));

        // Files should now be committed
        let blame = git.blame("new-note.md").unwrap();
        assert_eq!(blame[0].author, ProvenanceAuthor::Human);

        // Second call should find nothing
        let result = git.adopt_untracked().unwrap();
        assert_eq!(result, None);
    }
}
