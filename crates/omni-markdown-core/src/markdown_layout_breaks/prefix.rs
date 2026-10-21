//! Schedule actual ASCII-frontier dependencies without changing original LB stable-prefix, opportunity-query, or complete-EOF semantics.
use super::*;

/// Issue only from original GCB IsBoundary success or the same Source's actual GB2 endpoint, never from an integer alone.
/// Weak Source/scope identities retain no text, descriptor, index page, or BreakJob lifetime.
pub(crate) struct MarkdownCertifiedGraphemeEndpoint {
    source: MarkdownLayoutTextAuthority,
    scope: Weak<()>,
    boundary: crate::MarkdownGraphemeBoundary,
}
impl MarkdownCertifiedGraphemeEndpoint {
    pub(crate) fn same_scope(&self, other: &Self) -> bool {
        // Equality compares original scope identity only; consuming the endpoint separately checks authority and scope liveness.
        self.source == other.source && Weak::ptr_eq(&self.scope, &other.scope)
    }

    pub(crate) fn boundary(
        &self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<crate::MarkdownGraphemeBoundary> {
        if authority != &self.source {
            return Err(MarkdownLayoutBreakError::SourceMismatch);
        }
        if self.scope.strong_count() == 0 {
            return Err(MarkdownLayoutBreakError::Poisoned);
        }
        Ok(self.boundary)
    }
}

impl MarkdownLayoutBreakJob {
    /// Use actual GCB queries inside the source; actual EOF borrows the original machine's GB2 coordinates without rescanning unindexed text.
    /// GB2 grants no final ordinal, Previous result, or full readiness; retain existing Pending demand and issue no ticket for Known(None).
    pub(crate) fn certified_grapheme_endpoint(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        offset: usize,
    ) -> Result<(GraphemeAnswer, Option<MarkdownCertifiedGraphemeEndpoint>)> {
        let answer = if offset == self.maximum[1] {
            // Even a long CRLF source ineligible for Dense has a GB2 endpoint from this same immutable Source's actual extent.
            // Check original health/identity around the stable-prefix borrow, ending it before recheck without another reader or scan progress.
            self.require_source(authority)?;
            let result = self
                .payload
                .as_mut()
                .ok_or(MarkdownLayoutBreakError::Poisoned)?
                .grapheme
                .stable_prefix(authority)
                .and_then(|mut prefix| prefix.source_end_boundary());
            self.require_source(authority)?;
            match result {
                // Pass original GB2 coordinates into the same-scope ticket factory without changing index completion state.
                Ok(boundary) => GraphemeAnswer::Known(Some(boundary)),
                // Child cancellation/failure retires original scope too; known length cannot revive endpoint authority.
                Err(error) => return self.poison(error.into()),
            }
        } else {
            // Internal UTF-16 positions still need actual grapheme classification; EOF cannot classify low surrogates or CRLF interiors.
            self.grapheme_query(authority, offset, MarkdownGraphemeQuery::IsBoundary)?
        };
        self.require_source(authority)?;
        let endpoint = match answer {
            GraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == offset => {
                Some(MarkdownCertifiedGraphemeEndpoint {
                    source: authority.clone(),
                    scope: Rc::downgrade(
                        &self
                            .payload
                            .as_ref()
                            .ok_or(MarkdownLayoutBreakError::Poisoned)?
                            .endpoint_scope,
                    ),
                    boundary,
                })
            }
            GraphemeAnswer::Known(Some(_)) => {
                return self.poison(MarkdownLayoutBreakError::InconsistentCoordinates);
            }
            _ => None,
        };
        Ok((answer, endpoint))
    }

