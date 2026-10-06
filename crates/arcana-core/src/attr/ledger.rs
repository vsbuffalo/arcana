//! The ledger: the only code that writes note content in a ledger vault.
//!
//! Layout under the vault root:
//!
//! ```text
//! .arcana/attr/<note path>.attr         sidecar (committed with the note)
//! .arcana/cache/shadow/<note path>      content the sidecar last described (not committed)
//! .arcana/pending/<id>.json             changes waiting for review (not committed)
//! .arcana/pending/decided/<id>.json     review decisions, read back by agents
//! .arcana/ledger.lock                   cross-process lock
//! ```
//!
//! Every write takes the lock, writes content, sidecar and shadow, then makes
//! one git commit of exactly the note and its sidecar.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::attribution::{Attribution, Insertion};
use super::author::{Author, Grant, HumanVia, Origin, LIGHT_EDIT_POLICY};
use super::blocks::blocks;
use super::plan::{dispose, light_edit, resolve, touch, Disposition, RawEdit, Touch};
use super::sidecar::{bytes_hash, Sidecar};
use super::token::tokenize;
use super::types::{kind_of, load_types, NoteKind, NoteType};
use crate::config::LedgerConfig;
use crate::errors::{ArcanaError, Result};
use crate::git::VaultGit;
use crate::vault_path::VaultPath;
use crate::writer::atomic_write;

pub struct Ledger {
    root: PathBuf,
    cfg: LedgerConfig,
    types: BTreeMap<String, NoteType>,
}

/// A note as the ledger sees it now.
#[derive(Debug, Clone)]
pub struct NoteState {
    pub rel: String,
    pub content: String,
    pub attribution: Attribution,
    pub kind: NoteKind,
    pub note_type: Option<String>,
    /// The file changed since the sidecar was written; `attribution` already
    /// accounts for the change but has not been persisted.
    pub outside_edit: bool,
}

impl NoteState {
    /// Short content hash an agent passes back as `base`.
    pub fn base(&self) -> String {
        bytes_hash(&self.content)[..12].to_string()
    }
}

/// An agent's request to change one note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditRequest {
    pub note: String,
    /// `base` from `read`; if given and the note has changed, nothing is written.
    #[serde(default)]
    pub base: Option<String>,
    /// What the human asked for, in their words as the agent understood them.
    #[serde(default)]
    pub request: Option<String>,
    /// One line on why, shown in review.
    #[serde(default)]
    pub rationale: Option<String>,
    pub edits: Vec<RawEdit>,
}

/// What became of each edit in a request.
#[derive(Debug, Clone, Serialize)]
pub struct EditOutcome {
    pub note: String,
    pub results: Vec<EditResult>,
    /// Word diff of what was written now, `[-old-]{+new+}`.
    pub applied_diff: Option<String>,
    pub commit_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditResult {
    pub touches: Option<Touch>,
    #[serde(flatten)]
    pub disposition: Disposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_id: Option<String>,
}

/// A change waiting for review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub id: String,
    pub group: String,
    pub note: String,
    pub created: DateTime<Utc>,
    pub agent: String,
    pub session: String,
    pub request: Option<String>,
    pub rationale: Option<String>,
    /// `gate`: changes agent text; `suggest`: touches protected text.
    pub disposition: String,
    pub edit: RawEdit,
    /// The text being replaced (empty for insertions), and its replacement.
    pub before: String,
    pub after: String,
    /// The note text around the change when it was proposed.
    pub context: String,
}

/// A review decision, kept so agents can read why something was rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decided {
    pub pending: Pending,
    pub accepted: bool,
    pub reason: Option<String>,
    pub at: DateTime<Utc>,
}

/// A run of agent text applied without review.
#[derive(Debug, Clone, Serialize)]
pub struct UnreviewedSpan {
    pub note: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub agent: String,
    pub request: Option<String>,
}

/// What the watcher did with a changed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    Unchanged,
    /// Attribution updated for an outside edit; commit `paths` later.
    Updated {
        paths: Vec<PathBuf>,
    },
    Renamed {
        from: String,
        paths: Vec<PathBuf>,
    },
    Deleted {
        paths: Vec<PathBuf>,
    },
}

struct LockGuard(File);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Ledger {
    pub fn open(root: &Path, cfg: &LedgerConfig) -> Result<Self> {
        Ok(Ledger {
            root: root.to_path_buf(),
            cfg: cfg.clone(),
            types: load_types(root)?,
        })
    }

    pub fn types(&self) -> &BTreeMap<String, NoteType> {
        &self.types
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn arcana(&self) -> PathBuf {
        self.root.join(".arcana")
    }

    pub fn sidecar_rel(rel: &str) -> String {
        format!(".arcana/attr/{rel}.attr")
    }

    fn shadow_path(&self, rel: &str) -> PathBuf {
        self.arcana().join("cache").join("shadow").join(rel)
    }

    fn pending_dir(&self) -> PathBuf {
        self.arcana().join("pending")
    }

    fn lock(&self) -> Result<LockGuard> {
        std::fs::create_dir_all(self.arcana())?;
        let f = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.arcana().join("ledger.lock"))?;
        f.lock()?;
        Ok(LockGuard(f))
    }

    fn outside_insertion(&self) -> Insertion {
        Insertion {
            author: Grant::observed(self.cfg.outside_edits_are_human).author(None),
            origin: Origin::Composed,
            unreviewed: false,
            policy: None,
        }
    }

