//! Folded display analyzes only the complete B paragraphs actually needed, retaining at most
//! the previous and current original UBA results.

use super::*;
use crate::{
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    markdown_document_buffer::FrozenBidiParagraphCursor, markdown_layout_source::folds::FoldPlan,
};

pub(super) struct VisibleParagraphs {
    plan: Option<FoldPlan>,
    #[cfg(feature = "s2-table-window-prototype")]
    tables: Option<crate::markdown_layout_source::table_rows::TablePlan>,
    base: BidiBaseDirection,
    budget: BidiMemoryBudget,
    cancellation: MarkdownBidiCancellation,
    resources: Arc<resource::ResourceState>,
    previous: Option<Box<MarkdownBidiParagraph>>,
    current: Option<Box<MarkdownBidiParagraph>>,
    failure: Option<prepare::Failure>,
}

/// Only the coordinator retains the in-flight Future/Gate. Sealed paragraphs contain none
/// of these !Send resources and retain their original cross-thread contract.
#[derive(Default)]
pub(crate) struct VisiblePreparation {
    cursor: Option<FrozenBidiParagraphCursor>,
    preparation: Option<MarkdownBidiPrepareJob>,
    target: Option<usize>,
}

/// Cache only this sealed paragraph's actual UBA-query progress; numeric state travels with its paragraph.
#[derive(Default)]
pub(super) struct WordOrder {
    next: u32,
    permitted: Option<bool>,
}

/// Word consumers pay for original metadata access; these facts grant no GCB, Source endpoint, or Native commit authority.
pub(crate) struct WordOrderStep {
    pub(crate) records: usize,
    pub(crate) metadata_units: usize,
    pub(crate) permitted: Option<bool>,
}

impl MarkdownBidiParagraph {
    /// CR/LF/TAB are valid word whitespace without a Markdown control-free certificate;
    /// the actual complete paragraph must nevertheless have zero levels throughout.
    pub(crate) fn prepare_word_order(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        range: Range<usize>,
        records: usize,
    ) -> Result<(WordOrderStep, usize), BidiError> {
        self.require_layout_source(authority)?;
        if records == 0 {
            return Err(BidiError::InvalidBudget);
        }
        if range.is_empty() || range.start < self.source.start || range.end > self.source.end {
            return Err(BidiError::InvalidRange);
        }
        let step = match &mut self.resolution {
            Resolution::VisibleParagraphs(visible) => {
                let paragraph = visible.character_mut(range.start)?;
                let through = range.end.min(paragraph.source.end);
                // CRLF is one original GCB but spans two B paragraphs. Certify only the
                // actual current paragraph; the same cursor prepares its successor later.
                paragraph.prepare_word_order(authority, range.start..through, records)?
            }
            Resolution::UniformZero(_) => (
                WordOrderStep {
                    records: 0,
                    metadata_units: 1,
                    permitted: Some(true),
                },
                range.end,
            ), // Original complete proof already establishes level zero despite Markdown controls such as line endings.
            Resolution::Paged(store) => (
                self.word_order.advance(store, self.units, records)?,
                range.end,
            ),
        };
        self.require_layout_source(authority)?; // Cached and file-backed results both retain Source, cancellation, and storage checks.
        Ok(step)
    }
}

impl WordOrder {
    /// Consume each original Text record once; one GCB's level zero cannot certify visual
    /// order before the complete paragraph has been checked.
    fn advance(
        &mut self,
        store: &mut Store,
        units: u32,
        limit: usize,
    ) -> Result<WordOrderStep, BidiError> {
        let mut records = 0;
        if self.permitted.is_none() {
            while self.next < units && records < limit {
                let text = Text::read(store, self.next)?;
                records += 1;
                self.next += 1;
                // Reuse the original UBA certificate's class rules, rejecting embedding
                // and isolate controls even if their resulting level happens to be zero.
                if text.level != 0 || !uniform::eligible_class(text.original) {
                    self.permitted = Some(false);
                    break;
                }
            }
            if self.next == units && self.permitted.is_none() {
                self.permitted = Some(true);
            } // Low surrogates are actual table records too; charge them without decoding or copying paragraph text again.
        }
        Ok(WordOrderStep {
            records,
            metadata_units: records.max(1),
            permitted: self.permitted,
        })
    }
}

