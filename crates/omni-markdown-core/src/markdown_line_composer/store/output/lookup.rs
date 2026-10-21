//! Locate lines inside the same sealed prefix. Each actual 64-byte read consumes
//! one record of work; no document copy or second GCB index is constructed.

use super::*;

/// Choose the side of a Source endpoint shared by adjacent lines. This policy
/// grants neither caret/glyph geometry nor Native visibility authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedLineAffinity {
    Upstream,
    Downstream,
}

/// A Source target may lie inside a scalar: lookup returns an actual complete
/// line, without granting permission to edit at that raw coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedLineLookupTarget {
    Source {
        utf16: usize,
        affinity: MarkdownComposedLineAffinity,
    },
    Ordinal(usize),
}

pub enum MarkdownComposedLineLookupAnswer {
    Pending,
    Found(MarkdownComposedLine),
    /// This original prefix does not cover the target. It proves neither whole-
    /// document EOF nor permission to borrow a line from a newer output.
    PrefixNotCovered,
}

pub struct MarkdownComposedLineLookupStep {
    pub records: usize,
    pub answer: MarkdownComposedLineLookupAnswer,
}

#[derive(Clone, Copy)]
enum Edge {
    Last,
    Skip,
    Previous,
}

/// Fixed cursor state carries an already-read tail across work quanta, without
/// retaining a Reader, Source, Font, or another business owner.
pub struct MarkdownComposedLineLookup {
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    prefix: Range<usize>,
    count: usize,
    last: Option<usize>,
    target: MarkdownComposedLineLookupTarget,
    next: Option<usize>,
    tail: Option<[u8; 64]>,
    edge: Edge,
    current: Option<MarkdownComposedLine>,
    found: Option<MarkdownComposedLine>,
    done: bool,
    failed: bool,
}

#[derive(Clone, Copy)]
enum Relation {
    Before,
    Found,
    After,
}

impl MarkdownComposedLineLookup {
    /// Compare the actual line with the original target, respecting association
    /// at shared soft-line boundaries and the real Source endpoint.
    fn relation(&self, line: &MarkdownComposedLine, source_len: usize) -> Relation {
        match self.target {
            MarkdownComposedLineLookupTarget::Ordinal(target) => {
                match target.cmp(&line.facts.ordinal) {
                    std::cmp::Ordering::Less => Relation::Before,
                    std::cmp::Ordering::Equal => Relation::Found,
                    std::cmp::Ordering::Greater => Relation::After,
                }
            }
            MarkdownComposedLineLookupTarget::Source { utf16, affinity } => {
                let range = &line.facts.source_utf16;
                if utf16 < range.start
                    || (utf16 == range.start
                        && utf16 != 0
                        && line.facts.ordinal != 0
                        && affinity == MarkdownComposedLineAffinity::Upstream)
                {
                    Relation::Before // Upstream belongs to the actual preceding line, not an equal successor offset.
                } else if utf16 < range.end
                    || (utf16 == range.end
                        && (affinity == MarkdownComposedLineAffinity::Upstream
                            || utf16 == source_len))
                {
                    Relation::Found // Actual EOF can use the final line; a terminal empty line is searched first.
                } else {
                    Relation::After // An unprepared downstream endpoint cannot reuse the prefix's last line.
                }
            }
        }
    }

    /// Copy authenticated weak facts only. The caller still checks original
    /// output health before returning even a repeated terminal answer.
    fn answer(&self) -> MarkdownComposedLineLookupAnswer {
        if let Some(line) = &self.found {
            // Copy neither reader/file ownership nor a newer Source ticket.
            MarkdownComposedLineLookupAnswer::Found(MarkdownComposedLine {
                output: line.output.clone(),
                header: line.header,
                end: line.end,
                previous: line.previous,
                skip: line.skip,
                base_level: line.base_level,
                facts: line.facts.clone(),
            })
        } else if self.done {
            // Exhausted coverage is distinct from a cursor awaiting more work.
            MarkdownComposedLineLookupAnswer::PrefixNotCovered
        } else {
            MarkdownComposedLineLookupAnswer::Pending
        }
    }

