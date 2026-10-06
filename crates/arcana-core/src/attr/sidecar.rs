//! The sidecar file: a note's attribution on disk.
//!
//! Adapted from iA Writer's Markdown Annotations v0.2
//! (<https://github.com/iainc/Markdown-Annotations>): an author table with
//! sigils and ranges plus a content hash. Differences: it lives in its own
//! file, ranges count word tokens rather than graphemes, and it records two
//! hashes — the token stream (is the attribution still valid?) and the bytes
//! (did the file change at all?). One run per line, so git diffs are readable:
//!
//! ```text
//! arcana-attr 1
//! note 0b6f2c1e-… tokenizer tok-1
//! tokens 5e1a… bytes c903…
//! @0 human observed
//! &1 agent claude-code 91ab "add the bandwidth derivation"
//! 0,212 @0 composed
//! 212,9 &1 citation-insert
//! 221,40 &1 composed unreviewed
//! ```

use sha2::{Digest, Sha256};

use super::attribution::Attribution;
use super::author::{Author, HumanVia, Origin, TokAttr};
use super::token::{tokenize, TOKENIZER};
use crate::errors::{ArcanaError, Result};

const MAGIC: &str = "arcana-attr 1";

/// A parsed sidecar: the attribution plus the hashes of what it describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidecar {
    pub attribution: Attribution,
    pub tokens_hash: String,
    pub bytes_hash: String,
}

/// Hash of the token sequence: unchanged by re-wrapping.
pub fn tokens_hash(content: &str) -> String {
    let mut h = Sha256::new();
    for t in tokenize(content) {
        h.update(t.text(content).as_bytes());
        h.update([0u8]);
    }
    hex(&h.finalize())
}