    fn read_sidecar(&self, rel: &str) -> Result<Option<Sidecar>> {
        let p = self.root.join(Self::sidecar_rel(rel));
        match std::fs::read_to_string(&p) {
            Ok(t) => Sidecar::parse(&t).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The note's current content and attribution. An edit made outside
    /// arcana since the last write is attributed here, in memory.
    pub fn state(&self, rel: &str) -> Result<NoteState> {
        let path = VaultPath::resolve(&self.root, rel)?;
        let content = std::fs::read_to_string(path.as_path())
            .map_err(|_| ArcanaError::NoteNotFound(rel.to_string()))?;
        let (attribution, outside_edit) = match self.read_sidecar(rel)? {
            Some(sc) if sc.matches(&content) => (sc.attribution, false),
            Some(sc) => {
                let shadow = std::fs::read_to_string(self.shadow_path(rel)).ok();
                let attr = match shadow {
                    Some(old) if sc.matches(&old) => {
                        sc.attribution
                            .carry_forward(&old, &content, &self.outside_insertion())
                    }
                    // The sidecar's content is unknown: nothing can be carried.
                    _ => Attribution::uniform(
                        sc.attribution.note_id.clone(),
                        &content,
                        &Insertion {
                            author: Author::Unattributed,
                            ..self.outside_insertion()
                        },
                    ),
                };
                (attr, true)
            }
            // No sidecar: a renamed or moved note keeps the attribution of
            // the orphaned sidecar describing exactly this content; only a
            // genuinely new file is an outside edit.
            None => match self.orphan_matching(rel, &content)? {
                Some(sc) => (sc.attribution, true),
                None => (
                    Attribution::uniform(
                        uuid::Uuid::new_v4().to_string(),
                        &content,
                        &self.outside_insertion(),
                    ),
                    true,
                ),
            },
        };
        let note_type = frontmatter_field(&content, "type");
        let kind = kind_of(
            rel,
            note_type.as_deref(),
            &self.types,
            &self.cfg.kinds,
            self.cfg.default_kind,
        );
        Ok(NoteState {
            rel: rel.to_string(),
            content,
            attribution,
            kind,
            note_type,
            outside_edit,
        })
    }

    /// Write content (if changed), sidecar and shadow for a note.
    fn persist(&self, rel: &str, content: &str, attr: &Attribution) -> Result<Vec<PathBuf>> {
        let note = VaultPath::resolve(&self.root, rel)?;
        let on_disk = std::fs::read_to_string(note.as_path()).ok();
        if on_disk.as_deref() != Some(content) {
            atomic_write(&note, content.as_bytes())?;
        }
        let sidecar_rel = Self::sidecar_rel(rel);
        let sidecar = VaultPath::resolve(&self.root, &sidecar_rel)?;
        let mut attr = attr.clone();
        attr.compact();
        atomic_write(
            &sidecar,
            Sidecar::for_content(attr, content).render().as_bytes(),
        )?;
        let shadow = self.shadow_path(rel);
        std::fs::create_dir_all(shadow.parent().unwrap_or(&self.root))?;
        std::fs::write(&shadow, content)?;
        Ok(vec![PathBuf::from(rel), PathBuf::from(sidecar_rel)])
    }

    fn commit(
        &self,
        git: Option<&VaultGit>,
        paths: &[PathBuf],
        message: &str,
        human: bool,
    ) -> Option<String> {
        let git = git?;
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        match git.commit_paths(&refs, message, human) {
            Ok(_) => {
                let _ = std::fs::remove_file(self.arcana().join("cache").join("health"));
                None
            }
            Err(e) => {
                let msg = format!("git commit failed: {e}");
                let _ = std::fs::write(self.arcana().join("cache").join("health"), &msg);
                Some(msg)
            }
        }
    }

    /// The last commit or attribution failure, if unresolved.
    pub fn health(&self) -> Option<String> {
        std::fs::read_to_string(self.arcana().join("cache").join("health")).ok()
    }

    // ------------------------------------------------------------------
    // Outside edits (watcher)
    // ------------------------------------------------------------------

    /// Record an edit made outside arcana. Persists the sidecar but does not
    /// commit: the caller batches commits of observed edits.
    pub fn observe(&self, rel: &str) -> Result<Observed> {
        let _lock = self.lock()?;
        let path = self.root.join(rel);
        if !path.exists() {
            let sc = self.root.join(Self::sidecar_rel(rel));
            if sc.exists() {
                // Keep the sidecar as an orphan so a rename can claim it.
                return Ok(Observed::Deleted {
                    paths: vec![PathBuf::from(rel)],
                });
            }
            return Ok(Observed::Unchanged);
        }
        let had_sidecar = self.root.join(Self::sidecar_rel(rel)).exists();
        if !had_sidecar {
            if let Some(from) = self.find_orphan_for(rel)? {
                self.move_attribution(&from, rel)?;
                let st = self.state(rel)?;
                let mut paths = self.persist(rel, &st.content, &st.attribution)?;
                paths.push(PathBuf::from(&from));
                paths.push(PathBuf::from(Self::sidecar_rel(&from)));
                return Ok(Observed::Renamed { from, paths });
            }
        }
        let st = self.state(rel)?;
        if !st.outside_edit {
            return Ok(Observed::Unchanged);
        }
        let paths = self.persist(rel, &st.content, &st.attribution)?;
        Ok(Observed::Updated { paths })
    }

    fn orphan_matching(&self, rel: &str, content: &str) -> Result<Option<Sidecar>> {
        Ok(match self.find_orphan_with(rel, content)? {
            Some(from) => self.read_sidecar(&from)?,
            None => None,
        })
    }

    /// An orphaned sidecar (its note is gone) describing exactly this content.
    fn find_orphan_for(&self, rel: &str) -> Result<Option<String>> {
        let content = std::fs::read_to_string(self.root.join(rel))?;
        self.find_orphan_with(rel, &content)
    }

    fn find_orphan_with(&self, rel: &str, content: &str) -> Result<Option<String>> {
        let hash = bytes_hash(content);
        let attr_root = self.arcana().join("attr");
        for entry in walkdir::WalkDir::new(&attr_root).into_iter().flatten() {
            let p = entry.path();
            let Some(srel) = p
                .strip_prefix(&attr_root)
                .ok()
                .and_then(|r| r.to_str())
                .and_then(|r| r.strip_suffix(".attr"))
            else {
                continue;
            };
            if srel == rel || self.root.join(srel).exists() {
                continue;
            }
            if let Ok(sc) = Sidecar::parse(&std::fs::read_to_string(p)?) {
                if sc.bytes_hash == hash {
                    return Ok(Some(srel.to_string()));
                }
            }
        }
        Ok(None)
    }

    fn move_attribution(&self, from: &str, to: &str) -> Result<()> {
        let mv = |a: PathBuf, b: PathBuf| -> Result<()> {
            if a.exists() {
                std::fs::create_dir_all(b.parent().unwrap_or(&self.root))?;
                std::fs::rename(a, b)?;
            }
            Ok(())
        };
        mv(
            self.root.join(Self::sidecar_rel(from)),
            self.root.join(Self::sidecar_rel(to)),
        )?;
        mv(self.shadow_path(from), self.shadow_path(to))
    }

    /// Remove sidecars whose notes are gone; returns them for committing.
    pub fn sweep_orphans(&self) -> Result<Vec<PathBuf>> {
        let _lock = self.lock()?;
        let attr_root = self.arcana().join("attr");
        let mut removed = Vec::new();
        for entry in walkdir::WalkDir::new(&attr_root).into_iter().flatten() {
            let p = entry.path().to_path_buf();
            let Some(srel) = p
                .strip_prefix(&attr_root)
                .ok()
                .and_then(|r| r.to_str())
                .and_then(|r| r.strip_suffix(".attr"))
                .map(str::to_string)
            else {
                continue;
            };
            if !self.root.join(&srel).exists() {
                std::fs::remove_file(&p)?;
                let _ = std::fs::remove_file(self.shadow_path(&srel));
                removed.push(PathBuf::from(Self::sidecar_rel(&srel)));
            }
        }
        Ok(removed)
    }

    // ------------------------------------------------------------------
    // Agent writes
    // ------------------------------------------------------------------

    /// Plan and carry out an agent's edit request.
    pub fn agent_edit(
        &self,
        req: EditRequest,
        grant: Grant,
        git: Option<&VaultGit>,
    ) -> Result<EditOutcome> {
        if !grant.is_agent() {
            return Err(ArcanaError::Ledger(
                "agent_edit requires an agent grant".into(),
            ));
        }
        let _lock = self.lock()?;
        let mut st = self.state(&req.note)?;
        if let Some(base) = &req.base {
            if !bytes_hash(&st.content).starts_with(base.as_str()) {
                return Err(ArcanaError::Ledger(format!(
                    "{} changed since you read it (base {base}, now {}); read it again",
                    req.note,
                    st.base()
                )));
            }
        }
        // An outside edit since the last write is the human's (or
        // unattributed): record and commit it before the agent's changes, so
        // git never shows it under the agent's commit.
        if st.outside_edit {
            let paths = self.persist(&req.note, &st.content, &st.attribution)?;
            if let Some(e) = self.commit_observed(&paths, git) {
                tracing::warn!("ledger: {e}");
            }
            st.outside_edit = false;
        }
        let author = grant.author(req.request.as_deref());
        let Author::Agent { agent, session, .. } = &author else {
            unreachable!("agent grant yields an agent author")
        };
        let group = short_id();
        let original = st.content.clone();
        let mut results = Vec::new();

        for edit in &req.edits {
            let r = match resolve(&st.content, edit) {
                Ok(r) => r,
                Err(e) => {
                    results.push(EditResult {
                        touches: None,
                        disposition: Disposition::Refuse {
                            reason: e.to_string(),
                        },
                        pending_id: None,
                    });
                    continue;
                }
            };
            let t = touch(&st.content, &st.attribution, &r);
            let d = dispose(st.kind, t);
            let mut pending_id = None;
            match &d {
                Disposition::Apply => {
                    let new = r.apply(&st.content);
                    st.attribution = st.attribution.carry_forward(
                        &st.content,
                        &new,
                        &Insertion {
                            author: author.clone(),
                            origin: Origin::Composed,
                            unreviewed: true,
                            policy: None,
                        },
                    );
                    st.content = new;
                }
                Disposition::Gate | Disposition::Suggest => {
                    let p = Pending {
                        id: short_id(),
                        group: group.clone(),
                        note: req.note.clone(),
                        created: Utc::now(),
                        agent: agent.clone(),
                        session: session.clone(),
                        request: req.request.clone(),
                        rationale: req.rationale.clone(),
                        disposition: if d == Disposition::Gate {
                            "gate"
                        } else {
                            "suggest"
                        }
                        .into(),
                        edit: edit.clone(),
                        before: st.content[r.start..r.end].to_string(),
                        after: r.text.clone(),
                        context: context_around(&st.content, r.start, r.end),
                    };
                    self.save_pending(&p)?;
                    pending_id = Some(p.id);
                }
                Disposition::Refuse { .. } => {}
            }
            results.push(EditResult {
                touches: Some(t),
                disposition: d,
                pending_id,
            });
        }

        let mut outcome = EditOutcome {
            note: req.note.clone(),
            results,
            applied_diff: None,
            commit_error: None,
        };
        if st.content != original {
            let paths = self.persist(&req.note, &st.content, &st.attribution)?;
            outcome.applied_diff = Some(word_diff(&original, &st.content));
            let msg = commit_message(
                &format!("arcana: {} {}", "edit", req.note),
                &author,
                req.request.as_deref(),
            );
            outcome.commit_error = self.commit(git, &paths, &msg, false);
        }
        Ok(outcome)
    }

    /// Create a note from a type (or a plain chapter). Agent text lands
    /// unreviewed; agents cannot create `writing` notes.
    #[allow(clippy::too_many_arguments)]
    pub fn agent_create(
        &self,
        type_name: Option<&str>,
        path: Option<&str>,
        title: &str,
        fields: &BTreeMap<String, String>,
        body: Option<&str>,
        request: Option<&str>,
        grant: Grant,
        git: Option<&VaultGit>,
    ) -> Result<(String, Option<String>)> {
        if !grant.is_agent() {
            return Err(ArcanaError::Ledger(
                "agent_create requires an agent grant".into(),
            ));
        }
        let nt = match type_name {
            Some(t) => Some(self.types.get(t).ok_or_else(|| {
                ArcanaError::Ledger(format!(
                    "unknown note type {t:?}; known: {}",
                    self.types.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })?),
            None => None,
        };
        let rel = match (path, nt) {
            (Some(p), _) => p.to_string(),
            (None, Some(t)) => t.render_path(title, fields)?,
            (None, None) => return Err(ArcanaError::Ledger("give a note type or a path".into())),
        };
        let kind = nt.map(|t| t.kind).unwrap_or_else(|| {
            kind_of(
                &rel,
                None,
                &self.types,
                &self.cfg.kinds,
                self.cfg.default_kind,
            )
        });
        if kind == NoteKind::Writing {
            return Err(ArcanaError::Ledger(
                "agents cannot create writing notes; those are the human's".into(),
            ));
        }
        let _lock = self.lock()?;
        let vp = VaultPath::resolve(&self.root, &rel)?;
        if vp.as_path().exists() {
            return Err(ArcanaError::NoteAlreadyExists(rel));
        }
        let mut fm = format!("---\ntitle: {}\n", yaml_str(title));
        if let Some(t) = nt {
            fm.push_str(&format!("type: {}\n", t.name));
            if !t.tags.is_empty() {
                fm.push_str(&format!("tags: [{}]\n", t.tags.join(", ")));
            }
        }
        fm.push_str("---\n\n");
        let body = body
            .map(str::to_string)
            .or_else(|| nt.map(|t| t.template.clone()))
            .unwrap_or_default();
        let content = format!("{fm}{}\n", body.trim_end());
        let author = grant.author(request);
        let attr = Attribution::uniform(
            uuid::Uuid::new_v4().to_string(),
            &content,
            &Insertion {
                author: author.clone(),
                origin: Origin::Composed,
                unreviewed: true,
                policy: None,
            },
        );
        let paths = self.persist(&rel, &content, &attr)?;
        let msg = commit_message(&format!("arcana: create {rel}"), &author, request);
        let err = self.commit(git, &paths, &msg, false);
        Ok((rel, err))
    }

    // ------------------------------------------------------------------
    // Review
    // ------------------------------------------------------------------

    fn save_pending(&self, p: &Pending) -> Result<()> {
        let dir = self.pending_dir();
        std::fs::create_dir_all(&dir)?;
        let json =
            serde_json::to_string_pretty(p).map_err(|e| ArcanaError::Ledger(e.to_string()))?;
        std::fs::write(dir.join(format!("{}.json", p.id)), json)?;
        Ok(())
    }

    pub fn pending(&self) -> Result<Vec<Pending>> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.pending_dir()) else {
            return Ok(out);
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("json") {
                if let Ok(pd) = serde_json::from_str::<Pending>(&std::fs::read_to_string(&p)?) {
                    out.push(pd);
                }
            }
        }
        out.sort_by_key(|p| p.created);
        Ok(out)
    }

    /// Recent review decisions, newest first, so agents can learn from them.
    pub fn decided(&self, limit: usize) -> Result<Vec<Decided>> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.pending_dir().join("decided")) else {
            return Ok(out);
        };
        for e in entries.flatten() {
            if let Ok(d) = serde_json::from_str::<Decided>(&std::fs::read_to_string(e.path())?) {
                out.push(d);
            }
        }
        out.sort_by_key(|d| std::cmp::Reverse(d.at));
        out.truncate(limit);
        Ok(out)
    }