impl VisibleParagraphs {
    /// Close only the display scope; actual levels still require the original paragraph preparation.
    pub(super) fn new(
        plan: FoldPlan,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: Arc<resource::ResourceState>,
    ) -> Self {
        Self::create(
            Some(plan),
            #[cfg(feature = "s2-table-window-prototype")]
            None,
            base,
            budget,
            cancellation,
            resources,
        )
    }

    /// Table scope retains actual paragraph preparation from construction. The shared
    /// directory publishes later real rows without granting levels inside hidden bodies.
    #[cfg(feature = "s2-table-window-prototype")]
    pub(super) fn new_tables(
        tables: crate::markdown_layout_source::table_rows::TablePlan,
        folds: Option<FoldPlan>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: Arc<resource::ResourceState>,
    ) -> Self {
        Self::create(folds, Some(tables), base, budget, cancellation, resources)
    }

    /// Keep one original ownership graph for resources and two adjacent paragraphs;
    /// folds and tables change only query authority in their actual scopes.
    fn create(
        plan: Option<FoldPlan>,
        #[cfg(feature = "s2-table-window-prototype")] tables: Option<
            crate::markdown_layout_source::table_rows::TablePlan,
        >,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: Arc<resource::ResourceState>,
    ) -> Self {
        Self {
            plan,
            #[cfg(feature = "s2-table-window-prototype")]
            tables,
            base,
            budget,
            cancellation,
            resources,
            previous: None,
            current: None,
            failure: None,
        }
    }

    /// Original cancellation, a real child Store failure, or recorded close failure
    /// invalidates successful use of the display analysis scope.
    pub(super) fn healthy(&self) -> Result<(), BidiError> {
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(tables) = &self.tables {
            tables.health()?;
        }
        if let Some(failure) = self.failure {
            return Err(failure.error());
        }
        if let Some(errno) = self.resources.observe().snapshot().close_errno {
            return Err(BidiError::Storage(std::io::Error::from_raw_os_error(errno)));
        }
        for paragraph in [&self.previous, &self.current].into_iter().flatten() {
            match paragraph.resolution.healthy() {
                Ok(()) | Err(BidiError::Cancelled) => {}
                Err(error) => return Err(error),
            }
        } // Both paragraphs share cancellation; return actual storage failure before that weaker shared outcome.
        self.cancellation.check()
    }

    /// Retain the stable cause of an actual in-flight failure. Out-of-domain queries
    /// do not revoke other completed visible paragraphs.
    pub(super) fn remember_failure(&mut self, error: &BidiError) {
        if self.failure.is_none()
            && !matches!(
                error,
                BidiError::FoldedRange | BidiError::UnpreparedRange | BidiError::TableRange
            )
        {
            self.failure = Some(prepare::Failure::capture(error));
        }
    }

    /// Hidden regions retain their text but grant no visible level/run authority;
    /// the opening CR position keeps its original paragraph-control role.
    pub(super) fn require_range(&self, range: &Range<usize>) -> Result<(), BidiError> {
        if range.start > range.end {
            return Err(BidiError::InvalidRange);
        }
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(tables) = &self.tables
            && tables.intersects(range)?
        {
            return Err(BidiError::TableRange);
        }
        let hidden = if range.is_empty() {
            self.plan
                .as_ref()
                .is_some_and(|plan| plan.hides(range.start, true))
        } else {
            self.plan.as_ref().is_some_and(|plan| {
                plan.spans()
                    .iter()
                    .any(|span| range.start < span.end.utf16 && span.start.utf16 + 1 < range.end)
            })
        };
        if hidden {
            return Err(BidiError::FoldedRange);
        }
        Ok(())
    }

    /// Delegate only to an actual result covering the complete request; text outside
    /// the two retained slots does not default to level zero.
    pub(super) fn range(&self, range: &Range<usize>) -> Result<&MarkdownBidiParagraph, BidiError> {
        self.require_range(range)?;
        [&self.current, &self.previous]
            .into_iter()
            .flatten()
            .find(|paragraph| {
                paragraph.source.start <= range.start && range.end <= paragraph.source.end
            })
            .map(Box::as_ref)
            .ok_or(BidiError::UnpreparedRange)
    }

