//! Computes a content digest once per immutable root. External-file conflict
//! checks still inspect the real file rather than trusting this derived cache.

use std::sync::Weak;

use super::*;

/// One weak root identity and one 32-byte digest. Replacing the root overwrites
/// this entry instead of growing a generation-indexed digest table.
pub(super) struct RevisionCache {
    root: Option<Weak<MarkdownPieceTreeNode>>,
    revision: MarkdownDocumentRevision,
}

impl RevisionCache {
    /// Seeds the actual digest only after constructors verify UTF-8, aggregates,
    /// SHA, and the source-file identity.
    pub(super) fn seeded(root: &PieceTreeLink, revision: MarkdownDocumentRevision) -> Self {
        Self {
            root: root.as_ref().map(Arc::downgrade),
            revision,
        }
    }

    fn matches(&self, root: &PieceTreeLink) -> bool {
        match (&self.root, root) {
            (Some(weak), Some(root)) => std::ptr::eq(weak.as_ptr(), Arc::as_ptr(root)),
            (None, None) => true, // An empty root hashes empty input, independently of generation or filename.
            _ => false, // Empty/nonempty roots differ; the weak control block prevents identity reuse.
        }
    }

    pub(super) fn read(
        &mut self,
        source: &impl PieceByteSource,
        root: &PieceTreeLink,
    ) -> Result<MarkdownDocumentRevision, MarkdownDocumentBufferError> {
        source.source_health()?; // A hit cannot bypass permanent shared-mapping I/O failure.
        let revision = if self.matches(root) {
            self.revision
        } else {
            // Stream the real pieces once for an edited root. Do not materialize a
            // whole-document string or a vector of digest input fragments.
            let mut hasher = Sha256::new();
            let hashed = hash_piece_tree(source, root.as_deref(), &mut hasher);
            source.source_health()?; // Preserve a concurrent source failure rather than hiding it behind a quota error.
            hashed?;
            MarkdownDocumentRevision(hasher.finalize().into())
        };
        source.source_health()?; // Publish only after returning the final real lease and checking source health.
        self.root = root.as_ref().map(Arc::downgrade);
        self.revision = revision;
        Ok(revision)
    }
}

#[cfg(test)]
mod tests;
