//! Actual stable records from the original GCB job. A GB2 endpoint does not establish a final
//! ordinal, the last complete cluster, or completion of the source scan.
use super::*;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGraphemePrefixFacts {
    /// Source uses whole-source ordinals; visible domains cover only visible boundaries and structural endpoints.
    pub ordinal_domain: MarkdownGraphemeOrdinalDomain,
    /// Continuously committed boundaries in this domain; Dense counts logical records, with physical I/O separate.
    pub known_boundary_count: usize,
    /// Queryable positions through this frontier have been classified; hidden interiors remain outside the domain.
    pub confirmed_through: Option<MarkdownGraphemeBoundary>,
    pub last_committed_boundary: Option<MarkdownGraphemeBoundary>,
    pub complete: bool,
    /// Final count is available only after the original algorithm and index sealing complete.
    pub total_count: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemePrefixRead {
    Pending,
    Read,
}

/// Exclusively borrow the original job without cloning it or advancing its machine while borrowed.
/// This view creates no file descriptor, text copy, or replacement index.
pub struct MarkdownGraphemePrefix<'a> {
    job: &'a mut MarkdownGraphemeJob,
    authority: MarkdownLayoutTextAuthority,
    facts: MarkdownGraphemePrefixFacts,
}

impl MarkdownGraphemeJob {
    /// Borrow currently proven record coverage without changing Ready semantics. Unscanned Paged
    /// input has no stable records; exact Dense whole-source proof already supplies its complete domain.
    pub fn stable_prefix(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<MarkdownGraphemePrefix<'_>> {
        self.guard(authority)?;
        let complete = self.machine.ready;
        let known_boundary_count = self.index.len();
        let last_committed_boundary = if self.index.is_dense() {
            Some(MarkdownGraphemeBoundary {
                byte: self.source.byte_len(),
                utf16: self.source.utf16_len(),
            })
        } else {
            self.machine.last_committed_boundary()
        };
        let facts = MarkdownGraphemePrefixFacts {
            ordinal_domain: self.ordinal_domain(),
            known_boundary_count,
            confirmed_through: self.machine.frontier,
            last_committed_boundary,
            complete,
            total_count: complete.then_some(known_boundary_count),
        };
        self.guard(authority)?;
        Ok(MarkdownGraphemePrefix {
            job: self,
            authority: authority.clone(),
            facts,
        })
    }
}

impl MarkdownGraphemePrefix<'_> {
    pub fn facts(&mut self) -> Result<MarkdownGraphemePrefixFacts> {
        self.job.guard(&self.authority)?;
        Ok(self.facts)
    }

    /// GB2 certifies only the actual immutable Source endpoint, without scanning its unknown tail
    /// or guessing a final ordinal. Next, Previous, and the last cluster still require actual records.
    pub fn source_end_boundary(&mut self) -> Result<MarkdownGraphemeBoundary> {
        self.job.guard(&self.authority)?;
        let end = MarkdownGraphemeBoundary {
            byte: self.job.source.byte_len(),
            utf16: self.job.source.utf16_len(),
        };
        self.job.guard(&self.authority)?;
        Ok(end)
    }

    /// Read at most 256 committed records in the current domain; unknown records stay Pending
    /// without changing output. Adjacent visible ordinals do not establish hidden GCB counts or clusters.
    pub fn read_boundaries(
        &mut self,
        range: Range<usize>,
        output: &mut [MarkdownGraphemeBoundary],
    ) -> Result<MarkdownGraphemePrefixRead> {
        let result = (|| {
            self.job.guard(&self.authority)?;
            let maximum = self
                .job
                .source
                .utf16_len()
                .checked_add(1)
                .ok_or(MarkdownGraphemeError::CoordinateOverflow)?;
            if range.end.checked_sub(range.start) != Some(output.len())
                || output.len() > 256
                || range.end > maximum
            {
                return Err(MarkdownGraphemeError::InvalidRange);
            }
            if range.end > self.facts.known_boundary_count {
                return if self.facts.complete {
                    Err(MarkdownGraphemeError::InvalidRange)
                } else {
                    Ok(MarkdownGraphemePrefixRead::Pending)
                };
            }
            self.job.index.read_batch(range, output)?;
            Ok(MarkdownGraphemePrefixRead::Read)
        })();
        // Original Source failure precedes parameter/metadata results. A failed batch cannot
        // publish any prefix that may already have been written to the caller's buffer.
        let result = match self.job.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result.and_then(|answer| {
                self.job.guard(&self.authority)?;
                Ok(answer)
            }),
        };
        if result.is_err() {
            self.job.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests;
