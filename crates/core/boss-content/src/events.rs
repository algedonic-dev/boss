//! Domain event subjects for content operations.
//!
//! Per `docs/design/projection-rebuilders.md`: state events carry
//! the full row state so the rebuild path can reconstruct
//! `bulletins` + `bulletin_dismissals` from the event log alone.
//!
//! Manual-section events (SECTION_CREATED / SECTION_UPDATED) are
//! reserved for the follow-up commit that ships the manual
//! rebuilder.

pub const BULLETIN_CREATED: &str = "content.bulletin.created";
pub const BULLETIN_UPDATED: &str = "content.bulletin.updated";
pub const BULLETIN_DELETED: &str = "content.bulletin.deleted";
pub const BULLETIN_DISMISSED: &str = "content.bulletin.dismissed";

// File references — see docs/architecture-decisions.md §Content,
// files, knowledge.
//
// `content.file.attached` payload is the full FileRef JSON so the
// rebuilder can reconstruct file_refs without reading the table; the
// event id IS the file_ref row id (per the design's identity choice).
//
// `content.file.detached` payload carries `{ file_id, target_kind,
// target_id, deleted_by, deleted_at }` so the rebuilder can flip the
// deleted_at column without a side-channel lookup.
pub const FILE_ATTACHED: &str = "content.file.attached";
pub const FILE_DETACHED: &str = "content.file.detached";