    /// Queries borrow only the two existing results, without starting hidden analysis
    /// during synchronous drawing or run traversal.
    pub(super) fn range_mut(
        &mut self,
        range: &Range<usize>,
    ) -> Result<&mut MarkdownBidiParagraph, BidiError> {
        self.require_range(range)?;
        [&mut self.current, &mut self.previous]
            .into_iter()
            .flatten()
            .find(|paragraph| {
                paragraph.source.start <= range.start && range.end <= paragraph.source.end
            })
            .map(Box::as_mut)
            .ok_or(BidiError::UnpreparedRange)
    }

    /// Character lookup uses a half-open paragraph range, so the previous paragraph's
    /// endpoint cannot supply the next paragraph's level or base direction.
    pub(super) fn character_mut(
        &mut self,
        position: usize,
    ) -> Result<&mut MarkdownBidiParagraph, BidiError> {
        self.require_range(&(position..position))?;
        [&mut self.current, &mut self.previous]
            .into_iter()
            .flatten()
            .find(|paragraph| paragraph.source.contains(&position))
            .map(Box::as_mut)
            .ok_or(BidiError::UnpreparedRange)
    }

    /// Original endpoint discovery and the UBA child share one quantum; incomplete
    /// paragraphs publish no partial levels.
    pub(super) fn step(
        &mut self,
        source: &MarkdownLayoutTextSource,
        snapshot: &MarkdownTextSnapshot,
        authority: &MarkdownLayoutTextAuthority,
        position: usize,
        budget: MarkdownBidiPrepareBudget,
        work: &mut VisiblePreparation,
    ) -> Result<uniform::ParagraphPrepareStep, BidiError> {
        self.require_range(&(position..position))?;
        if let Some(job) = &work.preparation {
            job.require_healthy()?;
        } // A cached older paragraph cannot bypass the same item's actual in-flight failure.
        if let Some(paragraph) = [&mut self.current, &mut self.previous]
            .into_iter()
            .flatten()
            .find(|paragraph| paragraph.source.contains(&position))
        {
            // Complete UBA does not by itself close body-control endpoints. Uniform
            // continues its sole PreparedParagraph cursor, retaining real trailing
            // controls; a Paged result returns through the original zero-work entry.
            return paragraph.prepare_uniform_paragraph_range(authority, position, budget);
        }
        // A changed target retires the actual old preparation and projection before
        // checking cleanup health and starting the new original-source cursor.
        if work.target != Some(position) {
            drop(work.preparation.take());
            drop(work.cursor.take());
            self.healthy()?;
            work.target = Some(position);
            work.cursor = Some(snapshot.begin_bidi_paragraph(position)?);
        }
        let mut progress = uniform::ParagraphPrepareStep::ready();
        progress.ready = false;
        if let Some(cursor) = &mut work.cursor {
            let found = snapshot.advance_bidi_paragraph(
                cursor,
                budget.bytes,
                budget.scalars,
                budget.metadata_ops,
            )?;
            progress.bytes = found.bytes;
            progress.scalars = found.scalars;
            progress.metadata = found.metadata;
            progress.maximum_borrow = found.maximum_borrow;
            let Some(range) = found.range else {
                return Ok(progress);
            };
            let bytes = found.byte_range.ok_or(BidiError::CorruptMetadata)?;
            // Retire the oldest result before starting a third paragraph. The current
            // result remains available for original hard-break lookahead and publication.
            if let Some(previous) = self.previous.take() {
                drop(previous);
                self.healthy()?;
            }
            work.preparation = Some(MarkdownBidiPrepareJob::new_layout_paragraph_observed(
                source.clone(),
                authority,
                (range, bytes),
                self.base,
                self.budget,
                &self.cancellation,
                Arc::clone(&self.resources),
            )?);
            drop(work.cursor.take());
        }
        // Endpoint discovery has already consumed its share of each independent work
        // dimension; the UBA child receives only the remainder of the original quantum.
        let remaining = MarkdownBidiPrepareBudget {
            bytes: budget.bytes - progress.bytes,
            scalars: budget.scalars - progress.scalars,
            metadata_ops: budget.metadata_ops - progress.metadata,
        };
        if remaining.bytes == 0 || remaining.scalars == 0 || remaining.metadata_ops == 0 {
            return Ok(progress);
        }
        let preparation = work
            .preparation
            .as_mut()
            .ok_or(BidiError::CorruptMetadata)?;
        let actual = preparation.step(authority, remaining)?;
        progress.bytes += actual.bytes;
        progress.scalars += actual.scalars;
        progress.metadata += actual.metadata_ops;
        progress.maximum_borrow = progress.maximum_borrow.max(actual.source_borrow_max_bytes);
        if actual.status == MarkdownBidiPrepareStatus::Ready {
            let paragraph = preparation
                .take_ready(authority)?
                .ok_or(BidiError::CorruptMetadata)?;
            self.previous = self.current.take();
            self.current = Some(Box::new(paragraph));
            drop(work.preparation.take());
            work.target = None;
            let remaining = MarkdownBidiPrepareBudget {
                bytes: remaining.bytes - actual.bytes,
                scalars: remaining.scalars - actual.scalars,
                metadata_ops: remaining.metadata_ops - actual.metadata_ops,
            };
            if remaining.bytes != 0 && remaining.scalars != 0 && remaining.metadata_ops != 0 {
                // First Ready still enters the same paragraph-endpoint preparation; a
                // zero-work Future cannot skip it. Charge actual UBA work first and pass
                // only the remaining original quantum to endpoint discovery.
                let prepared = self
                    .current
                    .as_mut()
                    .expect("原Ready已移交当前完整段")
                    .prepare_uniform_paragraph_range(authority, position, remaining)?;
                progress.bytes += prepared.bytes;
                progress.scalars += prepared.scalars;
                progress.metadata += prepared.metadata;
                progress.maximum_borrow = progress.maximum_borrow.max(prepared.maximum_borrow);
                progress.ready = prepared.ready;
            }
        }
        self.healthy()?;
        Ok(progress)
    }

