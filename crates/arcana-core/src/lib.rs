pub mod config;
pub mod errors;
pub mod index;
pub mod note;
pub mod search;
pub mod vault;
pub mod watcher;
pub mod writer;

pub use config::ArcanaConfig;
pub use errors::{ArcanaError, Result};
pub use index::fts::IndexStats;
pub use note::{FileMeta, Frontmatter, Note};
pub use search::{SearchFilters, SearchQuery, SearchResult};
pub use vault::{Vault, VaultStats};
pub use watcher::{VaultWatcher, WatchEvent, WatchHandle};
pub use writer::NoteWriter;