pub fn bytes_hash(content: &str) -> String {
    hex(&Sha256::digest(content.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Sidecar {
    pub fn for_content(attribution: Attribution, content: &str) -> Self {
        Sidecar {
            attribution,
            tokens_hash: tokens_hash(content),
            bytes_hash: bytes_hash(content),
        }
    }

    /// Does this sidecar describe exactly `content`?
    pub fn matches(&self, content: &str) -> bool {
        self.bytes_hash == bytes_hash(content)
    }

    pub fn render(&self) -> String {
        let a = &self.attribution;
        let mut out = format!(
            "{MAGIC}\nnote {} tokenizer {TOKENIZER}\ntokens {} bytes {}\n",
            a.note_id, self.tokens_hash, self.bytes_hash
        );
        for (i, author) in a.authors.iter().enumerate() {
            out.push_str(&match author {
                Author::Human { via } => format!("@{i} human {}\n", via.as_str()),
                Author::Agent {
                    agent,
                    session,
                    request,
                } => {
                    let req = request
                        .as_deref()
                        .map(|r| format!(" {}", serde_json::to_string(r).unwrap_or_default()))
                        .unwrap_or_default();
                    format!("&{i} agent {} {}{req}\n", word(agent), word(session))
                }
                Author::Unattributed => format!("?{i} unattributed\n"),
            });
        }
        for (start, len, t) in a.runs() {
            let sigil = sigil(&a.authors[t.author]);
            out.push_str(&format!(
                "{start},{len} {sigil}{} {}",
                t.author,
                t.origin.as_str()
            ));
            if let Some(p) = &t.policy {
                out.push_str(&format!(" {p}"));
            }
            if t.unreviewed {
                out.push_str(" unreviewed");
            }
            out.push('\n');
        }
        out
    }

    pub fn parse(text: &str) -> Result<Self> {
        let bad = |line: &str| ArcanaError::Ledger(format!("malformed sidecar line: {line:?}"));
        let mut lines = text.lines();
        if lines.next() != Some(MAGIC) {
            return Err(ArcanaError::Ledger("not an arcana-attr 1 sidecar".into()));
        }
        let header = lines.next().ok_or_else(|| bad(""))?;
        let h: Vec<&str> = header.split_whitespace().collect();
        let (note_id, tokenizer) = match h.as_slice() {
            ["note", id, "tokenizer", tok] => (id.to_string(), *tok),
            _ => return Err(bad(header)),
        };
        if tokenizer != TOKENIZER {
            return Err(ArcanaError::Ledger(format!(
                "sidecar uses tokenizer {tokenizer}, this arcana uses {TOKENIZER}"
            )));
        }
        let hashes = lines.next().ok_or_else(|| bad(""))?;
        let (tokens_hash, bytes_hash) = match hashes.split_whitespace().collect::<Vec<_>>()[..] {
            ["tokens", t, "bytes", b] => (t.to_string(), b.to_string()),
            _ => return Err(bad(hashes)),
        };

        let mut authors = Vec::new();
        let mut tokens = Vec::new();
        for line in lines {
            let first = line.chars().next();
            match first {
                Some('@' | '&' | '?') => authors.push(parse_author(line).ok_or_else(|| bad(line))?),
                Some(c) if c.is_ascii_digit() => {
                    let (start, len, attr) = parse_run(line).ok_or_else(|| bad(line))?;
                    if start != tokens.len() || attr.author >= authors.len() {
                        return Err(bad(line));
                    }
                    tokens.extend(std::iter::repeat_n(attr, len));
                }
                _ if line.trim().is_empty() => {}
                _ => return Err(bad(line)),
            }
        }
        Ok(Sidecar {
            attribution: Attribution {
                note_id,
                authors,
                tokens,
            },
            tokens_hash,
            bytes_hash,
        })
    }
}

fn sigil(a: &Author) -> char {
    match a {
        Author::Human { .. } => '@',
        Author::Agent { .. } => '&',
        Author::Unattributed => '?',
    }
}

/// Agent names and sessions are single words in the format.
fn word(s: &str) -> String {
    let w: String = s
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .collect();
    if w.is_empty() {
        "-".into()
    } else {
        w
    }
}

fn parse_author(line: &str) -> Option<Author> {
    let mut parts = line.splitn(5, ' ');
    let _key = parts.next()?;
    match parts.next()? {
        "human" => Some(Author::Human {
            via: HumanVia::parse(parts.next()?)?,
        }),
        "unattributed" => Some(Author::Unattributed),
        "agent" => {
            let agent = parts.next()?.to_string();
            let session = parts.next()?.to_string();
            let request = match parts.next() {
                Some(q) => Some(serde_json::from_str::<String>(q).ok()?),
                None => None,
            };
            Some(Author::Agent {
                agent,
                session,
                request,
            })
        }
        _ => None,
    }
}

fn parse_run(line: &str) -> Option<(usize, usize, TokAttr)> {
    let mut parts = line.split_whitespace();
    let (start, len) = parts.next()?.split_once(',')?;
    let author: usize = parts.next()?.get(1..)?.parse().ok()?;
    let origin = Origin::parse(parts.next()?)?;
    let mut attr = TokAttr {
        author,
        origin,
        unreviewed: false,
        policy: None,
    };
    for flag in parts {
        match flag {
            "unreviewed" => attr.unreviewed = true,
            p if p.contains('@') => attr.policy = Some(p.to_string()),
            _ => return None,
        }
    }
    Some((start.parse().ok()?, len.parse().ok()?, attr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::attribution::Insertion;

    #[test]
    fn round_trip() {
        let old = "Resonance is the response of a system.";
        let a = Attribution::uniform(
            "id-1".into(),
            old,
            &Insertion {
                author: Author::Human {
                    via: HumanVia::Observed,
                },
                origin: Origin::Composed,
                unreviewed: false,
                policy: None,
            },
        );
        let new = "Resonance is the response of a linear system [@french1971].";
        let b = a.carry_forward(
            old,
            new,
            &Insertion {
                author: Author::Agent {
                    agent: "claude code".into(),
                    session: "s1".into(),
                    request: Some("add \"citation\"".into()),
                },
                origin: Origin::CitationInsert,
                unreviewed: true,
                policy: None,
            },
        );
        let sc = Sidecar::for_content(b, new);
        let text = sc.render();
        let back = Sidecar::parse(&text).unwrap();
        assert_eq!(back.render(), text);
        assert!(back.matches(new));
        assert!(text.contains("agent claude-code s1 \"add \\\"citation\\\"\""));
    }

    #[test]
    fn rewrap_keeps_tokens_hash() {
        assert_eq!(tokens_hash("a b c"), tokens_hash("a\nb   c"));
        assert_ne!(bytes_hash("a b c"), bytes_hash("a\nb   c"));
    }
}
