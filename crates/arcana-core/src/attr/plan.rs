//! Turning an agent's requested edit into what may actually happen to a note.
//!
//! The agent never says whose words it is touching; the planner reads that
//! from the note's attribution. Anything that touches, or inserts into, a block
//! containing words not written by an agent becomes a suggestion.

use serde::{Deserialize, Serialize};
use similar::{capture_diff_slices, Algorithm, DiffOp};

use super::attribution::Attribution;
use super::author::Origin;
use super::blocks::{block_at, blocks, BlockKind};
use super::token::tokenize;
use super::types::NoteKind;
use crate::errors::{ArcanaError, Result};

/// One edit as an agent asks for it. Text anchors, not offsets: `find` must
/// occur exactly once in the note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RawEdit {
    /// Replace the unique occurrence of `find` with `with`.
    Replace { find: String, with: String },
    /// Insert `text` immediately after the unique occurrence of `find`. Start
    /// `text` with a blank line to add a new paragraph rather than extend one.
    InsertAfter { find: String, text: String },
    /// Add `text` as new paragraphs at the end of the section under `heading`
    /// (matched without the leading `#`s), or at the end of the note.
    Append {
        #[serde(default)]
        heading: Option<String>,
        text: String,
    },
}

/// A resolved edit: replace `range` of the note with `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl Resolved {
    pub fn apply(&self, content: &str) -> String {
        format!(
            "{}{}{}",
            &content[..self.start],
            self.text,
            &content[self.end..]
        )
    }
}

pub fn resolve(content: &str, edit: &RawEdit) -> Result<Resolved> {
    match edit {
        RawEdit::Replace { find, with } => {
            let start = unique(content, find)?;
            Ok(Resolved {
                start,
                end: start + find.len(),
                text: with.clone(),
            })
        }
        RawEdit::InsertAfter { find, text } => {
            let pos = unique(content, find)? + find.len();
            Ok(Resolved {
                start: pos,
                end: pos,
                text: text.clone(),
            })
        }
        RawEdit::Append { heading, text } => {
            let pos = match heading {
                None => content.len(),
                Some(h) => section_end(content, h)?,
            };
            let before = content[..pos].trim_end_matches('\n');
            let pos = before.len();
            let lead = if before.is_empty() { "" } else { "\n\n" };
            let after = &content[pos..];
            let trail = if after.trim().is_empty() {
                "\n"
            } else {
                "\n\n"
            };
            Ok(Resolved {
                start: pos,
                end: pos + (after.len() - after.trim_start_matches('\n').len()),
                text: format!("{lead}{}{trail}", text.trim_matches('\n')),
            })
        }
    }
}

fn unique(content: &str, find: &str) -> Result<usize> {
    if find.is_empty() {
        return Err(ArcanaError::Ledger("`find` must not be empty".into()));
    }
    let mut hits = content.match_indices(find);
    match (hits.next(), hits.next()) {
        (Some((i, _)), None) => Ok(i),
        (None, _) => Err(ArcanaError::Ledger(format!(
            "text to find does not occur in the note: {find:?}"
        ))),
        (Some(_), Some(_)) => Err(ArcanaError::Ledger(format!(
            "text to find occurs more than once; quote more context: {find:?}"
        ))),
    }
}

/// Byte offset of the end of the section under `heading`.
fn section_end(content: &str, heading: &str) -> Result<usize> {
    let mut pos = 0;
    let mut level = None;
    for line in content.split_inclusive('\n') {
        let t = line.trim_end();
        let hashes = t.chars().take_while(|&c| c == '#').count();
        if hashes > 0 {
            match level {
                None if t[hashes..].trim().eq_ignore_ascii_case(heading.trim()) => {
                    level = Some(hashes)
                }
                Some(l) if hashes <= l => return Ok(pos),
                _ => {}
            }
        }
        pos += line.len();
    }
    if level.is_some() {
        Ok(content.len())
    } else {
        Err(ArcanaError::Ledger(format!(
            "no heading {heading:?} in the note"
        )))
    }
}

