//! Note kinds (fixed, decide write permissions) and note types (user-defined
//! templates such as `lab-note` or `chapter`, each mapped to a kind).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::errors::{ArcanaError, Result};

/// What agents may do to a note. Set by its type or path, never by an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    /// Textbook material: agents add and refine; new agent text lands
    /// unreviewed, changes to existing text are gated.
    #[default]
    Chapter,
    /// The human's own prose: every agent change is a suggestion.
    Writing,
    /// Dated records: agents append only.
    Log,
    /// A summary of, and link to, a document whose source of truth is elsewhere.
    Pointer,
}

impl NoteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NoteKind::Chapter => "chapter",
            NoteKind::Writing => "writing",
            NoteKind::Log => "log",
            NoteKind::Pointer => "pointer",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "chapter" => NoteKind::Chapter,
            "writing" => NoteKind::Writing,
            "log" => NoteKind::Log,
            "pointer" => NoteKind::Pointer,
            _ => return None,
        })
    }
}

/// A user-defined note type, from `.arcana/types/<name>.toml`.
///
/// ```toml
/// description = "A dated bench session: setup, measurements, observations"
/// kind = "log"
/// path = "projects/{project}/lab/{date}-{slug}.md"
/// tags = ["lab"]
/// style = "lab-note"          # .arcana/skills/lab-note.md
/// template = """
/// ## Setup
/// ## Measurements
/// ## Observations
/// ## Next
/// """
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteType {
    #[serde(skip_deserializing)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub kind: NoteKind,
    /// Path template; `{date}`, `{slug}` and any field passed at creation.
    pub path: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Name of a skill in `.arcana/skills/` describing the writing style.
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub template: String,
}

impl NoteType {
    /// Expand the path template.
    pub fn render_path(&self, title: &str, fields: &BTreeMap<String, String>) -> Result<String> {
        let mut path = self.path.clone();
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        // A title that already starts with a date ("2026-10-06 — Steak tacos")
        // must not produce "2026-10-06-2026-10-06-steak-tacos".
        let mut slug = slugify(title);
        if path.contains("{date}") {
            if let Some(rest) = slug.strip_prefix(&date).or_else(|| strip_iso_date(&slug)) {
                slug = rest.trim_start_matches('-').to_string();
            }
        }
        path = path.replace("{date}", &date).replace("{slug}", &slug);
        for (k, v) in fields {
            path = path.replace(&format!("{{{k}}}"), &slugify(v));
        }
        if let Some(start) = path.find('{') {
            let end = path[start..]
                .find('}')
                .map_or(path.len(), |e| start + e + 1);
            return Err(ArcanaError::Ledger(format!(
                "note type `{}` needs field {} to build its path `{}`",
                self.name,
                &path[start..end],
                self.path
            )));
        }
        Ok(path)
    }
}

/// `slug` without a leading `YYYY-MM-DD`, if it has one.
fn strip_iso_date(slug: &str) -> Option<&str> {
    let b = slug.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|x| x.iter().all(u8::is_ascii_digit));
    (digits(0..4)
        && b.get(4) == Some(&b'-')
        && digits(5..7)
        && b.get(7) == Some(&b'-')
        && digits(8..10))
    .then(|| &slug[10..])
}

pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// All note types defined in the vault.
pub fn load_types(vault_root: &Path) -> Result<BTreeMap<String, NoteType>> {
    let dir = vault_root.join(".arcana").join("types");
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let text = std::fs::read_to_string(&path)?;
        let mut t: NoteType = toml::from_str(&text)
            .map_err(|e| ArcanaError::Config(format!("note type {}: {e}", path.display())))?;
        t.name = name.clone();
        out.insert(name, t);
    }
    Ok(out)
}

/// The kind of a note: its frontmatter `type:` (through the type table), else
/// the most specific matching rule in `[ledger.kinds]`, else the default. A
/// rule is a path prefix (`writing/`) or a glob (`projects/*/lab/**`); the
/// longest rule that matches wins.
pub fn kind_of(
    rel_path: &str,
    frontmatter_type: Option<&str>,
    types: &BTreeMap<String, NoteType>,
    prefixes: &BTreeMap<String, NoteKind>,
    default: NoteKind,
) -> NoteKind {
    if let Some(t) = frontmatter_type.and_then(|t| types.get(t)) {
        return t.kind;
    }
    prefixes
        .iter()
        .filter(|(p, _)| rule_matches(p, rel_path))
        .max_by_key(|(p, _)| p.len())
        .map_or(default, |(_, k)| *k)
}

fn rule_matches(rule: &str, rel_path: &str) -> bool {
    if rule.contains('*') {
        globset::Glob::new(rule)
            .map(|g| g.compile_matcher().is_match(rel_path))
            .unwrap_or(false)
    } else {
        rel_path.starts_with(rule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_template() {
        let t: NoteType =
            toml::from_str("kind = \"log\"\npath = \"projects/{project}/lab/{date}-{slug}.md\"")
                .unwrap();
        let mut f = BTreeMap::new();
        f.insert("project".into(), "Bench PSU 12V".into());
        let p = t.render_path("Ripple FFT, run 2", &f).unwrap();
        assert!(p.starts_with("projects/bench-psu-12v/lab/"));
        assert!(p.ends_with("-ripple-fft-run-2.md"));
        assert!(t.render_path("x", &BTreeMap::new()).is_err());
        let p = t.render_path("2026-01-02 — Steak tacos v1", &f).unwrap();
        assert!(p.ends_with("-steak-tacos-v1.md"), "{p}");
        assert_eq!(p.matches("2026-01-02").count(), 0, "{p}");
    }

    #[test]
    fn kind_resolution() {
        let mut prefixes = BTreeMap::new();
        prefixes.insert("blog/".to_string(), NoteKind::Writing);
        let types = BTreeMap::new();
        assert_eq!(
            kind_of("blog/post.md", None, &types, &prefixes, NoteKind::Chapter),
            NoteKind::Writing
        );
        assert_eq!(
            kind_of("notes/x.md", None, &types, &prefixes, NoteKind::Chapter),
            NoteKind::Chapter
        );
        prefixes.insert("projects/*/lab/**".to_string(), NoteKind::Log);
        assert_eq!(
            kind_of(
                "projects/psu/lab/2026-05-10-test.md",
                None,
                &types,
                &prefixes,
                NoteKind::Chapter
            ),
            NoteKind::Log
        );
        assert_eq!(
            kind_of(
                "projects/psu/overview.md",
                None,
                &types,
                &prefixes,
                NoteKind::Chapter
            ),
            NoteKind::Chapter
        );
    }
}
