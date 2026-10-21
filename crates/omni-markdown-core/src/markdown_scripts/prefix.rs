//! Borrows stable ranges from the same script job. An open prefix is never
//! reported as a complete run, paragraph, or shaping item.
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownScriptPrefixKind {
    /// A complete run actually committed to the original store.
    ClosedRun,
    /// One proven script and complete clusters cover this stable prefix only;
    /// the actual run endpoint remains unknown.
    StableOpenPrefix,
}

/// Private construction and no cloning keep the job borrowed while the consumer
/// holds this range, preventing mutation of the same pending candidate.
pub struct MarkdownStableScriptPrefix<'a> {
    job: &'a MarkdownScriptJob,
    authority: MarkdownLayoutTextAuthority,
    run: MarkdownScriptRun,
    kind: MarkdownScriptPrefixKind,
}

pub enum MarkdownScriptPrefixAnswer<'a> {
    Pending,
    Known(Option<MarkdownStableScriptPrefix<'a>>),
}

impl MarkdownStableScriptPrefix<'_> {
    /// Reads original range facts, rechecking source failure/cancellation before
    /// returning. A borrowed value never grants permanent source authority.
    pub fn facts(&self) -> Result<MarkdownScriptRun> {
        self.job.guard(&self.authority)?;
        let result = self.run;
        self.job.guard(&self.authority)?;
        Ok(result)
    }

    /// Distinguishes a closed run from a stable open portion. Neither grants
    /// grapheme-boundary authority or native live-document editing permission.
    pub fn kind(&self) -> Result<MarkdownScriptPrefixKind> {
        self.job.guard(&self.authority)?;
        Ok(self.kind)
    }

    /// Lends the original weak payload identity only after the same live job
    /// confirms cancellation and source health; no document owner is returned.
    pub fn source_authority(&self) -> Result<&MarkdownLayoutTextAuthority> {
        self.job.guard(&self.authority)?;
        Ok(&self.authority)
    }
}

impl MarkdownScriptJob {
    /// Queries committed runs or complete-cluster prefixes with an established
    /// script, without advancing the source or inventing a terminal endpoint.
    pub fn stable_prefix_at(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        unit: MarkdownScriptUnit,
        offset: usize,
    ) -> Result<MarkdownScriptPrefixAnswer<'_>> {
        let result = (|| {
            self.guard(authority)?;
            let closed = self.find_run(unit, offset)?;
            let candidate = match closed {
                MarkdownScriptAnswer::Known(Some(run)) => {
                    Some((run, MarkdownScriptPrefixKind::ClosedRun))
                }
                MarkdownScriptAnswer::Known(None) => None,
                MarkdownScriptAnswer::Pending => {
                    // Only a stable complete-cluster prefix may answer an unfinished run.
                    // Its half-open coordinate range must actually contain this query.
                    self.machine.stable_open_prefix().and_then(|run| {
                        let (start, end) = match unit {
                            MarkdownScriptUnit::Byte => (run.start_byte, run.end_byte),
                            MarkdownScriptUnit::Utf16 => (run.start_utf16, run.end_utf16),
                        };
                        (start <= offset && offset < end)
                            .then_some((run, MarkdownScriptPrefixKind::StableOpenPrefix))
                    })
                }
            };
            self.guard(authority)?;
            Ok((
                candidate,
                matches!(closed, MarkdownScriptAnswer::Known(None)),
            ))
        })();
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        }; // Metadata failures cannot hide the same source's already confirmed first errno.
        let (candidate, eof) = match result {
            Ok(value) => value,
            Err(error) => {
                self.fail(&error);
                return Err(error);
            }
        };
        match candidate {
            Some((run, kind)) => Ok(MarkdownScriptPrefixAnswer::Known(Some(
                MarkdownStableScriptPrefix {
                    job: self,
                    authority: authority.clone(),
                    run,
                    kind,
                },
            ))),
            // Confirmed absence differs from an open candidate that has not yet
            // established its unique script or reached this source coordinate.
            None if eof => Ok(MarkdownScriptPrefixAnswer::Known(None)),
            None => Ok(MarkdownScriptPrefixAnswer::Pending),
        }
    }
}

#[cfg(test)]
mod tests;