/// Whose words an edit touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Touch {
    /// Inserts new paragraphs between existing ones.
    NewBlocks,
    /// Changes or extends text written only by agents.
    AgentText,
    /// Changes or extends a block containing words not written by an agent.
    ProtectedText,
    Frontmatter,
}

pub fn touch(content: &str, attr: &Attribution, r: &Resolved) -> Touch {
    let bs = blocks(content);
    let toks = tokenize(content);
    let in_frontmatter = bs
        .iter()
        .any(|b| b.kind == BlockKind::Frontmatter && r.start < b.end.max(b.start + 1));
    if in_frontmatter {
        return Touch::Frontmatter;
    }
    // The byte span whose ownership matters: the replaced range, widened to the
    // whole block when inserting inside (or onto the end of) a block.
    let pure_insert = content[r.start..r.end].trim().is_empty();
    let (lo, hi) = if pure_insert {
        let joins_block = !r.text.starts_with("\n\n") && !content[..r.start].ends_with("\n\n");
        let containing =
            block_at(&bs, r.start).or_else(|| bs.iter().find(|b| b.end == r.start && joins_block));
        match containing {
            Some(b) if joins_block || r.start > b.start => (b.start, b.end),
            _ => return Touch::NewBlocks,
        }
    } else {
        (r.start, r.end)
    };
    let protected = toks
        .iter()
        .zip(&attr.tokens)
        .filter(|(t, _)| t.start < hi && t.end > lo)
        .any(|(_, a)| !attr.author_of(a).is_agent());
    if protected {
        Touch::ProtectedText
    } else {
        Touch::AgentText
    }
}

/// What happens to an edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "kebab-case")]
pub enum Disposition {
    /// Written now, marked unreviewed.
    Apply,
    /// Queued for review; the words are the agent's once accepted.
    Gate,
    /// Queued for review as a suggestion to protected text.
    Suggest,
    Refuse {
        reason: String,
    },
}

/// What happens to an edit. `review_agent_edits` holds an agent's changes to
/// its own text for review; without it they are applied, marked unreviewed.
/// Edits touching the human's words are always suggestions.
pub fn dispose(kind: NoteKind, touch: Touch, review_agent_edits: bool) -> Disposition {
    use Disposition::*;
    match (kind, touch) {
        (NoteKind::Chapter, Touch::AgentText) if !review_agent_edits => Apply,
        (_, Touch::Frontmatter) => Refuse {
            reason: "agents cannot edit frontmatter".into(),
        },
        (NoteKind::Chapter, Touch::NewBlocks) => Apply,
        (NoteKind::Chapter, Touch::AgentText) => Gate,
        (NoteKind::Writing, Touch::AgentText) => Gate,
        (NoteKind::Log, Touch::NewBlocks) => Apply,
        (NoteKind::Log, _) => Refuse {
            reason: "logs are append-only for agents; use `append`".into(),
        },
        (NoteKind::Pointer, Touch::NewBlocks | Touch::AgentText) => Apply,
        (_, Touch::ProtectedText) | (NoteKind::Writing, Touch::NewBlocks) => Suggest,
    }
}

