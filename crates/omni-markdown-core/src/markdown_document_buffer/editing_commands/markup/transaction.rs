//! Finite markup edit plans, source-streamed ordered renumbering and atomic canonical output.

use super::*;

/// Replacement bytes are generated from counts/source context rather than retained text mirrors.
#[derive(Clone, Debug)]
pub(super) enum Payload {
    Empty,
    Spaces(usize),
    Number(u64),
    Marker {
        index: usize,
        add: i64,
    },
    BlankOuterAndBreak,
    ContinuedNewline {
        continued: bool,
        preserve_prefix: bool,
        loose: bool,
    },
    PlainNewline {
        indent: usize,
        explode: bool,
    },
}

/// At most two primary source edits are needed for a single caret, including a two-line quote exit.
#[derive(Clone, Debug)]
pub(super) struct Edit {
    pub(super) range: Range<usize>,
    pub(super) payload: Payload,
    bytes: Option<Range<usize>>,
}

impl Edit {
    /// Sequential sibling edits already own exact ASCII byte ranges; primary edits convert only once.
    fn byte_range(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Range<usize>, MarkdownDocumentBufferError> {
        if let Some(bytes) = &self.bytes {
            // Parsed list markers contain only ASCII, so their native and byte widths agree exactly.
            Ok(bytes.clone())
        } else {
            // A native primary selection may touch Unicode body text and requires canonical conversion.
            Ok(document.byte_offset_for_utf16(self.range.start)?
                ..document.byte_offset_for_utf16(self.range.end)?)
        }
    }
}

/// Ordered sibling traversal is regenerated from source, without retaining a per-item change vector.
#[derive(Clone)]
pub(super) struct Renumber {
    node: usize,
    kind: Kind,
    index: usize,
    previous: Option<u64>,
    offset: i64,
    stopped: bool,
}

/// One command owns only its active context path, two primary edits and up to two renumber selectors.
pub(super) struct Plan {
    pub(super) contexts: Vec<Context>,
    pub(super) separator: &'static str,
    pub(super) edits: Vec<Edit>,
    rules: Vec<Renumber>,
    prefix_columns: Vec<usize>,
    pub(super) after: usize,
}

impl Plan {
    /// Cache only per-active-ancestor prefix columns; no source text or suffix edit list is retained.
    pub(super) fn new(
        contexts: Vec<Context>,
        line: SourceLine,
        separator: &'static str,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        let mut prefix_columns = Vec::with_capacity(contexts.len());
        let mut cursor = line.cursor(document, 0)?;
        let mut columns = 0;
        for context in &contexts {
            // Ordered active offsets share one scalar pass instead of rescanning a deep prefix per ancestor.
            while let Some((scalar, _, unit)) = cursor
                .peek(document)?
                .filter(|value| value.2 - line.units.0 < context.from)
            {
                // Tabs advance visual columns; other whole scalars occupy one counted editor column.
                cursor.next(document)?;
                columns += if scalar == '\t' {
                    // A tab advances to the next four-column editor stop.
                    4 - columns % 4
                } else {
                    // Every other complete scalar contributes one visual column.
                    1
                };
                let _ = unit;
            }
            prefix_columns.push(columns);
        }
        Ok(Self {
            contexts,
            separator,
            edits: Vec::with_capacity(2),
            rules: Vec::with_capacity(2),
            prefix_columns,
            after: 0,
        })
    }

    /// Store one source-relative primary operation; ordered sibling edits remain an iterator contract.
    pub(super) fn add(&mut self, range: Range<usize>, payload: Payload) {
        self.edits.push(Edit {
            range,
            payload,
            bytes: None,
        });
    }

    /// Register a decimal sibling adjustment only when the selected container is ordered.
    pub(super) fn add_renumber(&mut self, index: usize, offset: i64) {
        let context = &self.contexts[index];
        if matches!(context.kind, Kind::Ordered(_)) {
            // One inner exit and one outer advance are the maximum simultaneous ordered-list obligations.
            self.rules.push(Renumber {
                node: context.node_start,
                kind: context.kind,
                index: context.item_index,
                previous: None,
                offset,
                stopped: false,
            });
        }
    }

    /// Count ASCII/native replacement width through the same generator used by canonical output.
    pub(super) fn payload_units(
        &self,
        payload: &Payload,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        let mut length = 0_usize;
        self.render(payload, |bytes| {
            // Every generated marker, decimal digit and configured CR/LF separator is ASCII.
            length = length
                .checked_add(bytes.len())
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
            Ok(())
        })?;
        Ok(length)
    }