    /// Resume from the original current line's actual predecessor; a broken
    /// chain is corruption, not legitimate uncovered-prefix backpressure.
    fn previous(&mut self) -> Result<()> {
        let line = self.current.as_ref().ok_or(Error::InconsistentCoverage)?;
        self.next = line.previous;
        self.edge = Edge::Previous;
        if self.next.is_none() {
            return Err(Error::InconsistentCoverage);
        } // An admitted target cannot precede ordinal zero.
        Ok(())
    }

    /// Authenticate each actual backward edge before moving the cursor. Skips
    /// may bypass lines, but cannot reverse record/Source order or forge adjacency.
    fn accept(&mut self, line: MarkdownComposedLine, source_len: usize) -> Result<()> {
        if let Some(current) = &self.current {
            let ordinal = line.facts.ordinal;
            if ordinal >= current.facts.ordinal
                || line.end >= current.header
                || line.facts.source_utf16.end > current.facts.source_utf16.start
            {
                return Err(Error::InconsistentCoverage);
            } // Multi-line skips still preserve original record and Source monotonicity.
            match self.edge {
                Edge::Skip if current.skip != Some((line.end, ordinal)) => {
                    // The actual skip must point to this exact earlier line.
                    return Err(Error::InconsistentCoverage);
                }
                Edge::Previous
                    if current.previous != Some(line.end)
                        || ordinal.checked_add(1) != Some(current.facts.ordinal)
                        || line.facts.source_utf16.end != current.facts.source_utf16.start =>
                {
                    // A predecessor edge also proves adjacent ordinal and
                    // continuous Source coverage, not just a smaller record.
                    return Err(Error::InconsistentCoverage);
                }
                _ => {} // Nonadjacent skips authenticate only their actual declared target.
            }
            if ordinal.checked_add(1) == Some(current.facts.ordinal)
                && line.facts.source_utf16.end != current.facts.source_utf16.start
            {
                // Even a skip that happens to be adjacent must preserve the
                // actual joining Source endpoint.
                return Err(Error::InconsistentCoverage);
            }
        } else if !matches!(self.edge, Edge::Last)
            || line.facts.ordinal.checked_add(1) != Some(self.count)
            || Some(line.end) != self.last
            || line.facts.source_utf16.end != self.prefix.end
        {
            // The initial record must be the original sealed prefix's last
            // line, before any predecessor/skip search can begin.
            return Err(Error::InconsistentCoverage);
        }
        match self.relation(&line, source_len) {
            Relation::Found => {
                // Retain only the authenticated found line and release cursor
                // predecessor state; repeated answers still check output health.
                self.found = Some(line);
                self.done = true;
                self.current = None;
                self.next = None;
            }
            Relation::Before => {
                // Search earlier lines from this authenticated current record.
                let skip = line.skip.map(|value| value.0);
                self.current = Some(line);
                if let Some(skip) = skip {
                    // Prefer the writer's actual skip edge when available.
                    self.next = Some(skip);
                    self.edge = Edge::Skip;
                } else {
                    // Without a skip, only the true predecessor can advance.
                    self.previous()?;
                }
            }
            Relation::After if matches!(self.edge, Edge::Skip) => {
                self.previous()?; // An overshooting skip resumes from the original current predecessor.
            }
            Relation::After if matches!(self.edge, Edge::Last) => {
                // The target is beyond this actual prefix; do not acquire a
                // successor writer or label it whole-document EOF.
                self.done = true;
                self.next = None;
            }
            Relation::After => return Err(Error::InconsistentCoverage),
        }
        Ok(())
    }
}