    fn record_decision(&self, p: &Pending, accepted: bool, reason: Option<String>) -> Result<()> {
        let dir = self.pending_dir().join("decided");
        std::fs::create_dir_all(&dir)?;
        let d = Decided {
            pending: p.clone(),
            accepted,
            reason,
            at: Utc::now(),
        };
        let json =
            serde_json::to_string_pretty(&d).map_err(|e| ArcanaError::Ledger(e.to_string()))?;
        std::fs::write(dir.join(format!("{}.json", p.id)), json)?;
        let _ = std::fs::remove_file(self.pending_dir().join(format!("{}.json", p.id)));
        Ok(())
    }

    /// Accept a pending change. A light edit to the human's words keeps them
    /// the human's (recorded with the policy); anything else is the agent's.
    pub fn accept(&self, id: &str, git: Option<&VaultGit>) -> Result<Option<String>> {
        let _lock = self.lock()?;
        let p = self.load_pending(id)?;
        let st = self.state(&p.note)?;
        let r = resolve(&st.content, &p.edit).map_err(|e| {
            ArcanaError::Ledger(format!(
                "the note changed and the change no longer applies: {e}"
            ))
        })?;
        let agent = Author::Agent {
            agent: p.agent.clone(),
            session: p.session.clone(),
            request: p.request.clone(),
        };
        let light = (p.disposition == "suggest")
            .then(|| light_edit(&p.before, &p.after))
            .flatten();
        let ins = match light {
            Some(Origin::CitationInsert) => Insertion {
                author: agent,
                origin: Origin::CitationInsert,
                unreviewed: false,
                policy: Some(LIGHT_EDIT_POLICY.into()),
            },
            Some(o) => Insertion {
                author: Author::Human {
                    via: HumanVia::Review,
                },
                origin: o,
                unreviewed: false,
                policy: Some(LIGHT_EDIT_POLICY.into()),
            },
            None => Insertion {
                author: agent.clone(),
                origin: Origin::Composed,
                unreviewed: false,
                policy: None,
            },
        };
        let new = r.apply(&st.content);
        let attr = st.attribution.carry_forward(&st.content, &new, &ins);
        let paths = self.persist(&p.note, &new, &attr)?;
        self.record_decision(&p, true, None)?;
        let msg = commit_message(
            &format!("arcana: accept change to {}", p.note),
            &ins.author,
            p.request.as_deref(),
        );
        Ok(self.commit(git, &paths, &msg, true))
    }