    /// Generate one complete replacement with fixed spacing scratch and bounded decimal strings.
    fn render(
        &self,
        payload: &Payload,
        mut emit: impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        match payload {
            Payload::Empty => {
                // Deletion allocates no synthetic empty canonical piece.
            }
            Payload::Spaces(count) => {
                // Blank marker/indentation output uses the shared fixed-size spacing emitter.
                spaces(*count, &mut emit)?;
            }
            Payload::Number(number) => {
                // u64 formatting is bounded independently of source-line/document length.
                emit(number.to_string().as_bytes())?;
            }
            Payload::Marker { index, add } => {
                // Nested exit reuses only the chosen parent's source-derived marker spelling/counts.
                marker(&self.contexts[*index], *add, &mut emit)?;
            }
            Payload::BlankOuterAndBreak => {
                // Tight second-item expansion places a container-aware blank before the existing source line.
                self.prefix(false, true, &mut emit)?;
                emit(self.separator.as_bytes())?;
            }
            Payload::ContinuedNewline {
                continued,
                preserve_prefix,
                loose,
            } => {
                // The leading break always follows the old line after its trailing whitespace is removed.
                emit(self.separator.as_bytes())?;
                if *loose {
                    // A loose list inserts one additional ancestor-aware blank before its next item/body line.
                    self.prefix(false, true, &mut emit)?;
                    emit(self.separator.as_bytes())?;
                }
                if *preserve_prefix {
                    // Continued items receive blank indentation; a marker line receives its next sibling marker.
                    self.prefix(!continued, false, &mut emit)?;
                }
            }
            Payload::PlainNewline { indent, explode } => {
                // Default Enter preserves visible indentation using the configured physical separator.
                emit(self.separator.as_bytes())?;
                spaces(*indent, &mut emit)?;
                if *explode {
                    // An adjacent closing bracket remains on a second line with the original indentation.
                    emit(self.separator.as_bytes())?;
                    spaces(*indent, &mut emit)?;
                }
            }
        }
        Ok(())
    }