impl MarkdownComposedLines {
    /// Capture the original sealed prefix without reading line records or
    /// document endpoints. A newer writer cannot extend this cursor's authority.
    ///
    /// # Errors
    /// Source/cancellation/FD failure or an invalid Source target passes through
    /// the original output's terminal error/release boundary.
    pub fn line_lookup(
        &mut self,
        target: MarkdownComposedLineLookupTarget,
    ) -> Result<MarkdownComposedLineLookup> {
        let result = (|| {
            self.require(&self.authority)?;
            let len = self
                .payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .source
                .utf16_len();
            let outside = match target {
                MarkdownComposedLineLookupTarget::Ordinal(ordinal) => ordinal >= self.count,
                MarkdownComposedLineLookupTarget::Source { utf16, affinity } => {
                    if utf16 > len || utf16 < self.range.start {
                        // Reject targets outside Source or before this sealed
                        // prefix; a raw coordinate cannot manufacture coverage.
                        return Err(Error::InvalidRequest);
                    }
                    utf16 > self.range.end
                        || (utf16 == self.range.end
                            && utf16 < len
                            && affinity == MarkdownComposedLineAffinity::Downstream)
                }
            };
            Ok(MarkdownComposedLineLookup {
                output: Arc::downgrade(&self.identity),
                source: self.authority.clone(),
                prefix: self.range.clone(),
                count: self.count,
                last: self.last,
                target,
                next: (!outside).then_some(self.last).flatten(),
                tail: None,
                edge: Edge::Last,
                current: None,
                found: None,
                done: outside,
                failed: false,
            })
        })();
        self.finish(result)
    }

    /// Charge each actual fixed-record read once. Tail/head may span two quota-
    /// one calls; empty scheduling does not count as IO. Locating an original
    /// prefix line grants no glyph, caret, or Native visibility authority.
    ///
    /// # Errors
    /// Reject foreign/expired cursors, invalid quotas, corrupt edges, original
    /// Source failures, and cancellation without restarting from another output.
    pub fn step_line_lookup(
        &mut self,
        lookup: &mut MarkdownComposedLineLookup,
        record_budget: usize,
    ) -> Result<MarkdownComposedLineLookupStep> {
        let result = (|| {
            self.require(&self.authority)?; // Metadata hits/repeated terminal answers preserve original Source priority.
            if lookup.failed {
                // An earlier failed cursor is terminal, even if its offsets
                // still fit this output's numerical range.
                return Err(Error::Poisoned);
            }
            if !Weak::ptr_eq(&lookup.output, &Arc::downgrade(&self.identity))
                || lookup.source != self.authority
                || lookup.prefix != self.range
                || lookup.count != self.count
                || lookup.last != self.last
            {
                // Preserve original allocation, Source, prefix, and last-line
                // identity together; equal ranges alone cannot rebind a cursor.
                return Err(Error::SourceMismatch);
            }
            if record_budget == 0 || record_budget > STEP {
                // Caller work remains inside the existing fixed record quantum.
                return Err(Error::InvalidRequest);
            }
            let source_len = self
                .payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .source
                .utf16_len();
            let mut records = 0;
            while records < record_budget && !lookup.done {
                let end = lookup.next.ok_or(Error::InconsistentCoverage)?;
                if let Some(tail) = lookup.tail.take() {
                    // Consume the already-paid tail and charge only this real
                    // header read before the shared pure decoder authenticates it.
                    let header = self.line_header(end, &tail)?;
                    records += 1;
                    let head = self.row(header)?;
                    let line = self.decode_line(end, tail, head)?;
                    lookup.accept(line, source_len)?;
                } else {
                    // Pay for the actual tail read now and retain it for the
                    // next header quantum, without prefetching unrelated rows.
                    records += 1;
                    let tail = self.row(end)?;
                    self.line_header(end, &tail)?;
                    lookup.tail = Some(tail);
                } // This synchronous Core boundary holds no borrow across await or Native delivery.
            }
            self.require(&self.authority)?;
            Ok(MarkdownComposedLineLookupStep {
                records,
                answer: lookup.answer(),
            })
        })();
        let result = self.finish(result);
        if result.is_err() {
            // Failure permanently ends this cursor; a later call must not reset
            // its allowance or publish partially authenticated line facts.
            lookup.failed = true;
        }
        result
    }
}

#[cfg(test)]
#[path = "lookup_tests.rs"]
mod tests;
