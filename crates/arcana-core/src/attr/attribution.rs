//! Per-token attribution of one note, and how it is carried across an edit.

use std::collections::HashMap;

use similar::{capture_diff_slices, Algorithm, DiffOp};

use super::author::{Author, Origin, TokAttr};
use super::token::{tokenize, Token};

/// Inserted runs at least this long are matched against deleted runs, so
/// moved text keeps its author.
pub const MOVE_MIN_TOKENS: usize = 8;

/// The author of every token of one note's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub note_id: String,
    pub authors: Vec<Author>,
    pub tokens: Vec<TokAttr>,
}

/// How newly inserted tokens are labelled.
#[derive(Debug, Clone)]
pub struct Insertion {
    pub author: Author,
    pub origin: Origin,
    pub unreviewed: bool,
    pub policy: Option<String>,
}

impl Attribution {
    /// Every token of `content` credited to one author.
    pub fn uniform(note_id: String, content: &str, ins: &Insertion) -> Self {
        let mut a = Attribution {
            note_id,
            authors: Vec::new(),
            tokens: Vec::new(),
        };
        let attr = a.attr_for(ins);
        a.tokens = vec![attr; tokenize(content).len()];
        a
    }

    pub fn intern(&mut self, author: &Author) -> usize {
        if let Some(i) = self.authors.iter().position(|a| a == author) {
            return i;
        }
        self.authors.push(author.clone());
        self.authors.len() - 1
    }

    fn attr_for(&mut self, ins: &Insertion) -> TokAttr {
        TokAttr {
            author: self.intern(&ins.author),
            origin: ins.origin,
            unreviewed: ins.unreviewed,
            policy: ins.policy.clone(),
        }
    }

    pub fn author_of(&self, attr: &TokAttr) -> &Author {
        &self.authors[attr.author]
    }

