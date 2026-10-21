//! Frozen workspace candidates expose bounded leases and preserve a split UTF-8 scalar.
//! A search hit cannot bypass validation of the remaining immutable candidate.
use std::{cell::Cell, ops::Range};

use crate::original_backing::{ORIGINAL_READ_BYTES, OriginalBacking};

use super::MarkdownWorkspaceError;

type Result<T> = std::result::Result<T, MarkdownWorkspaceError>;

/// Own one frozen source without a whole-extent borrow API. Failed lease reclamation
/// revokes the backing; explicit close reports whether its actual mappings were returned.
pub(in super::super) struct BoundedMarkdownText {
    backing: Option<OriginalBacking>,
    bytes: usize,
    validated: Cell<bool>,
}
impl BoundedMarkdownText {
    /// Bind the captured backing to its exact extent, including a genuinely empty file.
    pub(super) fn new(backing: Option<OriginalBacking>, bytes: usize) -> Result<Self> {
        if backing.as_ref().map_or(0, OriginalBacking::len) != bytes {
            // A missing nonempty backing is a capture failure, never an empty document.
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        Ok(Self {
            backing,
            bytes,
            validated: Cell::new(false),
        })
    }

    /// Expose only the frozen byte count; this does not authorize a same-sized slice.
    pub(in super::super) fn len(&self) -> usize {
        self.bytes
    }

    /// Borrow at most 4 KiB and publish the callback result only after that same lease
    /// is reclaimed successfully and the original query still owns its authority.
    pub(in super::super) fn with_bytes<R>(
        &self,
        range: Range<usize>,
        is_current: &impl Fn() -> bool,
        visit: impl FnOnce(&[u8]) -> R,
    ) -> Result<R> {
        if !is_current() {
            // Cancellation precedes the source operation so obsolete work maps no new bytes.
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        if range.start > range.end || range.end > self.bytes || range.len() > ORIGINAL_READ_BYTES {
            // Reject oversized or invalid ranges before mapping; no whole-map fallback exists.
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        let output = match &self.backing {
            Some(backing) => backing
                .with_bytes(range, visit)
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?,
            // Construction proved that the absent backing denotes an actual empty source.
            None => visit(&[]),
        };
        if !is_current() {
            // Even a Rust callback can revoke its query; successful reclamation cannot
            // restore that authority or publish a now-obsolete partial result.
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        Ok(output)
    }

    /// Share each finite lease between validation and the consumer. Any invalid scalar,
    /// including one discovered after a useful match, invalidates the candidate result.
    pub(in super::super) fn visit_validated_bytes(
        &self,
        is_current: &impl Fn() -> bool,
        mut visit: impl FnMut(usize, &[u8]) -> Result<()>,
    ) -> Result<()> {
        let mut utf8 = Utf8Continuation::default();
        let mut start = 0;
        while start < self.bytes {
            // Keep only one lease and a possible split scalar live across each iteration.
            let end = start.saturating_add(ORIGINAL_READ_BYTES).min(self.bytes);
            self.with_bytes(start..end, is_current, |bytes| {
                // Validate before consumption. A split final scalar remains provisional
                // until the next window or actual EOF establishes its validity.
                utf8.push(bytes)?;
                visit(start, bytes)
            })??;
            start = end;
        }
        if !is_current() {
            // Empty inputs and cancellation after the final callback still need this check.
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        utf8.finish()?;
        // Only real EOF with no incomplete scalar establishes this immutable source fact.
        self.validated.set(true);
        Ok(())
    }

    /// Reuse validation of this exact frozen source while retaining query cancellation.
    pub(in super::super) fn ensure_validated(&self, is_current: &impl Fn() -> bool) -> Result<()> {
        if self.validated.get() {
            if is_current() {
                // A later digest pass need not revalidate the same immutable source.
                return Ok(());
            }
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        // A consumer that inspected only a local interval still owes complete UTF-8
        // validation before its candidate-derived result can be published.
        self.visit_validated_bytes(is_current, |_, _| Ok(()))
    }

    /// Stream the same frozen source into the caller-owned writer. A write failure
    /// stops traversal; the outer destination guard owns any rollback.
    pub(in super::super) fn copy_to(&self, output: &mut impl std::io::Write) -> Result<()> {
        self.visit_validated_bytes(&|| true, |_, bytes| {
            output
                .write_all(bytes)
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)
        })
    }

    /// Stream an exact half-open source range without materializing that entire range.
    /// The caller owns both destination rollback and any separate UTF-8 validation.
    pub(in super::super) fn copy_range_to(
        &self,
        range: Range<usize>,
        output: &mut impl std::io::Write,
    ) -> Result<()> {
        if range.start > range.end || range.end > self.bytes {
            // Range copies cannot widen their authority beyond the captured source.
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        let mut start = range.start;
        while start < range.end {
            // A large explicit export remains a sequence of finite leases, not one slice.
            let end = start.saturating_add(ORIGINAL_READ_BYTES).min(range.end);
            self.with_bytes(start..end, &|| true, |bytes| {
                output
                    .write_all(bytes)
                    .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)
            })??;
            start = end;
        }
        Ok(())
    }

    /// Consume the source owner and report fallible reclamation before returning a result.
    pub(super) fn close(mut self) -> Result<()> {
        if let Some(backing) = self.backing.take() {
            // Reclamation failure outranks callback success; Drop cannot defer an unmap
            // failure until after the caller has already published its result.
            backing
                .close()
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        }
        Ok(())
    }
}

/// Retain only one split scalar, never the validated prefix or a paragraph mirror.
#[derive(Default)]
struct Utf8Continuation {
    bytes: [u8; 4],
    used: usize,
    needed: usize,
}
impl Utf8Continuation {
    /// Validate a window after completing any scalar that began in the previous lease.
    fn push(&mut self, mut bytes: &[u8]) -> Result<()> {
        if self.used != 0 {
            // Complete the original scalar before validating unrelated bytes in this window.
            let take = (self.needed - self.used).min(bytes.len());
            self.bytes[self.used..self.used + take].copy_from_slice(&bytes[..take]);
            self.used += take;
            bytes = &bytes[take..];
            if self.used != self.needed {
                // The next lease continues this scalar; finish rejects it at actual EOF.
                return Ok(());
            }
            std::str::from_utf8(&self.bytes[..self.used])
                .map_err(|_| MarkdownWorkspaceError::InvalidUtf8Document)?;
            self.used = 0;
        }
        match std::str::from_utf8(bytes) {
            // A complete valid remainder leaves no carry for the next lease.
            Ok(_) => Ok(()),
            Err(error) if error.error_len().is_none() => {
                // The standard validator proved the prefix. Preserve only its one to
                // three trailing bytes and the width implied by the actual lead byte.
                let tail = &bytes[error.valid_up_to()..];
                self.needed = match tail[0] {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => return Err(MarkdownWorkspaceError::InvalidUtf8Document),
                };
                self.bytes[..tail.len()].copy_from_slice(tail);
                self.used = tail.len();
                Ok(())
            }
            // A definite malformed scalar cannot become valid by supplying another lease.
            Err(_) => Err(MarkdownWorkspaceError::InvalidUtf8Document),
        }
    }
    /// Reject an unfinished scalar at real EOF rather than silently truncating the source.
    fn finish(self) -> Result<()> {
        if self.used != 0 {
            Err(MarkdownWorkspaceError::InvalidUtf8Document)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