    pub fn reject(&self, id: &str, reason: Option<String>) -> Result<()> {
        let _lock = self.lock()?;
        let p = self.load_pending(id)?;
        self.record_decision(&p, false, reason)
    }

    fn load_pending(&self, id: &str) -> Result<Pending> {
        let path = self.pending_dir().join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path)
            .map_err(|_| ArcanaError::Ledger(format!("no pending change {id}")))?;
        serde_json::from_str(&text).map_err(|e| ArcanaError::Ledger(e.to_string()))
    }

    /// Agent text applied without review, per note, as contiguous blocks.
    pub fn unreviewed(&self) -> Result<Vec<UnreviewedSpan>> {
        let attr_root = self.arcana().join("attr");
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&attr_root).into_iter().flatten() {
            let Some(rel) = entry
                .path()
                .strip_prefix(&attr_root)
                .ok()
                .and_then(|r| r.to_str())
                .and_then(|r| r.strip_suffix(".attr"))
                .map(str::to_string)
            else {
                continue;
            };
            let Ok(st) = self.state(&rel) else { continue };
            out.extend(unreviewed_spans(&st));
        }
        Ok(out)
    }

    /// Mark an unreviewed span reviewed (keep) or remove its text (reject).
    pub fn review_span(
        &self,
        rel: &str,
        start: usize,
        keep: bool,
        git: Option<&VaultGit>,
    ) -> Result<Option<String>> {
        let _lock = self.lock()?;
        let st = self.state(rel)?;
        let span = unreviewed_spans(&st)
            .into_iter()
            .find(|s| s.start == start)
            .ok_or_else(|| ArcanaError::Ledger("that unreviewed text has changed".into()))?;
        let toks = tokenize(&st.content);
        let (content, mut attr) = if keep {
            let mut attr = st.attribution.clone();
            for (t, a) in toks.iter().zip(attr.tokens.iter_mut()) {
                if t.start >= span.start && t.end <= span.end {
                    a.unreviewed = false;
                }
            }
            (st.content.clone(), attr)
        } else {
            let (s, e) = widen_to_blocks(&st.content, span.start, span.end);
            let new = tidy_blank_lines(&format!("{}{}", &st.content[..s], &st.content[e..]));
            let attr = st.attribution.carry_forward(
                &st.content,
                &new,
                &Insertion {
                    author: Author::Human {
                        via: HumanVia::Review,
                    },
                    origin: Origin::Mechanical,
                    unreviewed: false,
                    policy: None,
                },
            );
            (new, attr)
        };
        attr.compact();
        let paths = self.persist(rel, &content, &attr)?;
        let verb = if keep {
            "review"
        } else {
            "remove unreviewed text in"
        };
        Ok(self.commit(git, &paths, &format!("arcana: {verb} {rel}"), true))
    }

    /// The human edited a note in `$EDITOR` from review.
    pub fn editor_edit(
        &self,
        rel: &str,
        new: &str,
        git: Option<&VaultGit>,
    ) -> Result<Option<String>> {
        let _lock = self.lock()?;
        let st = self.state(rel)?;
        let grant = Grant::editor();
        let ins = Insertion {
            author: grant.author(None),
            origin: Origin::Composed,
            unreviewed: false,
            policy: None,
        };
        let attr = st.attribution.carry_forward(&st.content, new, &ins);
        let paths = self.persist(rel, new, &attr)?;
        Ok(self.commit(git, &paths, &format!("vault: edit {rel}"), true))
    }

    /// Bring a note under the ledger (or re-attribute an outside edit) and
    /// commit the observation as the human's.
    pub fn commit_observed(&self, paths: &[PathBuf], git: Option<&VaultGit>) -> Option<String> {
        if paths.is_empty() {
            return None;
        }
        let names: Vec<String> = paths
            .iter()
            .filter(|p| !p.starts_with(".arcana"))
            .map(|p| p.display().to_string())
            .collect();
        let msg = format!("vault: update {}", names.join(", "));
        self.commit(git, paths, &msg, true)
    }

    /// Create a note from outside the agent path (the `arcana create` CLI).
    /// Its words are attributed exactly like an edit made in an editor: the
    /// human's while the write boundary holds, unattributed otherwise.
    pub fn create_outside(
        &self,
        rel: &str,
        content: &str,
        git: Option<&VaultGit>,
    ) -> Result<Option<String>> {
        let _lock = self.lock()?;
        let vp = VaultPath::resolve(&self.root, rel)?;
        if vp.as_path().exists() {
            return Err(ArcanaError::NoteAlreadyExists(rel.to_string()));
        }
        let attr = Attribution::uniform(
            uuid::Uuid::new_v4().to_string(),
            content,
            &self.outside_insertion(),
        );
        let paths = self.persist(rel, content, &attr)?;
        Ok(self.commit_observed(&paths, git))
    }

    /// Restore a note and its attribution to an earlier commit. Restored words
    /// get back the authors they had then; nothing is re-credited to whoever
    /// restored them. A note that predates the ledger at that commit is
    /// restored as unattributed.
    pub fn restore(&self, rel: &str, commit: &str, git: &VaultGit) -> Result<Option<String>> {
        let _lock = self.lock()?;
        let content = git
            .file_at(rel, commit)?
            .ok_or_else(|| ArcanaError::NoteNotFound(format!("{rel} at {commit}")))?;
        let content = String::from_utf8(content)
            .map_err(|_| ArcanaError::Ledger(format!("{rel} at {commit} is not UTF-8")))?;
        let sidecar = git
            .file_at(&Self::sidecar_rel(rel), commit)?
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| Sidecar::parse(&t).ok())
            .filter(|sc| sc.matches(&content));
        let attr = match sidecar {
            Some(sc) => sc.attribution,
            None => Attribution::uniform(
                uuid::Uuid::new_v4().to_string(),
                &content,
                &Insertion {
                    author: Author::Unattributed,
                    origin: Origin::Composed,
                    unreviewed: false,
                    policy: None,
                },
            ),
        };
        let paths = self.persist(rel, &content, &attr)?;
        Ok(self.commit(
            Some(git),
            &paths,
            &format!(
                "arcana: restore {rel} to {}",
                &commit[..commit.len().min(8)]
            ),
            true,
        ))
    }

    /// Start tracking an existing note with an explicit, non-human author.
    pub fn import(
        &self,
        rel: &str,
        as_: &ImportAs,
        git: Option<&VaultGit>,
    ) -> Result<Option<String>> {
        let _lock = self.lock()?;
        let path = VaultPath::resolve(&self.root, rel)?;
        let content = std::fs::read_to_string(path.as_path())?;
        let author = match as_ {
            ImportAs::Agent(a) => Author::Agent {
                agent: a.clone(),
                session: "import".into(),
                request: None,
            },
            ImportAs::Declared => Author::Human {
                via: HumanVia::Declared,
            },
            ImportAs::Unattributed => Author::Unattributed,
        };
        let attr = Attribution::uniform(
            uuid::Uuid::new_v4().to_string(),
            &content,
            &Insertion {
                author,
                origin: Origin::Composed,
                unreviewed: false,
                policy: None,
            },
        );
        let paths = self.persist(rel, &content, &attr)?;
        let human = matches!(as_, ImportAs::Declared);
        Ok(self.commit(git, &paths, &format!("arcana: import {rel} ({as_})"), human))
    }
}