    /// Emit blank ancestors and an optional next marker using the baseline relative-column contract.
    fn prefix(
        &self,
        last_marker: bool,
        outer_only: bool,
        emit: &mut impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        let length = self.contexts.len().saturating_sub(usize::from(outer_only));
        let mut written = 0;
        for index in 0..length {
            // All source context remains descriptors; generated bytes stream directly into the caller's sink.
            let context = &self.contexts[index];
            if last_marker && index + 1 == length {
                // Only the innermost noncontinued item repeats a marker rather than blank indentation.
                marker(context, 1, emit)?;
                written += context.marker_units(1);
                continue;
            }
            let base = context.before + usize::from(context.kind == Kind::Quote);
            spaces(context.before, emit)?;
            if context.kind == Kind::Quote {
                // Blank quote ancestors preserve a visible quote marker instead of all-space indentation.
                emit(b">")?;
            }
            let target = if index + 1 < length {
                // A following ancestor starts at the current physical line's tab-expanded source column.
                self.prefix_columns[index + 1].saturating_sub(written)
            } else if outer_only {
                // The last blank-line ancestor omits its trailing marker padding, matching blankLine().
                context.to - context.from - context.after
            } else {
                // The innermost continued item retains its complete original content indentation.
                context.to - context.from
            };
            let fill = target.saturating_sub(base);
            spaces(fill, emit)?;
            written += base + fill;
        }
        Ok(())
    }
}

/// Emit arbitrary normalized spacing with one fixed scratch array, never a giant temporary String.
fn spaces(
    mut count: usize,
    emit: &mut impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
) -> Result<(), MarkdownDocumentBufferError> {
    const SPACING: [u8; ORIGINAL_PIECE_BYTES] = [b' '; ORIGINAL_PIECE_BYTES];
    while count > 0 {
        // Every output chunk fits the canonical builder and contains only complete ASCII scalars.
        let length = count.min(SPACING.len());
        emit(&SPACING[..length])?;
        count -= length;
    }
    Ok(())
}

/// Generate source-derived marker spelling, resetting checkboxes and advancing ordered decimals.
fn marker(
    context: &Context,
    add: i64,
    emit: &mut impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
) -> Result<(), MarkdownDocumentBufferError> {
    spaces(context.before, emit)?;
    match context.kind {
        Kind::Quote => {
            // A quote marker has no sibling number or list-item checkbox state.
            emit(b">")?;
        }
        Kind::Bullet(punctuation) => {
            // Literal bullet spelling is preserved even when a checked source item becomes unchecked.
            emit(&[punctuation])?;
            if context.task > 0 {
                // Retain the exact pre-checkbox padding but reset x/X to the editable unchecked spelling.
                spaces(context.task - 3, emit)?;
                emit(b"[ ]")?;
            }
        }
        Kind::Ordered(punctuation) => {
            // Ordered markers grow across decimal-width boundaries without changing their punctuation kind.
            let number = (context.number as i64 + add).max(0) as u64;
            emit(number.to_string().as_bytes())?;
            emit(&[punctuation])?;
        }
    }
    spaces(context.after, emit)
}

/// Walk finite primary changes plus source-regenerated ordered sibling replacements in source order.
fn each_edit<E: From<MarkdownDocumentBufferError>>(
    document: &mut MarkdownDocumentBuffer,
    plan: &Plan,
    mut visit: impl FnMut(&mut MarkdownDocumentBuffer, Edit) -> Result<(), E>,
) -> Result<(), E> {
    let mut primary = 0;
    if plan.rules.is_empty() {
        // Most Enter/Backspace commands need no suffix scan or sibling metadata at all.
        for edit in &plan.edits {
            // Finite primary operations are already in original source order and need no scanner ownership.
            visit(document, edit.clone())?;
        }
        return Ok(());
    }
    let mut rules = plan.rules.clone();
    let mut scanner = Scanner::new();
    let mut lines = SourceLines::new(document);
    while let Some(line) = lines.next(document)? {
        // Original piece descriptors retain no borrow across private canonical append allocation.
        let number = line.number;
        scanner.consume(document, line)?;
        for context in &scanner.contexts {
            // Only a marker physically introduced on this line is a candidate sibling item.
            if context.item_line != number {
                // Retained continuation ancestors do not represent newly encountered sibling marker bytes.
                continue;
            }
            for rule in &mut rules {
                // At most two source-based selectors inspect this physically introduced list item.
                if rule.stopped
                    || context.node_start != rule.node
                    || context.kind != rule.kind
                    || context.item_index < rule.index
                {
                    // Other lists, preceding items and completed nonconsecutive runs remain untouched.
                    continue;
                }
                if let Some(previous) = rule.previous {
                    // The chosen original item seeds the sequence; only subsequent consecutive siblings change.
                    if context.number != previous + 1 {
                        // The original command stops at the first intentionally nonconsecutive sibling number.
                        rule.stopped = true;
                        continue;
                    }
                    let start = line.units.0 + context.from + context.before;
                    let digits = context.to - context.from - context.before - 1 - context.after;
                    while primary < plan.edits.len() && plan.edits[primary].range.start <= start {
                        // A primary insertion/deletion sorts before every following renumbering at this source point.
                        visit(document, plan.edits[primary].clone())?;
                        primary += 1;
                    }
                    let number = (previous as i64 + 2 + rule.offset).max(0) as u64;
                    visit(
                        document,
                        Edit {
                            range: start..start + digits,
                            payload: Payload::Number(number),
                            bytes: Some(
                                line.bytes.0 + context.from + context.before
                                    ..line.bytes.0 + context.from + context.before + digits,
                            ),
                        },
                    )?;
                }
                rule.previous = Some(context.number);
            }
        }
    }
    while primary < plan.edits.len() {
        // No following matching sibling exists; the command still owns its finite primary source edit.
        visit(document, plan.edits[primary].clone())?;
        primary += 1;
    }
    Ok(())
}

#[cfg(test)]
// 保留已有失败golden的直接规划入口；不绕开新journal与append回滚。
pub(super) fn commit(
    document: &mut MarkdownDocumentBuffer,
    before: MarkdownTextSelection,
    plan: Plan,
) -> Result<MarkdownEditingOutcome, MarkdownDocumentBufferError> {
    commit_observed(document, before, plan, changes::NoopObserver)
        .map(|(outcome, ())| outcome)
        .map_err(changes::legacy_error)
}

/// Preflight complete geometry/budget, then publish one candidate root and one structural undo memento.
pub(super) fn commit_observed<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    before: MarkdownTextSelection,
    plan: Plan,
    observer: O,
) -> Result<(MarkdownEditingOutcome, O::Output), MarkdownObservedEditError<O::Error>> {
    let mut envelope: Option<Range<usize>> = None;
    let mut count = 0;
    let mut removed_units = 0;
    let mut inserted_units = 0;
    let mut removed_bytes = 0;
    let mut inserted_bytes = 0;
    each_edit(document, &plan, |document, edit| {
        // Planning borrows no source past this callback and does not allocate canonical edit storage.
        let bytes = edit.byte_range(document)?.len();
        let inserted = plan.payload_units(&edit.payload)?;
        count += 1;
        removed_units += edit.range.len();
        inserted_units += inserted;
        removed_bytes += bytes;
        inserted_bytes += inserted;
        if let Some(range) = &mut envelope {
            // Ordered iteration keeps one contiguous native notification without a per-item change list.
            range.end = edit.range.end;
        } else {
            // The first real source change establishes the one command's native notification origin.
            envelope = Some(edit.range);
        }
        Ok::<(), MarkdownDocumentBufferError>(())
    })?;
    let envelope = envelope.expect("markup command has a primary edit");
    let first_byte = document.byte_offset_for_utf16(envelope.start)?;
    let last_byte = document.byte_offset_for_utf16(envelope.end)?;
    let replacement_bytes = last_byte - first_byte - removed_bytes + inserted_bytes;
    let append = if count < 4096 {
        // Sparse commands preserve every original body piece and append generated markers only.
        inserted_bytes
    } else {
        // Dense ordered changes stream one new canonical envelope rather than creating unbounded tiny leaves.
        replacement_bytes
    };
    if document
        .edits
        .used_bytes
        .checked_add(append)
        .is_none_or(|total| total > document.maximum_document_bytes)
    {
        // A complete budget check precedes all append allocation and leaves text/history/selection unchanged.
        return Err(MarkdownDocumentBufferError::EditBufferLimitExceeded.into());
    }
    let result_bytes = document.byte_len() - removed_bytes + inserted_bytes;
    let metrics = MarkdownReplacementMetrics::new(
        envelope.start,
        envelope.len(),
        envelope.len() - removed_units + inserted_units,
    )?;
    let after = MarkdownTextSelection {
        anchor_utf16: plan.after,
        head_utf16: plan.after,
    };
    let (edit, output) = commit_command_observed(
        document,
        metrics,
        result_bytes,
        before,
        observer,
        |document, observer| {
            // Shared publication owns append rollback, future selection validation and the single history transition.
            if count < 4096 {
                // Ascending edits map original byte coordinates through prior byte deltas on an unpublished root.
                let mut root = document.root.clone();
                let mut removed = 0;
                let mut inserted = 0;
                each_edit(document, &plan, |document, edit| {
                    // Original root/indexes stay unchanged while replacement output consumes canonical append storage.
                    changes::emit(
                        document,
                        observer,
                        edit.range.clone(),
                        plan.payload_units(&edit.payload)?,
                    )?;
                    let bytes = edit.byte_range(document)?;
                    let (from, to) = (bytes.start, bytes.end);
                    let mut output = CanonicalOutput::new();
                    plan.render(&edit.payload, |bytes| output.push(document, bytes))?;
                    let replacement = output.finish(document)?;
                    let replacement_length = plan.payload_units(&edit.payload)?;
                    root = replace_tree_range(
                        document,
                        root.take(),
                        from - removed + inserted..to - removed + inserted,
                        replacement,
                    )?;
                    removed += to - from;
                    inserted += replacement_length;
                    Ok::<(), MarkdownObservedEditError<O::Error>>(())
                })?;
                Ok((root, after))
            } else {
                // Dense output copies old overlaps through fixed scratch and never borrows across append growth.
                let mut output = CanonicalOutput::new();
                let mut consumed = first_byte;
                each_edit(document, &plan, |document, edit| {
                    // Original byte coordinates stay valid while this unpublished output appends canonical segments.
                    changes::emit(
                        document,
                        observer,
                        edit.range.clone(),
                        plan.payload_units(&edit.payload)?,
                    )?;
                    let bytes = edit.byte_range(document)?;
                    let (from, to) = (bytes.start, bytes.end);
                    copy_source(document, &mut output, consumed..from)?;
                    plan.render(&edit.payload, |bytes| output.push(document, bytes))?;
                    consumed = to;
                    Ok::<(), MarkdownObservedEditError<O::Error>>(())
                })?;
                let replacement = output.finish(document)?;
                let root = replace_tree_range(
                    document,
                    document.root.clone(),
                    first_byte..last_byte,
                    replacement,
                )?;
                Ok((root, after))
            }
        },
    )?;
    Ok((MarkdownEditingOutcome::Changed(edit), output))
}