    /// Attribution for `new` after editing `old`, whose attribution is `self`.
    /// Unchanged tokens keep their author; inserted tokens get `ins`, except
    /// runs of at least [`MOVE_MIN_TOKENS`] that match deleted text, which keep
    /// the deleted text's author.
    pub fn carry_forward(&self, old: &str, new: &str, ins: &Insertion) -> Attribution {
        let old_toks = tokenize(old);
        let new_toks = tokenize(new);
        let old_text: Vec<&str> = old_toks.iter().map(|t| t.text(old)).collect();
        let new_text: Vec<&str> = new_toks.iter().map(|t| t.text(new)).collect();
        debug_assert_eq!(old_text.len(), self.tokens.len());

        let mut out = Attribution {
            note_id: self.note_id.clone(),
            authors: self.authors.clone(),
            tokens: Vec::with_capacity(new_text.len()),
        };
        let fresh = out.attr_for(ins);
        let mut slots: Vec<Option<TokAttr>> = vec![None; new_text.len()];
        let mut deleted: Vec<usize> = Vec::new();
        let mut inserted: Vec<(usize, usize)> = Vec::new();

        for op in capture_diff_slices(Algorithm::Patience, &old_text, &new_text) {
            match op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } => {
                    for k in 0..len {
                        slots[new_index + k] = self.tokens.get(old_index + k).cloned();
                    }
                }
                DiffOp::Delete {
                    old_index, old_len, ..
                } => deleted.extend(old_index..old_index + old_len),
                DiffOp::Insert {
                    new_index, new_len, ..
                } => inserted.push((new_index, new_len)),
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    deleted.extend(old_index..old_index + old_len);
                    inserted.push((new_index, new_len));
                }
            }
        }

        self.carry_moves(&old_text, &new_text, &deleted, &inserted, &mut slots);
        out.tokens = slots
            .into_iter()
            .map(|s| s.unwrap_or_else(|| fresh.clone()))
            .collect();
        out.compact();
        out
    }

    /// Match inserted runs against deleted tokens by content and copy their
    /// attribution, so cut-and-paste keeps the original author.
    fn carry_moves(
        &self,
        old_text: &[&str],
        new_text: &[&str],
        deleted: &[usize],
        inserted: &[(usize, usize)],
        slots: &mut [Option<TokAttr>],
    ) {
        if deleted.len() < MOVE_MIN_TOKENS {
            return;
        }
        // Contiguous deleted runs, indexed by their first MOVE_MIN_TOKENS tokens.
        let runs = contiguous(deleted);
        let mut index: HashMap<&[&str], Vec<usize>> = HashMap::new();
        for &(start, len) in &runs {
            for s in start..start + len.saturating_sub(MOVE_MIN_TOKENS - 1) {
                index
                    .entry(&old_text[s..s + MOVE_MIN_TOKENS])
                    .or_default()
                    .push(s);
            }
        }
        let run_end = |s: usize| {
            runs.iter()
                .find(|&&(st, l)| s >= st && s < st + l)
                .map(|&(st, l)| st + l)
                .unwrap_or(s)
        };

        for &(ins_start, ins_len) in inserted {
            let mut i = ins_start;
            let end = ins_start + ins_len;
            while i + MOVE_MIN_TOKENS <= end {
                let key = &new_text[i..i + MOVE_MIN_TOKENS];
                let Some(&src) = index.get(key).and_then(|v| v.first()) else {
                    i += 1;
                    continue;
                };
                let src_end = run_end(src);
                let mut k = 0;
                while i + k < end && src + k < src_end && new_text[i + k] == old_text[src + k] {
                    slots[i + k] = self.tokens.get(src + k).cloned();
                    k += 1;
                }
                i += k.max(1);
            }
        }
    }

    /// Drop authors no token refers to, renumbering the rest.
    pub fn compact(&mut self) {
        let mut used = vec![false; self.authors.len()];
        for t in &self.tokens {
            used[t.author] = true;
        }
        let mut remap = vec![0; self.authors.len()];
        let mut kept = Vec::new();
        for (i, a) in self.authors.drain(..).enumerate() {
            if used[i] {
                remap[i] = kept.len();
                kept.push(a);
            }
        }
        self.authors = kept;
        for t in &mut self.tokens {
            t.author = remap[t.author];
        }
    }

    /// Consecutive tokens with identical attribution, as `(start, len)`.
    pub fn runs(&self) -> Vec<(usize, usize, &TokAttr)> {
        let mut out: Vec<(usize, usize, &TokAttr)> = Vec::new();
        for (i, t) in self.tokens.iter().enumerate() {
            match out.last_mut() {
                Some((_, len, prev)) if *prev == t => *len += 1,
                _ => out.push((i, 1, t)),
            }
        }
        out
    }

    /// Token counts by broad author class.
    pub fn summary(&self) -> Summary {
        let mut s = Summary::default();
        for t in &self.tokens {
            match self.author_of(t) {
                Author::Human {
                    via: super::author::HumanVia::Declared,
                } => s.declared += 1,
                Author::Human { .. } => s.human += 1,
                Author::Agent { .. } => s.agent += 1,
                Author::Unattributed => s.unattributed += 1,
            }
            if t.unreviewed {
                s.unreviewed += 1;
            }
        }
        s
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Summary {
    /// Words arcana saw the human write (or accept as a light edit).
    pub human: usize,
    /// Words the human claimed at import.
    pub declared: usize,
    pub agent: usize,
    pub unattributed: usize,
    pub unreviewed: usize,
}

fn contiguous(sorted: &[usize]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &i in sorted {
        match out.last_mut() {
            Some((s, l)) if *s + *l == i => *l += 1,
            _ => out.push((i, 1)),
        }
    }
    out
}

/// Tokens of `content` paired with their attribution, for rendering.
pub fn zip<'a>(content: &'a str, attr: &'a Attribution) -> Vec<(Token, &'a TokAttr)> {
    tokenize(content)
        .into_iter()
        .zip(attr.tokens.iter())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::author::HumanVia;

    fn human() -> Insertion {
        Insertion {
            author: Author::Human {
                via: HumanVia::Observed,
            },
            origin: Origin::Composed,
            unreviewed: false,
            policy: None,
        }
    }

    fn agent() -> Insertion {
        Insertion {
            author: Author::Agent {
                agent: "test".into(),
                session: "s1".into(),
                request: None,
            },
            origin: Origin::Composed,
            unreviewed: true,
            policy: None,
        }
    }

    fn classes(content: &str, a: &Attribution) -> String {
        zip(content, a)
            .iter()
            .map(|(_, t)| match a.author_of(t) {
                Author::Human { .. } => 'H',
                Author::Agent { .. } => 'A',
                Author::Unattributed => '?',
            })
            .collect()
    }

    #[test]
    fn insertion_is_credited_to_the_writer() {
        let old = "My sentence here.";
        let a = Attribution::uniform("n".into(), old, &human());
        let new = "My sentence here. Agent adds this.";
        let b = a.carry_forward(old, new, &agent());
        assert_eq!(classes(new, &b), "HHHHAAAA");
    }

    #[test]
    fn rewrap_changes_nothing() {
        let old = "one two three four five six seven eight nine";
        let a = Attribution::uniform("n".into(), old, &human());
        let new = "one two three four\nfive six seven\neight nine";
        let b = a.carry_forward(old, new, &agent());
        assert_eq!(b.summary().agent, 0);
        assert_eq!(b, a);
    }

    #[test]
    fn moved_paragraph_keeps_its_author() {
        let mine = "I wrote this first paragraph myself.";
        let theirs = "The agent wrote this second paragraph with several words in it.";
        let old = format!("{mine}\n\n{theirs}");
        let mut a = Attribution::uniform("n".into(), mine, &human());
        let b = a.carry_forward(mine, &old, &agent());
        a = b;
        // Human moves the agent paragraph above their own.
        let new = format!("{theirs}\n\n{mine}");
        let c = a.carry_forward(&old, &new, &human());
        let n_theirs = tokenize(theirs).len();
        let n_mine = tokenize(mine).len();
        assert_eq!(
            classes(&new, &c),
            format!("{}{}", "A".repeat(n_theirs), "H".repeat(n_mine))
        );
    }

    #[test]
    fn compact_drops_unused_authors() {
        let old = "alpha beta";
        let a = Attribution::uniform("n".into(), old, &human());
        let b = a.carry_forward(old, "gamma delta", &agent());
        assert_eq!(b.authors.len(), 1);
        assert!(b.authors[0].is_agent());
    }
}