/// Who imported words are credited to. Never a witnessed human: the closest
/// is `Declared`, which records that the human claimed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportAs {
    Agent(String),
    Declared,
    Unattributed,
}

impl std::str::FromStr for ImportAs {
    type Err = ArcanaError;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim() {
            "declared" => Ok(ImportAs::Declared),
            "unattributed" | "" => Ok(ImportAs::Unattributed),
            a => match a.strip_prefix("agent:") {
                Some(name) if !name.is_empty() => Ok(ImportAs::Agent(name.to_string())),
                _ => Err(ArcanaError::Ledger(format!(
                    "import author must be agent:<name>, declared or unattributed, not {s:?}"
                ))),
            },
        }
    }
}

impl std::fmt::Display for ImportAs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportAs::Agent(a) => write!(f, "agent:{a}"),
            ImportAs::Declared => f.write_str("declared"),
            ImportAs::Unattributed => f.write_str("unattributed"),
        }
    }
}

/// Contiguous unreviewed runs of a note, as byte spans.
/// Spans break at headings, so a new chapter is reviewed section by section.
fn unreviewed_spans(st: &NoteState) -> Vec<UnreviewedSpan> {
    let toks = tokenize(&st.content);
    let headings: std::collections::HashSet<usize> = blocks(&st.content)
        .iter()
        .filter(|b| b.kind == super::blocks::BlockKind::Heading)
        .map(|b| b.start)
        .collect();
    let mut out: Vec<UnreviewedSpan> = Vec::new();
    let mut prev_unreviewed = false;
    for (t, a) in toks.iter().zip(&st.attribution.tokens) {
        if headings.contains(&t.start) {
            prev_unreviewed = false;
        }
        if a.unreviewed {
            let (agent, request) = match st.attribution.author_of(a) {
                Author::Agent { agent, request, .. } => (agent.clone(), request.clone()),
                _ => ("?".into(), None),
            };
            match out.last_mut() {
                Some(s) if prev_unreviewed => s.end = t.end,
                _ => out.push(UnreviewedSpan {
                    note: st.rel.clone(),
                    start: t.start,
                    end: t.end,
                    text: String::new(),
                    agent,
                    request,
                }),
            }
        }
        prev_unreviewed = a.unreviewed;
    }
    for s in &mut out {
        s.text = st.content[s.start..s.end].to_string();
    }
    out
}

