//! Who wrote a word, and what a writer must present to write.
//!
//! Two separate things:
//!
//! - [`Author`] is the stored label on a run of words. It is output: built by
//!   the ledger from a [`Grant`], written to the sidecar, and readable by
//!   anyone. It cannot be turned back into permission to write.
//! - [`Grant`] is what a write presents. Its constructors are private to this
//!   crate and each grant is consumed by one write, so code outside
//!   `arcana-core` (the MCP server, agents) can obtain only an agent grant.

use serde::Serialize;

/// How a human edit was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HumanVia {
    /// Edited in `$EDITOR` from the review TUI.
    Editor,
    /// Accepted a light edit in review; the words stay the human's.
    Review,
    /// Changed on disk by something other than arcana while agents are
    /// barred from writing the vault.
    Observed,
    /// Declared the human's at import: older writing arcana never saw being
    /// written. Reported separately from witnessed words.
    Declared,
}

impl HumanVia {
    pub fn as_str(self) -> &'static str {
        match self {
            HumanVia::Editor => "editor",
            HumanVia::Review => "review",
            HumanVia::Observed => "observed",
            HumanVia::Declared => "declared",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "editor" => HumanVia::Editor,
            "review" => HumanVia::Review,
            "observed" => HumanVia::Observed,
            "declared" => HumanVia::Declared,
            _ => return None,
        })
    }
}

/// The stored author of a run of words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Author {
    Human {
        via: HumanVia,
    },
    Agent {
        /// Client or model name, e.g. `claude-code`.
        agent: String,
        /// One connection or session.
        session: String,
        /// What the human asked for, as the agent reported it.
        request: Option<String>,
    },
    /// Changed by a path nothing witnessed.
    Unattributed,
}

impl Author {
    pub fn is_human(&self) -> bool {
        matches!(self, Author::Human { .. })
    }

    pub fn is_agent(&self) -> bool {
        matches!(self, Author::Agent { .. })
    }
}

/// How a run of words came to be there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Composed,
    /// Spelling, punctuation, capitalisation or markup only.
    Mechanical,
    /// A small, meaning-preserving wording change within the light-edit bound.
    Copyedit,
    /// A citation or reference inserted by an agent.
    CitationInsert,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Composed => "composed",
            Origin::Mechanical => "assist:mechanical",
            Origin::Copyedit => "assist:copyedit",
            Origin::CitationInsert => "citation-insert",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "composed" => Origin::Composed,
            "assist:mechanical" => Origin::Mechanical,
            "assist:copyedit" => Origin::Copyedit,
            "citation-insert" => Origin::CitationInsert,
            _ => return None,
        })
    }
}

/// The named rule that admitted a light edit; recorded on the run.
pub const LIGHT_EDIT_POLICY: &str = "light-edit@1";

/// Attribution of one token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokAttr {
    /// Index into the attribution's author table.
    pub author: usize,
    pub origin: Origin,
    /// Agent text applied without review, until the human reviews it.
    pub unreviewed: bool,
    /// The light-edit policy that admitted this token, if any.
    pub policy: Option<String>,
}

/// Permission for one write. Consumed by the write it authorizes.
#[derive(Debug)]
pub struct Grant {
    pub(crate) kind: GrantKind,
}

#[derive(Debug)]
pub(crate) enum GrantKind {
    /// An agent's own write. New words are the agent's.
    Agent(AgentIdentity),
    /// The human edited in `$EDITOR` from review. New words are the human's.
    Editor,
    /// A file change arcana did not make. New words are the human's while the
    /// write boundary holds, unattributed otherwise.
    Observed { boundary_holds: bool },
}

/// Who an agent is, as reported by its MCP client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    pub agent: String,
    pub session: String,
}

impl Grant {
    /// Any caller may obtain an agent grant: it can only credit an agent.
    pub fn agent(identity: AgentIdentity) -> Self {
        Grant {
            kind: GrantKind::Agent(identity),
        }
    }

    pub(crate) fn editor() -> Self {
        Grant {
            kind: GrantKind::Editor,
        }
    }

    pub(crate) fn observed(boundary_holds: bool) -> Self {
        Grant {
            kind: GrantKind::Observed { boundary_holds },
        }
    }

    /// The author credited with words this grant inserts.
    pub(crate) fn author(&self, request: Option<&str>) -> Author {
        match &self.kind {
            GrantKind::Agent(id) => Author::Agent {
                agent: id.agent.clone(),
                session: id.session.clone(),
                request: request.map(str::to_string),
            },
            GrantKind::Editor => Author::Human {
                via: HumanVia::Editor,
            },
            GrantKind::Observed {
                boundary_holds: true,
            } => Author::Human {
                via: HumanVia::Observed,
            },
            GrantKind::Observed {
                boundary_holds: false,
            } => Author::Unattributed,
        }
    }

    pub(crate) fn is_agent(&self) -> bool {
        matches!(self.kind, GrantKind::Agent(_))
    }
}
