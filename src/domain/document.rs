//! A fetched document.

use super::path::DocPath;
use super::value::{Fields, UpdateTime};

/// A document as read from Firestore.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// Where the document lives.
    pub path: DocPath,
    /// When it was last modified (nanosecond precision).
    pub update_time: UpdateTime,
    /// Its fields.
    pub data: Fields,
}