/// Expand a byte span to whole lines plus one following blank line, so
/// removing it leaves well-formed paragraphs.
fn widen_to_blocks(content: &str, start: usize, end: usize) -> (usize, usize) {
    let s = content[..start].rfind('\n').map_or(0, |i| i + 1);
    let mut e = content[end..]
        .find('\n')
        .map_or(content.len(), |i| end + i + 1);
    if content[e..].starts_with('\n') {
        e += 1;
    }
    (s, e)
}

/// At most one blank line between blocks, and exactly one trailing newline.
fn tidy_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut newlines = 0;
    for c in s.chars() {
        if c == '\n' {
            newlines += 1;
            if newlines > 2 {
                continue;
            }
        } else {
            newlines = 0;
        }
        out.push(c);
    }
    format!("{}\n", out.trim_end_matches('\n'))
}

fn context_around(content: &str, start: usize, end: usize) -> String {
    let bs = blocks(content);
    let s = bs
        .iter()
        .filter(|b| b.start <= start)
        .map(|b| b.start)
        .max()
        .unwrap_or(0);
    let e = bs
        .iter()
        .filter(|b| b.end >= end)
        .map(|b| b.end)
        .min()
        .unwrap_or(content.len());
    content[s..e.max(s)].to_string()
}

/// Inline word diff: `[-removed-]{+added+}`, with adjacent changes merged.
pub fn word_diff(old: &str, new: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_words(old, new);
    let mut out = String::new();
    let mut run: Option<(ChangeTag, String)> = None;
    let flush = |out: &mut String, run: &mut Option<(ChangeTag, String)>| {
        if let Some((tag, text)) = run.take() {
            match tag {
                ChangeTag::Equal => out.push_str(&text),
                ChangeTag::Delete => out.push_str(&format!("[-{text}-]")),
                ChangeTag::Insert => out.push_str(&format!("{{+{text}+}}")),
            }
        }
    };
    for change in diff.iter_all_changes() {
        match &mut run {
            Some((tag, text)) if *tag == change.tag() => text.push_str(change.value()),
            _ => {
                flush(&mut out, &mut run);
                run = Some((change.tag(), change.value().to_string()));
            }
        }
    }
    flush(&mut out, &mut run);
    out
}

