//! Transient active Markdown container ownership for one explicit keyboard command.
//!
//! Only the current nesting path, paragraph state and exact opaque-block identities survive each
//! scanned line. Source traversal, lexical marker grammar and foreign indentation are separate modules.

use super::foreign_blocks::{HtmlContext, html_close, html_start};
use super::markers::{
    ascii_spaces, fence_descriptor, line_interrupts, marker_at, marker_interrupts,
};
use super::*;

/// Container identity includes marker spelling because different list delimiters start a new list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    /// Quote spelling has no list sibling sequence.
    Quote,
    /// Preserve the exact bullet punctuation when advancing an item.
    Bullet(u8),
    /// Ordered delimiter identity distinguishes dot and parenthesized lists.
    Ordered(u8),
}

/// Active container metadata references source spelling rather than retaining a marker/body string.
#[derive(Clone, Debug)]
pub(super) struct Context {
    pub(super) kind: Kind,
    pub(super) from: usize,
    pub(super) to: usize,
    pub(super) before: usize,
    pub(super) after: usize,
    pub(super) number: u64,
    pub(super) task: usize,
    pub(super) item_line: usize,
    pub(super) node_start: usize,
    pub(super) item_index: usize,
    pub(super) loose: bool,
    pub(super) blank_seen: bool,
    pub(super) eligible: bool,
    pub(super) indent: usize,
}

impl Context {
    /// A quote has no list-item identity or sibling renumbering obligation.
    pub(super) fn is_list(&self) -> bool {
        self.kind != Kind::Quote
    }

    /// Generated task markers always reset an existing checked state to the baseline unchecked form.
    pub(super) fn marker_units(&self, add: i64) -> usize {
        let number = if matches!(self.kind, Kind::Ordered(_)) {
            // Incremented ordered markers can gain a digit at 9/99 boundaries without a source mirror.
            (self.number as i64 + add).max(0).to_string().len()
        } else {
            // Bullets and quotes have one punctuation unit and no decimal prefix.
            0
        };
        self.before + number + 1 + self.task + self.after
    }
}

/// A parsed marker owns only finite counts and positions, including arbitrary ASCII-space padding.
pub(super) struct Marker {
    pub(super) context: Context,
    pub(super) punctuation: usize,
    pub(super) punctuation_column: usize,
    /// Logical content boundary may lie inside the visual width of a physical tab.
    pub(super) to_column: usize,
    /// Actual scalar-aligned source endpoint after structural marker padding.
    pub(super) syntax_to: usize,
    /// Actual tab-expanded column at syntax_to, including any overflow beyond the logical boundary.
    pub(super) source_column: usize,
}

/// Scanner state keeps only the active container path and an exact opaque fence descriptor.
pub(super) struct Scanner {
    pub(super) contexts: Vec<Context>,
    paragraph: bool,
    fence: Option<(char, usize)>,
    html: Option<HtmlContext>,
    pub(super) current_html: Option<HtmlContext>,
    fence_scope: Option<(usize, usize)>,
    pub(super) opening_body_from: Option<usize>,
}

impl Scanner {
    /// A new explicit keyboard command starts without idle parser state or retained syntax ownership.
    pub(super) fn new() -> Self {
        Self {
            contexts: Vec::new(),
            paragraph: false,
            fence: None,
            html: None,
            current_html: None,
            fence_scope: None,
            opening_body_from: None,
        }
    }

