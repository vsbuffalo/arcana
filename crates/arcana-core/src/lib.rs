pub mod config;
pub mod drafts;
pub mod errors;
pub mod frontmatter;
pub mod git;
pub mod index;
pub mod note;
pub mod profile;
pub mod search;
pub mod skill;
pub mod vault;
pub mod watcher;
pub mod writer;

pub use config::{
    global_config_path, load_merged, AgentSettings, ArcanaConfig, DraftsConfig, GitConfig,
    LlmConfig, OperationOverrides,
};
pub use drafts::{DraftInfo, DraftKind, DraftManager, DraftStatus, SessionInfo, SessionMeta};
pub use errors::{ArcanaError, Result};
pub use git::{CommitInfo, InitInfo, LineProvenance, NoteProvenance, ProvenanceAuthor, VaultGit};
pub use index::fts::IndexStats;
pub use note::{AiMeta, Confidence, FileMeta, Frontmatter, Note};
pub use profile::BrainProfile;
pub use search::{SearchFilters, SearchQuery, SearchResult};
pub use skill::{list_skills, resolve_skill, Skill, SkillMeta, SkillSummary};
pub use vault::{Vault, VaultStats};
pub use watcher::{VaultWatcher, WatchEvent, WatchHandle};
pub use writer::NoteWriter;