fn commit_message(subject: &str, author: &Author, request: Option<&str>) -> String {
    let who = match author {
        Author::Human { via } => format!("human ({})", via.as_str()),
        Author::Agent { agent, session, .. } => format!("agent {agent}/{session}"),
        Author::Unattributed => "unattributed".into(),
    };
    let mut msg = format!("{subject}\n\nArcana-Author: {who}\n");
    if let Some(r) = request {
        let one_line: String = r.split_whitespace().collect::<Vec<_>>().join(" ");
        msg.push_str(&format!("Arcana-Request: {one_line}\n"));
    }
    msg
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..10].to_string()
}

fn yaml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

/// A top-level scalar from the note's frontmatter.
fn frontmatter_field(content: &str, key: &str) -> Option<String> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    rest[..end].lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::author::AgentIdentity;

    fn ledger(dir: &Path) -> Ledger {
        Ledger::open(dir, &LedgerConfig::default()).unwrap()
    }

    fn agent() -> Grant {
        Grant::agent(AgentIdentity {
            agent: "test-agent".into(),
            session: "s1".into(),
        })
    }

    fn classes(l: &Ledger, rel: &str) -> String {
        let st = l.state(rel).unwrap();
        st.attribution
            .tokens
            .iter()
            .map(|t| match st.attribution.author_of(t) {
                Author::Human { .. } => 'H',
                Author::Agent { .. } => 'A',
                Author::Unattributed => '?',
            })
            .collect()
    }

    #[test]
    fn agent_cannot_change_human_words_without_review() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(dir.path());
        std::fs::write(dir.path().join("n.md"), "Teh human wrote this.\n").unwrap();
        // Watcher picks up the human's note.
        assert!(matches!(
            l.observe("n.md").unwrap(),
            Observed::Updated { .. }
        ));

        let out = l
            .agent_edit(
                EditRequest {
                    note: "n.md".into(),
                    base: None,
                    request: Some("fix typos".into()),
                    rationale: None,
                    edits: vec![
                        RawEdit::Replace {
                            find: "Teh".into(),
                            with: "The".into(),
                        },
                        RawEdit::Append {
                            heading: None,
                            text: "An agent paragraph.".into(),
                        },
                    ],
                },
                agent(),
                None,
            )
            .unwrap();
        assert_eq!(out.results[0].disposition, Disposition::Suggest);
        assert_eq!(out.results[1].disposition, Disposition::Apply);
        let content = std::fs::read_to_string(dir.path().join("n.md")).unwrap();
        assert_eq!(content, "Teh human wrote this.\n\nAn agent paragraph.\n");
        assert_eq!(classes(&l, "n.md"), "HHHHHAAAA");

        // Accepting the typo fix: a light edit, so the word stays the human's.
        let id = out.results[0].pending_id.clone().unwrap();
        l.accept(&id, None).unwrap();
        let st = l.state("n.md").unwrap();
        assert!(st.content.starts_with("The human"));
        assert_eq!(classes(&l, "n.md"), "HHHHHAAAA");
        let first = &st.attribution.tokens[0];
        assert_eq!(first.origin, Origin::Copyedit);
        assert_eq!(first.policy.as_deref(), Some(LIGHT_EDIT_POLICY));
    }

    #[test]
    fn outside_edit_after_agent_write_is_reconciled() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(dir.path());
        let grant = agent();
        let (rel, _) = l
            .agent_create(
                None,
                Some("c.md"),
                "Q factor",
                &BTreeMap::new(),
                Some("Agent body text."),
                None,
                grant,
                None,
            )
            .unwrap();
        // Human appends in Obsidian.
        let mut c = std::fs::read_to_string(dir.path().join(&rel)).unwrap();
        c.push_str("\nMy note.\n");
        std::fs::write(dir.path().join(&rel), &c).unwrap();
        assert!(matches!(l.observe(&rel).unwrap(), Observed::Updated { .. }));
        assert!(classes(&l, &rel).ends_with("AHHH"));
    }

    #[test]
    fn rename_keeps_attribution() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(dir.path());
        l.agent_create(
            None,
            Some("a.md"),
            "T",
            &BTreeMap::new(),
            Some("Body."),
            None,
            agent(),
            None,
        )
        .unwrap();
        std::fs::rename(dir.path().join("a.md"), dir.path().join("b.md")).unwrap();
        assert!(matches!(
            l.observe("a.md").unwrap(),
            Observed::Deleted { .. }
        ));
        assert!(matches!(
            l.observe("b.md").unwrap(),
            Observed::Renamed { .. }
        ));
        assert!(classes(&l, "b.md").chars().all(|c| c == 'A'));
    }

    #[test]
    fn stale_base_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(dir.path());
        std::fs::write(dir.path().join("n.md"), "text\n").unwrap();
        let err = l
            .agent_edit(
                EditRequest {
                    note: "n.md".into(),
                    base: Some("000000000000".into()),
                    request: None,
                    rationale: None,
                    edits: vec![],
                },
                agent(),
                None,
            )
            .unwrap_err();
        assert!(err.to_string().contains("changed since you read it"));
    }

    #[test]
    fn rejecting_unreviewed_text_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(dir.path());
        std::fs::write(dir.path().join("n.md"), "Mine.\n").unwrap();
        l.observe("n.md").unwrap();
        l.agent_edit(
            EditRequest {
                note: "n.md".into(),
                base: None,
                request: None,
                rationale: None,
                edits: vec![RawEdit::Append {
                    heading: None,
                    text: "Agent paragraph.".into(),
                }],
            },
            agent(),
            None,
        )
        .unwrap();
        let spans = l.unreviewed().unwrap();
        assert_eq!(spans.len(), 1);
        l.review_span("n.md", spans[0].start, false, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("n.md")).unwrap(),
            "Mine.\n"
        );
    }

    #[test]
    fn rejection_is_recorded_with_reason_for_agents() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(dir.path());
        std::fs::write(dir.path().join("n.md"), "Mine entirely.\n").unwrap();
        let out = l
            .agent_edit(
                EditRequest {
                    note: "n.md".into(),
                    base: None,
                    request: None,
                    rationale: None,
                    edits: vec![RawEdit::Replace {
                        find: "entirely".into(),
                        with: "wholly and completely".into(),
                    }],
                },
                agent(),
                None,
            )
            .unwrap();
        let id = out.results[0].pending_id.clone().unwrap();
        l.reject(&id, Some("keep my wording".into())).unwrap();
        assert!(l.pending().unwrap().is_empty());
        let d = &l.decided(5).unwrap()[0];
        assert!(!d.accepted);
        assert_eq!(d.reason.as_deref(), Some("keep my wording"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("n.md")).unwrap(),
            "Mine entirely.\n"
        );
    }

    #[test]
    fn unreviewed_text_splits_at_headings() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(dir.path());
        l.agent_create(
            None,
            Some("c.md"),
            "T",
            &BTreeMap::new(),
            Some("## One\n\nFirst section.\n\n## Two\n\nSecond section."),
            None,
            agent(),
            None,
        )
        .unwrap();
        let spans = l.unreviewed().unwrap();
        assert_eq!(spans.len(), 3, "{spans:?}"); // frontmatter, One, Two
        assert!(spans[2].text.starts_with("## Two"));
    }

    #[test]
    fn moved_note_keeps_attribution_without_the_watcher() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(dir.path());
        l.agent_create(
            None,
            Some("textbook/a.md"),
            "T",
            &BTreeMap::new(),
            Some("Agent body."),
            None,
            agent(),
            None,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("notes")).unwrap();
        std::fs::rename(
            dir.path().join("textbook/a.md"),
            dir.path().join("notes/a.md"),
        )
        .unwrap();
        // No observe(): an ordinary read must still find the moved note's authors.
        assert!(classes(&l, "notes/a.md").chars().all(|c| c == 'A'));
    }
}