/// If replacing `old` with `new` is a light edit under `light-edit@1`, its
/// origin. Bounds: punctuation/case/markup-only changes of any size up to 10
/// tokens; one inserted citation (`[@key]` or `[^n]`) and nothing else; or at
/// most 3 changed word tokens.
pub fn light_edit(old: &str, new: &str) -> Option<Origin> {
    let ot: Vec<&str> = tokenize(old).iter().map(|t| t.text(old)).collect();
    let nt: Vec<&str> = tokenize(new).iter().map(|t| t.text(new)).collect();
    let mut deleted = Vec::new();
    let mut inserted = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, &ot, &nt) {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete {
                old_index, old_len, ..
            } => deleted.extend_from_slice(&ot[old_index..old_index + old_len]),
            DiffOp::Insert {
                new_index, new_len, ..
            } => inserted.extend_from_slice(&nt[new_index..new_index + new_len]),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                deleted.extend_from_slice(&ot[old_index..old_index + old_len]);
                inserted.extend_from_slice(&nt[new_index..new_index + new_len]);
            }
        }
    }
    if deleted.is_empty() && inserted.is_empty() {
        return Some(Origin::Mechanical);
    }
    let is_word = |t: &&str| t.chars().any(char::is_alphanumeric);
    let words = |v: &[&str]| -> Vec<String> {
        v.iter()
            .filter(|t| is_word(t))
            .map(|t| t.to_lowercase())
            .collect()
    };
    if deleted.is_empty() && is_citation(&inserted.concat()) {
        return Some(Origin::CitationInsert);
    }
    if words(&deleted) == words(&inserted) && deleted.len() + inserted.len() <= 10 {
        return Some(Origin::Mechanical);
    }
    let changed = deleted
        .iter()
        .filter(|t| is_word(t))
        .count()
        .max(inserted.iter().filter(|t| is_word(t)).count());
    (changed <= 3).then_some(Origin::Copyedit)
}

fn is_citation(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("[@") || s.starts_with("[^")) && s.ends_with(']') && s.matches('[').count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::attribution::Insertion;
    use crate::attr::author::{Author, HumanVia};

    fn human_note(s: &str) -> Attribution {
        Attribution::uniform(
            "n".into(),
            s,
            &Insertion {
                author: Author::Human {
                    via: HumanVia::Observed,
                },
                origin: Origin::Composed,
                unreviewed: false,
                policy: None,
            },
        )
    }

    #[test]
    fn inserting_inside_a_human_paragraph_is_protected() {
        let s = "My own words here.\n\nSecond para.\n";
        let a = human_note(s);
        let r = resolve(
            s,
            &RawEdit::InsertAfter {
                find: "own".into(),
                text: " carefully chosen".into(),
            },
        )
        .unwrap();
        assert_eq!(touch(s, &a, &r), Touch::ProtectedText);
        let r = resolve(
            s,
            &RawEdit::InsertAfter {
                find: "here.".into(),
                text: "\n\nA new agent paragraph.".into(),
            },
        )
        .unwrap();
        assert_eq!(touch(s, &a, &r), Touch::NewBlocks);
        let r = resolve(
            s,
            &RawEdit::Append {
                heading: None,
                text: "Appended.".into(),
            },
        )
        .unwrap();
        assert_eq!(touch(s, &a, &r), Touch::NewBlocks);
        assert_eq!(
            r.apply(s),
            "My own words here.\n\nSecond para.\n\nAppended.\n"
        );
    }

    #[test]
    fn append_to_section() {
        let s = "# A\n\none\n\n# B\n\ntwo\n";
        let r = resolve(
            s,
            &RawEdit::Append {
                heading: Some("A".into()),
                text: "more".into(),
            },
        )
        .unwrap();
        assert_eq!(r.apply(s), "# A\n\none\n\nmore\n\n# B\n\ntwo\n");
    }

    #[test]
    fn anchors_must_be_unique() {
        assert!(resolve(
            "a a",
            &RawEdit::Replace {
                find: "a".into(),
                with: "b".into()
            }
        )
        .is_err());
    }

    #[test]
    fn light_edit_bounds() {
        assert_eq!(light_edit("teh cat", "the cat"), Some(Origin::Copyedit));
        assert_eq!(
            light_edit("the cat ,sat", "The cat, sat"),
            Some(Origin::Mechanical)
        );
        assert_eq!(
            light_edit("as shown.", "as shown [@french1971]."),
            Some(Origin::CitationInsert)
        );
        assert_eq!(
            light_edit(
                "a short line",
                "an entirely different and much longer sentence"
            ),
            None
        );
    }
}
