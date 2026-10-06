//! Word-level attribution: who wrote every word of a note, recorded at write
//! time, and the single write path that keeps it true.
//!
//! See `docs/dev/proposals/2026-10-06-attribution-ledger-refactor.md`.

pub mod attribution;
pub mod author;
pub mod blocks;
pub mod ledger;
pub mod plan;
pub mod sidecar;
pub mod token;
pub mod types;

pub use attribution::{Attribution, Summary};
pub use author::{AgentIdentity, Author, Grant, HumanVia, Origin};
pub use ledger::{
    word_diff, Decided, EditOutcome, EditRequest, EditResult, Ledger, NoteState, Observed, Pending,
    UnreviewedSpan,
};
pub use plan::{Disposition, RawEdit, Touch};
pub use types::{NoteKind, NoteType};
