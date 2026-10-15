//! Bounded Markdown presentation cache and public viewport/table contracts.

//! A transient UTF-8 viewport becomes nonoverlapping absolute UTF-16 style runs.
//! No text body, HTML/AST, or AppKit object is retained; native adapters map runs
//! to attributes while the piece tree remains the sole authoritative text model.

mod parser;

use parser::*;

use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use thiserror::Error;

pub const MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS: usize = 8 * 1_024;
const MAX_MARKDOWN_PRESENTATION_DECORATIONS: usize = 8 * 1_024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// User-facing choice between semantic inline preview and literal source styling.
pub enum MarkdownPresentationMode {
    #[default]
    InlinePreview,
    Source,
}

impl MarkdownPresentationMode {
    /// Stable ordering used by the native popup control. Keeping the mapping in
    /// the domain type prevents AppKit and the controller from inventing
    /// different mode values at the boundary.
    /// Provide the stable native-popup ordering for presentation modes.
    pub const fn popup_index(self) -> usize {
        match self {
            // Inline preview occupies the first popup item.
            Self::InlinePreview => 0,
            // Literal source occupies the second popup item.
            Self::Source => 1,
        }
    }

    /// Converts the bounded popup index back to a domain mode. Unknown values
    /// are rejected instead of silently selecting a future mode.
    /// Reject unknown popup indices rather than silently selecting a future mode.
    pub const fn from_popup_index(index: usize) -> Option<Self> {
        match index {
            // The first item selects inline preview.
            0 => Some(Self::InlinePreview),
            // The second item selects literal source.
            1 => Some(Self::Source),
            // Unknown native values cannot identify a supported presentation mode.
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
/// Fixed-width composable semantic roles consumed by native attribute adapters.
pub struct MarkdownPresentationStyles(u32);

impl MarkdownPresentationStyles {
    pub const HEADING_1: Self = Self(1 << 0);
    pub const HEADING_2: Self = Self(1 << 1);
    pub const HEADING_3: Self = Self(1 << 2);
    pub const HEADING_4: Self = Self(1 << 3);
    pub const HEADING_5: Self = Self(1 << 4);
    pub const HEADING_6: Self = Self(1 << 5);
    pub const STRONG: Self = Self(1 << 6);
    pub const EMPHASIS: Self = Self(1 << 7);
    pub const STRIKETHROUGH: Self = Self(1 << 8);
    pub const INLINE_CODE: Self = Self(1 << 9);
    pub const LINK: Self = Self(1 << 10);
    pub const BLOCK_QUOTE: Self = Self(1 << 11);
    pub const UNORDERED_LIST: Self = Self(1 << 12);
    pub const ORDERED_LIST: Self = Self(1 << 13);
    pub const TASK_LIST: Self = Self(1 << 14);
    pub const HORIZONTAL_RULE: Self = Self(1 << 15);
    pub const FENCED_CODE: Self = Self(1 << 16);
    pub const TABLE: Self = Self(1 << 17);
    pub const FORMULA: Self = Self(1 << 18);
    pub const SUPERSCRIPT: Self = Self(1 << 19);
    pub const UNDERLINE: Self = Self(1 << 20);
    pub const MARKER: Self = Self(1 << 21);
    pub const HIDDEN_MARKER: Self = Self(1 << 22);
    pub const TABLE_HEADER: Self = Self(1 << 23);
    pub const TABLE_DELIMITER: Self = Self(1 << 24);
    pub const TABLE_BOUNDARY: Self = Self(1 << 25);
    pub const TABLE_ALIGN_LEFT: Self = Self(1 << 26);
    pub const TABLE_ALIGN_CENTER: Self = Self(1 << 27);
    pub const TABLE_ALIGN_RIGHT: Self = Self(1 << 28);
    /// A native table overlays this preview row; TextKit retains a fixed-height placeholder.
    pub const TABLE_ROW_PLACEHOLDER: Self = Self(1 << 29);

    #[must_use]
    /// Require all requested style bits to be present in this style set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    /// Combine independent presentation roles without changing either input set.
    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    /// Remove requested roles while preserving unrelated presentation bits.
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[must_use]
    /// Detect an unstyled source interval.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterate the fixed-width active style indices used by the transition sweep.
    fn set_bits(self) -> impl Iterator<Item = usize> {
        (0..u32::BITS as usize).filter(move |bit| {
            // Only active bits participate in the fixed-width style-transition sweep.
            self.0 & (1_u32 << bit) != 0
        })
    }

    /// Collapse nested style occurrence counts into the currently active style set.
    fn from_presence_counts(counts: &[u16; u32::BITS as usize]) -> Self {
        let mut bits = 0_u32;
        for (bit, count) in counts.iter().enumerate() {
            // Inspect the fixed 32-bit style domain; counts reflect nesting rather than source size.
            if *count != 0 {
                // A nonzero occurrence count keeps that style active even if an inner span has ended.
                bits |= 1_u32 << bit;
            }
        }
        Self(bits)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One disjoint styled interval in absolute document UTF-16 coordinates.
pub struct MarkdownPresentationRun {
    pub utf16_range: Range<usize>,
    pub styles: MarkdownPresentationStyles,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Exact styled or unstyled interval safe to reuse for one native attribute query.
pub struct MarkdownPresentationQuery {
    pub utf16_range: Range<usize>,
    pub styles: MarkdownPresentationStyles,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Platform-neutral horizontal cell alignment.
pub enum MarkdownTableAlignment {
    Left,
    Center,
    Right,
}

/// Immutable semantic prototype for one cell inside the current bounded viewport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownTableCellPrototype {
    pub utf16_range: Range<usize>,
    pub column_index: usize,
    pub alignment: MarkdownTableAlignment,
}

/// Row metadata indexes a flat cell array rather than allocating one vector per row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownTableRowPrototype {
    pub utf16_range: Range<usize>,
    pub cell_index_range: Range<usize>,
    pub is_header: bool,
}

/// Native-consumable table prototype without document text or platform ownership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownTablePrototype {
    pub utf16_range: Range<usize>,
    pub header_utf16_range: Range<usize>,
    pub delimiter_utf16_range: Range<usize>,
    pub rows: Vec<MarkdownTableRowPrototype>,
    pub cells: Vec<MarkdownTableCellPrototype>,
}

#[derive(Debug, Error, Eq, PartialEq)]
/// Bounded projection failures that require literal source presentation instead of partial semantics.
pub enum MarkdownViewportPresentationError {
    #[error("Markdown 视口范围与 UTF-16 文本长度不一致")]
    InvalidUtf16Window,
    #[error("Markdown parser 返回了无效的 UTF-8 边界")]
    InvalidParserBoundary,
    #[error("Markdown 视口语义超过有界 span 上限")]
    DecorationLimitExceeded,
}

/// Semantic runs restricted to the current native layout window.
///
/// Generation ties metadata to the piece-tree revision; edit/history/selection
/// changes invalidate it synchronously. Text slices exist only during rebuild.
pub struct MarkdownViewportPresentation {
    mode: MarkdownPresentationMode,
    generation: Option<u64>,
    active_selection: Range<usize>,
    utf16_window: Option<Range<usize>>,
    runs: Vec<MarkdownPresentationRun>,
    tables: Vec<MarkdownTablePrototype>,
}

impl Default for MarkdownViewportPresentation {
    /// Create an empty inline-preview cache without source text or native object ownership.
    fn default() -> Self {
        Self {
            mode: MarkdownPresentationMode::InlinePreview,
            generation: None,
            active_selection: 0..0,
            utf16_window: None,
            runs: Vec::new(),
            tables: Vec::new(),
        }
    }
}

impl MarkdownViewportPresentation {
    #[must_use]
    /// Read the current presentation preference without rebuilding semantic metadata.
    pub fn mode(&self) -> MarkdownPresentationMode {
        self.mode
    }

    /// Invalidate bounded attributes only when the presentation preference changes.
    pub fn set_mode(&mut self, mode: MarkdownPresentationMode) -> Option<Range<usize>> {
        if self.mode == mode {
            // Reselecting the active mode preserves the existing cache and needs no native invalidation.
            return None;
        }
        self.mode = mode;
        self.invalidate()
    }

    /// Normalize directional selections and invalidate selection-dependent syntax visibility on change.
    pub fn set_active_selection(&mut self, selection: Range<usize>) -> Option<Range<usize>> {
        let normalized = selection.start.min(selection.end)..selection.start.max(selection.end);
        if self.active_selection == normalized {
            // An unchanged normalized selection cannot change marker visibility.
            return None;
        }
        self.active_selection = normalized;
        self.invalidate()
    }

    #[must_use]
    /// Require a rebuild for a changed document generation or a position outside the cached window.
    pub fn needs_rebuild(&self, generation: u64, utf16_index: usize) -> bool {
        self.generation != Some(generation)
            || self.cached_utf16_window().is_none_or(|window| {
                // A coordinate outside cached half-open coverage requires fresh bounded presentation.
                utf16_index < window.start || utf16_index >= window.end
            })
    }

    /// Validate and atomically replace one bounded semantic cache after successful parsing.
    pub fn rebuild(
        &mut self,
        generation: u64,
        utf16_window: Range<usize>,
        viewport_text: &str,
    ) -> Result<(), MarkdownViewportPresentationError> {
        if utf16_window.start > utf16_window.end
            || viewport_text.encode_utf16().count() != utf16_window.end - utf16_window.start
        {
            // The supplied text must exactly represent the declared UTF-16 window before any cache mutation.
            return Err(MarkdownViewportPresentationError::InvalidUtf16Window);
        }

        let build = if self.mode == MarkdownPresentationMode::Source || viewport_text.is_empty() {
            // Source mode and empty text require no Markdown parser or semantic allocations.
            MarkdownPresentationBuild::default()
        } else {
            // Inline preview parses only the complete bounded source supplied by the document layer.
            build_markdown_presentation(viewport_text, utf16_window.start, &self.active_selection)?
        };
        // Publish only validated complete coverage; replacing vectors releases stale semantic ownership.
        self.generation = Some(generation);
        self.utf16_window = Some(utf16_window);
        self.runs = build.runs;
        self.tables = build.tables;
        Ok(())
    }

    /// Record the bounded source-mode attribute window used by native layout.
    ///
    /// Source mode needs no parser/body materialization. Retaining its last range
    /// allows a later preview switch to invalidate only that range, not all text.
    /// Record native source-mode coverage without parsing or retaining a document body.
    pub fn record_source_window(
        &mut self,
        generation: u64,
        utf16_window: Range<usize>,
    ) -> Result<(), MarkdownViewportPresentationError> {
        if self.mode != MarkdownPresentationMode::Source || utf16_window.start > utf16_window.end {
            // Only a valid source-mode window can be recorded through this path.
            return Err(MarkdownViewportPresentationError::InvalidUtf16Window);
        }
        // Publish only validated complete coverage; replacing vectors releases stale semantic ownership.
        self.generation = Some(generation);
        self.utf16_window = Some(utf16_window);
        self.runs.clear();
        self.tables.clear();
        Ok(())
    }

    /// Record bounded source fallback when a complete Markdown block exceeds its budget.
    /// Record explicit inline-preview fallback coverage when a complete block exceeds its parse budget.
    pub fn record_source_fallback_window(
        &mut self,
        generation: u64,
        utf16_window: Range<usize>,
    ) -> Result<(), MarkdownViewportPresentationError> {
        if self.mode != MarkdownPresentationMode::InlinePreview
            || utf16_window.start > utf16_window.end
        {
            // Fallback is meaningful only for a valid inline-preview window.
            return Err(MarkdownViewportPresentationError::InvalidUtf16Window);
        }
        // Publish only validated complete coverage; replacing vectors releases stale semantic ownership.
        self.generation = Some(generation);
        self.utf16_window = Some(utf16_window);
        self.runs.clear();
        self.tables.clear();
        Ok(())
    }

    /// Drop stale semantic runs after a generation change and return only the last
    /// bounded attribute range requiring invalidation, without materializing text.
    /// Drop stale semantics and return the previous bounded attribute interval for native invalidation.
    pub fn invalidate_document_generation(&mut self) -> Option<Range<usize>> {
        self.invalidate()
    }

    #[must_use]
    /// Find the containing styled run or exact unstyled gap inside a current cached window.
    pub fn query(&self, utf16_index: usize) -> Option<MarkdownPresentationQuery> {
        self.generation?;
        let utf16_window = self.utf16_window.as_ref()?;
        if utf16_index < utf16_window.start || utf16_index >= utf16_window.end {
            // Coordinates outside the cached half-open interval cannot use this generation's attributes.
            return None;
        }

        let candidate = self.runs.partition_point(|run| {
            // Skip runs ending before the queried position using the sorted disjoint-run invariant.
            run.utf16_range.end <= utf16_index
        });
        if let Some(run) = self.runs.get(candidate)
            && run.utf16_range.start <= utf16_index
        {
            // A candidate whose start precedes the requested position contains the desired styled interval.
            return Some(MarkdownPresentationQuery {
                utf16_range: run.utf16_range.clone(),
                styles: run.styles,
            });
        }

        // Unstyled gaps receive precise boundaries so native layout need not query every character.
        let gap_start = candidate
            .checked_sub(1)
            .and_then(|previous| {
                // An existing previous run bounds the beginning of the unstyled gap.
                self.runs.get(previous)
            })
            .map_or(utf16_window.start, |run| {
                // The previous styled interval ends where the unstyled gap begins.
                run.utf16_range.end
            });
        let gap_end = self.runs.get(candidate).map_or(utf16_window.end, |run| {
            // The following styled interval begins where the unstyled gap ends.
            run.utf16_range.start
        });
        Some(MarkdownPresentationQuery {
            utf16_range: gap_start..gap_end,
            styles: MarkdownPresentationStyles::default(),
        })
    }

    #[must_use]
    /// Expose cached coverage only while its generation remains valid.
    pub fn cached_utf16_window(&self) -> Option<Range<usize>> {
        self.generation.and_then(|_| {
            // Expose only the small owned coverage range after a valid cache generation has been established.
            self.utf16_window.as_ref().cloned()
        })
    }

    #[must_use]
    /// Borrow current immutable table metadata without transferring source or native ownership.
    pub fn tables(&self) -> &[MarkdownTablePrototype] {
        &self.tables
    }

    /// Borrow current bounded style runs for transient native visible-cell rendering.
    #[must_use]
    /// Borrow current bounded runs for transient native visible-cell rendering.
    pub fn runs(&self) -> &[MarkdownPresentationRun] {
        &self.runs
    }

    /// Release semantic vectors while retaining the last bounded window for repeated native invalidations.
    fn invalidate(&mut self) -> Option<Range<usize>> {
        // Keep the last window through rapid selection/mode invalidations even
        // after generation is cleared; query rejects stale runs using that generation.
        let invalidated = self.utf16_window.as_ref().cloned();
        self.generation = None;
        self.runs.clear();
        self.tables.clear();
        invalidated
    }
}

#[cfg(test)]
mod tests;
