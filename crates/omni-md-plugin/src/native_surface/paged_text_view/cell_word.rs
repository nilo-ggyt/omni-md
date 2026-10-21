//! Word-query intent from an actual Cell hit; only the actual successor Row/Cell publication grants display Source access.

use super::NativeMarkdownPagedHeadAffinity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Row start, column, and affinity identify the queried Cell, retaining no old Source, text, or publication claim.
/// When a commit changes canonical coordinates, map row_start/query head only through that same actual edit.
pub struct NativeMarkdownPagedCellWordTarget {
    pub row_start: usize,
    pub column: usize,
    pub affinity: NativeMarkdownPagedHeadAffinity,
}