    /// Borrow only records actually committed by original GCB; known GB2 coordinates supply neither final ordinals nor full readiness.
    pub(crate) fn grapheme_prefix_facts(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<crate::markdown_graphemes::MarkdownGraphemePrefixFacts> {
        self.require_source(authority)?;
        let total = self.ready_grapheme_count(authority)?;
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .stable_prefix(authority)
            .and_then(
                |mut prefix: crate::markdown_graphemes::MarkdownGraphemePrefix<'_>| prefix.facts(),
            );
        self.require_source(authority)?;
        match result {
            Ok(value) if value.total_count == total => Ok(value),
            Ok(_) => self.poison(MarkdownLayoutBreakError::InconsistentCoordinates),
            Err(error) => self.poison(error.into()),
        }
    }

    /// Read at most 256 actual original records; unknown ordinals request GCB progress instead of implying an empty table or EOF.
    pub(crate) fn read_grapheme_prefix(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        range: std::ops::Range<usize>,
        output: &mut [crate::MarkdownGraphemeBoundary],
    ) -> Result<bool> {
        self.require_source(authority)?;
        if self
            .payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .is_ready()
        {
            return self.read_ready_graphemes(authority, range, output);
        } // Complete tables use the original bounded read entry; unfinished ones use a stable-prefix ticket without exchanging EOF meanings.
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .stable_prefix(authority)
            .and_then(|mut prefix| prefix.read_boundaries(range, output));
        self.require_source(authority)?;
        match result {
            Ok(crate::markdown_graphemes::MarkdownGraphemePrefixRead::Read) => Ok(true),
            Ok(crate::markdown_graphemes::MarkdownGraphemePrefixRead::Pending) => {
                self.grapheme_source_demand = true;
                Ok(false)
            }
            Err(error) => self.poison(error.into()),
        }
    }

    pub(crate) fn take_grapheme_source_demand(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<bool> {
        self.require_source(authority)?;
        Ok(std::mem::take(&mut self.grapheme_source_demand))
    }

    /// Advance only the actually blocked child next step, without separately spending a full caller budget on both LB and GCB.
    pub(crate) fn step_dependency(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        grapheme: bool,
        budget: MarkdownLineBreakBudget,
    ) -> Result<MarkdownLayoutBreakStep> {
        let result = (|| {
            self.require_source(authority)?;
            if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 {
                return Err(MarkdownLayoutBreakError::InvalidRequest);
            }
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLayoutBreakError::Poisoned)?;
            let progress = if grapheme {
                let value = payload.grapheme.step(
                    authority,
                    MarkdownGraphemeBudget {
                        bytes: budget.bytes,
                        scalars: budget.scalars,
                        records: budget.records,
                    },
                )?;
                MarkdownLayoutBreakStep {
                    bytes: value.bytes,
                    scalars: value.scalars,
                    records: value.records,
                    ready: false,
                    needs_dictionary: payload.line.storage_profile().needs_dictionary,
                }
            } else {
                let value = payload.line.step(authority, budget)?;
                MarkdownLayoutBreakStep {
                    bytes: value.bytes,
                    scalars: value.scalars,
                    records: value.records,
                    ready: false,
                    needs_dictionary: value.needs_dictionary,
                }
            };
            let ready = payload.grapheme.is_ready() && payload.line.is_ready();
            self.require_source(authority)?;
            Ok(MarkdownLayoutBreakStep { ready, ..progress })
        })();
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.poison(error),
        }
    }

    /// Only completed Dense GCB from the original exact whole-source Printable proof allows LB to remain an unfinished prefix.
    /// Reuse GCB's actual source proof without rereading text or guessing the character domain from length/generation.
    pub(crate) fn ascii_prefix_ready(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<bool> {
        self.require_source(authority)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        let ready =
            payload.grapheme.is_ready() && payload.grapheme.storage_profile().dense_printable_ascii;
        self.require_source(authority)?;
        Ok(ready)
    }

    /// Take actual line-source demand from the latest original query; this scalar grants no boundary, range, or EOF authority.
    /// Consumers end their current borrow before advancing the same job next step; failures close indexes through original poison handling.
    pub(crate) fn take_line_source_demand(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<bool> {
        self.require_source(authority)?;
        Ok(std::mem::take(&mut self.line_source_demand))
    }

    #[cfg(test)]
    pub(crate) fn line_source_bytes_read_for_test(&mut self) -> usize {
        self.payload
            .as_ref()
            .expect("测试只检查仍活的原LB任务")
            .line
            .storage_profile()
            .source_bytes_read
    }
}

#[cfg(test)]
mod tests;
