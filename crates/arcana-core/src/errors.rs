use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArcanaError {
    #[error("note not found: {0}")]
    NoteNotFound(String),

    #[error("note already exists: {0}")]
    NoteAlreadyExists(String),

    #[error("invalid frontmatter: {0}")]
    InvalidFrontmatter(String),

    #[error("path escapes vault root: {0}")]
    PathEscape(String),

    #[error("index error: {0}")]
    Index(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("walk error: {0}")]
    Walk(#[from] walkdir::Error),

    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("search error: {0}")]
    Search(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("git error: {0}")]
    Git(String),
}

pub type Result<T> = std::result::Result<T, ArcanaError>;