    /// Aggregate actual files and pages from the two paragraphs without observers retaining Source or UBA results.
    pub(super) fn resource_observer(&self) -> MarkdownBidiResourceObserver {
        self.resources.observe()
    }

    /// Page counts include already retired results; current cache bytes come from the actual shared resource receipt.
    pub(super) fn storage(&self) -> (usize, u64) {
        (
            self.resources.observe().snapshot().cache_bytes,
            self.resources.page_loads(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MarkdownDocumentBuffer;

    /// Bound combined endpoint/rule work; independent UBA oracles cover only finite visible paragraphs.
    fn prepare(
        paragraph: &mut MarkdownBidiParagraph,
        source: &MarkdownLayoutTextSource,
        position: usize,
    ) -> usize {
        let budget = MarkdownBidiPrepareBudget {
            bytes: 7,
            scalars: 3,
            metadata_ops: 5,
        };
        let mut bytes = 0;
        let mut work = VisiblePreparation::default();
        for _ in 0..200_000 {
            let step = paragraph
                .prepare_layout_paragraph_range(&source.authority(), position, budget, &mut work)
                .unwrap();
            assert!(
                step.bytes <= budget.bytes
                    && step.scalars <= budget.scalars
                    && step.metadata <= budget.metadata_ops
            );
            bytes += step.bytes;
            if step.ready {
                return bytes;
            }
            assert!(step.bytes != 0 || step.scalars != 0 || step.metadata != 0);
        }
        panic!("实际可见 B 段没有在原预算内完成");
    }

    /// Compare actual complete original paragraphs without removing a hidden RTL suffix
    /// and thereby changing their P2/UBA direction result.
    fn assert_levels(actual: &mut MarkdownBidiParagraph, text: &str, start: usize) {
        let units: Vec<u16> = text.encode_utf16().collect();
        let oracle =
            unicode_bidi::utf16::BidiInfo::new_with_data_source(&UnicodeBidiData, &units, None);
        for (offset, level) in oracle.levels.iter().enumerate() {
            assert_eq!(
                actual.level_at_utf16(start + offset).unwrap(),
                level.number()
            );
        }
    }

    #[test]
    /// CR and LF each end a real B paragraph. Certify the shared GCB through two complete
    /// paragraph scopes rather than extending either paragraph's authority.
    fn word_order_proof_stops_at_each_actual_b_paragraph_inside_crlf() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("crlf-word.md");
        let text = "one two\r\n# H\r\nאבג\r\n# N\r\n";
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let start = "one two\r\n# H".len();
        let end = "one two\r\n# H\r\nאבג\r\n".encode_utf16().count();
        let plan = FoldPlan::new(&source, std::slice::from_ref(&(start..end))).unwrap();
        let cancel = MarkdownBidiCancellation::new();
        let (job, receipt) = MarkdownBidiPrepareJob::new_layout_source_folded_observed(
            source.clone(),
            &authority,
            plan,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &cancel,
        );
        let mut job = job.unwrap();
        assert_eq!(
            job.step(&authority, MarkdownBidiPrepareBudget::default())
                .unwrap()
                .status,
            MarkdownBidiPrepareStatus::Ready
        );
        let mut paragraph = job.take_ready(&authority).unwrap().unwrap();
        drop(job);
        for (position, expected_source, expected_through) in [(7, 0..8, 8), (8, 8..9, 9)] {
            prepare(&mut paragraph, &source, position);
            let Resolution::VisibleParagraphs(visible) = &mut paragraph.resolution else {
                panic!("隐藏 RTL 必须保留真实可见段职责")
            };
            assert_eq!(
                visible.character_mut(position).unwrap().source,
                expected_source
            );
            let mut permitted = None;
            for _ in 0..16 {
                let (step, through) = paragraph
                    .prepare_word_order(&authority, position..9, 1)
                    .unwrap();
                assert_eq!(through, expected_through);
                assert!(step.records <= 1 && step.metadata_units <= 1);
                if step.permitted.is_some() {
                    permitted = step.permitted;
                    break;
                }
            }
            assert_eq!(permitted, Some(true));
        }
        drop(paragraph);
        assert!(receipt.is_released());
    }

    #[test]
    /// Hidden RTL does not start whole-source UBA; two-result authority, retirement before
    /// a third paragraph, and actual page release must all hold.
    fn hidden_rtl_keeps_two_actual_paragraphs_and_releases_the_oldest_before_third() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("visible-bidi.md");
        let first = "标题 אב";
        let hidden = "אב\u{202b}123\u{202c}😀".repeat(32_768);
        let second = "中 אב 12\n";
        let third = "אב (tail)\n";
        let text = format!("{first}\n{hidden}\n{second}{third}");
        std::fs::write(&path, &text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let start = first.encode_utf16().count();
        let end = start + 1 + hidden.encode_utf16().count() + 1;
        let plan = FoldPlan::new(&source, std::slice::from_ref(&(start..end))).unwrap();
        let cancel = MarkdownBidiCancellation::new();
        let (job, receipt) = MarkdownBidiPrepareJob::new_layout_source_folded_observed(
            source.clone(),
            &authority,
            plan,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &cancel,
        );
        let mut job = job.unwrap();
        assert_eq!(
            job.step(&authority, MarkdownBidiPrepareBudget::default())
                .unwrap()
                .status,
            MarkdownBidiPrepareStatus::Ready
        );
        let mut paragraph = job.take_ready(&authority).unwrap().unwrap();
        drop(job);
        assert!(!paragraph.whole_source_uniform_zero(&authority).unwrap());
        assert!(matches!(
            paragraph.level_at_utf16(0),
            Err(BidiError::UnpreparedRange)
        ));
        assert!(matches!(
            paragraph.level_at_utf16(start + 1),
            Err(BidiError::FoldedRange)
        ));
        let first_bytes = prepare(&mut paragraph, &source, 0);
        assert_levels(&mut paragraph, first, 0);
        let second_bytes = prepare(&mut paragraph, &source, end);
        assert_levels(&mut paragraph, first, 0);
        assert_levels(&mut paragraph, second, end);
        let live = receipt.snapshot();
        assert!(live.file_created && live.file_open && live.cache_bytes == 131_072);
        paragraph = std::thread::spawn(move || {
            // Actually completed paragraphs retain the public cross-thread contract;
            // in-flight Gate/Future values never entered this output owner.
            assert_levels(&mut paragraph, second, end);
            paragraph
        })
        .join()
        .unwrap();
        let third_start = end + second.encode_utf16().count();
        let third_bytes = prepare(&mut paragraph, &source, third_start);
        assert_levels(&mut paragraph, second, end);
        assert_levels(&mut paragraph, third, third_start);
        assert!(matches!(
            paragraph.level_at_utf16(0),
            Err(BidiError::UnpreparedRange)
        ));
        assert!(first_bytes + second_bytes + third_bytes < 32 * 1024);
        assert!(receipt.snapshot().peak_cache_bytes <= 131_072);
        cancel.cancel();
        assert!(matches!(
            paragraph.level_at_utf16(end),
            Err(BidiError::Cancelled)
        ));
        drop(paragraph);
        assert!(receipt.is_released());
        let closed = receipt.snapshot();
        assert!(closed.close_succeeded && !closed.file_open && closed.cache_bytes == 0);
    }
}
