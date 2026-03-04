use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use crate::errors::{ArcanaError, Result};
use crate::frontmatter::split_frontmatter;

#[derive(Debug, Clone)]
pub struct Note {
    pub path: PathBuf,
    pub frontmatter: Frontmatter,
    pub body: String,
    pub file_meta: FileMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Frontmatter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiMeta>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiMeta {
    pub model: String,
    pub provider: String,
    pub agent_session: String,
    pub task: String,
    pub prompt: String,
    #[serde(default)]
    pub sources: Vec<String>,
    pub confidence: Confidence,
    #[serde(default)]
    pub reviewed: bool,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
    Speculative,
}

#[derive(Debug, Clone)]
pub struct FileMeta {
    pub size_bytes: u64,
    pub modified_on_disk: std::time::SystemTime,
    pub content_hash: u64,
}

impl Note {
    pub fn parse(path: PathBuf, content: &str, file_meta: FileMeta) -> Result<Self> {
        let (frontmatter, body) = parse_frontmatter(content)?;
        Ok(Note {
            path,
            frontmatter,
            body,
            file_meta,
        })
    }

    pub fn title(&self) -> &str {
        self.frontmatter.title.as_deref().unwrap_or_else(|| {
            self.path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Untitled")
        })
    }
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let fm = &self.frontmatter;
        let has_content = fm.title.is_some()
            || fm.created.is_some()
            || fm.modified.is_some()
            || !fm.tags.is_empty()
            || !fm.aliases.is_empty()
            || fm.ai.is_some()
            || !fm.extra.is_empty();

        if has_content {
            let yaml = serde_yaml::to_string(&self.frontmatter).unwrap_or_default();
            write!(f, "---\n{}---\n{}", yaml, self.body)
        } else {
            write!(f, "{}", self.body)
        }
    }
}

fn parse_frontmatter(content: &str) -> Result<(Frontmatter, String)> {
    let (yaml, body) = split_frontmatter(content);

    if yaml.is_empty() {
        return Ok((Frontmatter::default(), body));
    }

    match serde_yaml::from_str::<Frontmatter>(&yaml) {
        Ok(fm) => Ok((fm, body)),
        Err(e) => Err(ArcanaError::InvalidFrontmatter(e.to_string())),
    }
}

static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]").unwrap());

static INLINE_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s(])#([a-zA-Z][a-zA-Z0-9_/-]*)").unwrap());

pub fn extract_wikilinks(body: &str) -> Vec<String> {
    WIKILINK_RE
        .captures_iter(body)
        .map(|cap| cap[1].to_string())
        .collect()
}

pub fn extract_inline_tags(body: &str) -> Vec<String> {
    INLINE_TAG_RE
        .captures_iter(body)
        .map(|cap| cap[1].to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_meta() -> FileMeta {
        FileMeta {
            size_bytes: 0,
            modified_on_disk: std::time::SystemTime::UNIX_EPOCH,
            content_hash: 0,
        }
    }

    #[test]
    fn parse_note_with_frontmatter() {
        let content = "---\ntitle: Test Note\ntags:\n  - rust\n  - programming\n---\nHello world\n";
        let note = Note::parse("test.md".into(), content, dummy_meta()).unwrap();
        assert_eq!(note.frontmatter.title.as_deref(), Some("Test Note"));
        assert_eq!(note.frontmatter.tags, vec!["rust", "programming"]);
        assert_eq!(note.body, "Hello world\n");
    }

    #[test]
    fn parse_note_without_frontmatter() {
        let content = "Just a plain note\nWith some content\n";
        let note = Note::parse("plain.md".into(), content, dummy_meta()).unwrap();
        assert!(note.frontmatter.title.is_none());
        assert_eq!(note.body, content);
    }

    #[test]
    fn parse_empty_frontmatter() {
        let content = "---\n---\nBody here\n";
        let note = Note::parse("empty-fm.md".into(), content, dummy_meta()).unwrap();
        assert!(note.frontmatter.title.is_none());
        assert_eq!(note.body, "Body here\n");
    }

    #[test]
    fn round_trip_fidelity() {
        let content = "---\ntitle: Round Trip\ntags:\n  - test\n---\nBody content\n";
        let note = Note::parse("rt.md".into(), content, dummy_meta()).unwrap();
        let output = note.to_string();
        let note2 = Note::parse("rt.md".into(), &output, dummy_meta()).unwrap();
        assert_eq!(note.frontmatter.title, note2.frontmatter.title);
        assert_eq!(note.frontmatter.tags, note2.frontmatter.tags);
        assert_eq!(note.body, note2.body);
    }

    #[test]
    fn extra_field_preservation() {
        let content = "---\ntitle: Extra\ncustom_field: hello\nmy_number: 42\n---\nBody\n";
        let note = Note::parse("extra.md".into(), content, dummy_meta()).unwrap();
        assert_eq!(
            note.frontmatter.extra.get("custom_field"),
            Some(&serde_yaml::Value::String("hello".to_string()))
        );
        let output = note.to_string();
        assert!(output.contains("custom_field"));
        assert!(output.contains("hello"));
    }

    #[test]
    fn wikilink_extraction() {
        let body = "See [[other note]] and [[folder/note|display name]] and [[simple]].";
        let links = extract_wikilinks(body);
        assert_eq!(links, vec!["other note", "folder/note", "simple"]);
    }

    #[test]
    fn tag_extraction() {
        let body = "This has #rust and #programming/async tags.\nAlso #test-tag here.";
        let tags = extract_inline_tags(body);
        assert!(tags.contains(&"rust".to_string()));
        assert!(tags.contains(&"programming/async".to_string()));
    }

    #[test]
    fn tag_not_in_code() {
        // Tags at start of line should work
        let body = "#starting-tag and #mid-tag";
        let tags = extract_inline_tags(body);
        assert!(tags.contains(&"starting-tag".to_string()));
        assert!(tags.contains(&"mid-tag".to_string()));
    }
}