    /// Classify one line, retaining only source-referenced active containers for the following line.
    pub(super) fn consume(
        &mut self,
        document: &MarkdownDocumentBuffer,
        line: SourceLine,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        self.opening_body_from = None;
        self.current_html = None;
        let mut offset = 0;
        let mut source_column = 0;
        let mut parent_column = 0;
        let mut retained = 0;
        // Three finite suffix boundaries amortize thematic rejection across arbitrary nested marker chains.
        let mut rejected_thematic = [0; 3];
        let mut pending = marker_at(
            document,
            line,
            offset,
            source_column,
            &mut rejected_thematic,
        )?;
        let mut spacing_cursor: Option<SourceCursor> = None;
        let blank = line.whitespace(document, false)?;
        for index in 0..self.contexts.len() {
            // Each ancestor consumes only its own explicit source prefix; tab overflow remains source metadata.
            let old = &self.contexts[index];
            let target = parent_column + old.indent;
            if old.kind == Kind::Quote {
                // A quote requires its own explicit prefix unless paragraph laziness later retains the old path.
                if let Some(marker) = pending
                    .as_ref()
                    .filter(|marker| marker.context.kind == Kind::Quote)
                {
                    // Existing quote nodes keep their original command spelling while current syntax advances source.
                    offset = marker.syntax_to;
                    source_column = marker.source_column;
                    parent_column = marker.to_column;
                    retained += 1;
                    spacing_cursor = None;
                    pending = marker_at(
                        document,
                        line,
                        offset,
                        source_column,
                        &mut rejected_thematic,
                    )?;
                    continue;
                }
            } else if let Some(marker) = pending.as_ref().filter(|marker| {
                // Only this delimiter identity at a shallower column can introduce the next sibling item.
                marker.context.kind == old.kind && marker.punctuation_column < target
            }) {
                // A same-level sibling inherits list identity but owns this source line's actual item geometry.
                let mut context = marker.context.clone();
                context.node_start = old.node_start;
                context.item_index = old.item_index + 1;
                context.loose = old.loose || (old.item_index == 0 && old.blank_seen);
                context.from = context.from.max(offset);
                context.before = marker.punctuation - context.from;
                context.eligible &= ascii_spaces(document, line, context.from, marker.punctuation)?;
                context.indent = marker.to_column - parent_column;
                self.contexts[index] = context;
                offset = marker.syntax_to;
                source_column = marker.source_column;
                parent_column = marker.to_column;
                retained += 1;
                pending = marker_at(
                    document,
                    line,
                    offset,
                    source_column,
                    &mut rejected_thematic,
                )?;
                // A new parent item releases every old child before recognizing its own explicit descendants.
                break;
            } else {
                // One ordered padding cursor is shared by all list ancestors, avoiding a quadratic deep scan.
                let cursor =
                    spacing_cursor.get_or_insert_with(|| line.ascii_cursor(document, offset));
                while source_column < target {
                    // Consume each actual padding scalar once, sharing overflow with deeper ancestor checks.
                    let Some((scalar, _, unit)) = cursor
                        .peek(document)?
                        .filter(|value| matches!(value.0, ' ' | '\t'))
                    else {
                        // Content before the required column cannot be an explicitly indented continuation.
                        break;
                    };
                    cursor.next(document)?;
                    offset = unit - line.units.0 + 1;
                    source_column += if scalar == '\t' {
                        // A tab advances to the next four-column stop, possibly crossing the target.
                        4 - source_column % 4
                    } else {
                        // Literal space advances exactly one visual column and one physical source unit.
                        1
                    };
                }
                if source_column >= target || blank {
                    // Logical content may begin within a tab, while source offsets advance only past whole units.
                    parent_column = target;
                    retained += 1;
                    continue;
                }
            }
            let pending_interrupts = if let Some(marker) = &pending {
                // Markdown only lets nonempty bullets and one-start ordered markers interrupt an existing paragraph.
                marker_interrupts(document, line, marker)?
            } else {
                // Ordinary prose has no explicit marker interrupting its lazy container continuation.
                false
            };
            if self.paragraph
                && !blank
                && !pending_interrupts
                && !line_interrupts(document, line, offset)?
            {
                // A lazy paragraph preserves the old ancestry but does not reinterpret marker-shaped prose as a child.
                retained = self.contexts.len();
                pending = None;
            }
            // A failed explicit ancestor boundary ends that ancestor and every deeper old context.
            break;
        }
        self.contexts.truncate(retained);
        let scope = self
            .contexts
            .last()
            .map(|context| (context.node_start, context.item_line));
        if self.fence.is_some() && self.fence_scope != scope {
            // A contained fence ends when its own list item/quote ends before an outside line.
            self.fence = None;
            self.fence_scope = None;
        }
        if self.html.is_some_and(|context| context.scope != scope) {
            // Foreign child content cannot retain a source container after its actual boundary.
            self.html = None;
        }
        if let Some(context) = self.html {
            // Known foreign source owns all inner marker spelling until an actual terminator.
            self.current_html = Some(context);
            if html_close(document, line, context.kind)?.is_some() {
                // Its closing line remains foreign while the next line can resume ordinary Markdown.
                self.html = None;
            }
            self.paragraph = false;
            return Ok(true);
        }
        if blank {
            // Only one Boolean per active list records a possible loose first/second-item separator.
            for context in &mut self.contexts {
                // The next matching sibling decides looseness; no blank-line body/index is retained.
                context.blank_seen = true;
            }
            self.paragraph = false;
            return Ok(self.fence.is_some());
        }
        if let Some((marker, length)) = self.fence {
            // Opaque fenced bodies cannot introduce nested list/quote contexts through their visible text.
            if fence_descriptor(document, line, offset)?
                .is_some_and(|value| value.0 == marker && value.1 >= length && value.2)
            {
                // Exact same-marker closing runs release only the existing fence after this source line.
                self.fence = None;
                self.fence_scope = None;
            }
            self.paragraph = false;
            return Ok(true);
        }
        if self.paragraph && self.contexts.is_empty() {
            // Paragraph interruption rules apply before assigning any new root list ownership.
            if let Some(marker) = &pending
                && !marker_interrupts(document, line, marker)?
            {
                // Empty bullet and non-one ordered spellings stay literal paragraph source.
                pending = None;
            }
        }
        while let Some(mut marker) = pending {
            // Only active nesting metadata is retained; code-indented source never creates a list context.
            if marker.punctuation_column.saturating_sub(parent_column) > 3 {
                // A code-indented marker-shaped suffix has no authority to add another active container.
                break;
            }
            if marker.context.kind != Kind::Quote {
                // Item offsets reflect actual native source, including padding tabs consumed by its ancestor.
                marker.context.from = marker.context.from.max(offset);
                marker.context.before = marker.punctuation - marker.context.from;
                marker.context.eligible &=
                    ascii_spaces(document, line, marker.context.from, marker.punctuation)?;
                marker.context.node_start = line.units.0 + marker.context.from;
            }
            marker.context.indent = marker.to_column - parent_column;
            offset = marker.syntax_to;
            source_column = marker.source_column;
            parent_column = marker.to_column;
            self.contexts.push(marker.context);
            pending = marker_at(
                document,
                line,
                offset,
                source_column,
                &mut rejected_thematic,
            )?;
        }
        if let Some((marker, length, _)) = fence_descriptor(document, line, offset)? {
            // An opening fence suppresses Markdown context after its exact body start, preserving left affinity.
            self.fence = Some((marker, length));
            self.fence_scope = self
                .contexts
                .last()
                .map(|context| (context.node_start, context.item_line));
            self.opening_body_from = Some(offset);
            self.paragraph = false;
            return Ok(true);
        }
        if let Some(kind) = html_start(document, line, offset)? {
            // Embedded HTML is foreign to the markup command, including its complete opening source line.
            let context = HtmlContext {
                kind,
                opening_line: line.number,
                scope: self
                    .contexts
                    .last()
                    .map(|context| (context.node_start, context.item_line)),
            };
            self.current_html = Some(context);
            self.opening_body_from = Some(offset);
            if html_close(document, line, kind)?.is_none() {
                // A same-line close grants no retained foreign-block state to following source.
                self.html = Some(context);
            }
            self.paragraph = false;
            return Ok(true);
        }
        self.paragraph = !line_interrupts(document, line, offset)?;
        Ok(false)
    }
}
