//! Compose multiple lines from one frozen source. Completed input items and
//! completed visual lines are separate publication boundaries.
//!
//! Itemization intersects actual bidi, script, grapheme, and style authority;
//! an item end does not imply a soft line end. Composition consumes the original
//! measure, safe-slice, and resolver results without introducing native dependencies.

mod controls;
mod font;
pub use controls::{
    MarkdownComposedLinePartKind, MarkdownLineControlResponse, MarkdownLineParagraphStyle,
    MarkdownLineTabAlignment, MarkdownLineTabPlacement, MarkdownLineTabStop,
};
mod folds;
mod item;

#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use item::closed_item::{ClosedItem, ClosedShapingInput};
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use item::{CertifiedShapingEndpoint, ResolvedBidiShapingSeam};

#[cfg(feature = "paged-shaping-frontier")]
pub use item::{MarkdownLineFrontierReason, MarkdownLineFrontierWork};

mod engine;
mod store;
pub(crate) mod worker_resources;
pub use engine::{MarkdownLineComposeAnswer, MarkdownLineComposeStep, MarkdownLineComposer};
#[cfg(feature = "paged-shaping-frontier")]
pub use engine::{
    MarkdownWordDeleteAnswer, MarkdownWordDeleteDirection, MarkdownWordDeleteStep,
    MarkdownWordGroupAction, MarkdownWordGroupAnswer, MarkdownWordGroupStep,
    MarkdownWordMovePreparation, MarkdownWordMoveScope,
};
pub use store::output::{
    MarkdownComposedCaretAssociation, MarkdownComposedCaretSide,
    MarkdownComposedGraphemeOrdinalAnswer, MarkdownComposedLine, MarkdownComposedLineAffinity,
    MarkdownComposedLineFacts, MarkdownComposedLineKind, MarkdownComposedLineLookup,
    MarkdownComposedLineLookupAnswer, MarkdownComposedLineLookupStep,
    MarkdownComposedLineLookupTarget, MarkdownComposedLinePart, MarkdownComposedLinePartFacts,
    MarkdownComposedLineResourceObserver, MarkdownComposedLineResources,
    MarkdownComposedLineTrailingWhitespace, MarkdownComposedLineTrailingWhitespaceStep,
    MarkdownComposedLines, MarkdownComposedVisualAnswer, MarkdownComposedVisualGraphemeAnswer,
    MarkdownComposedVisualGraphemeQuery, MarkdownComposedVisualGraphemeStep,
    MarkdownComposedVisualQuery, MarkdownComposedVisualStep,
};
#[cfg(feature = "paged-shaping-frontier")]
pub use store::output::{
    MarkdownComposedVisualWordAnswer, MarkdownComposedVisualWordQuery,
    MarkdownComposedVisualWordStep,
};
pub use worker_resources::{
    MarkdownLineComposerClusterResources, MarkdownLineComposerResolveResources,
    MarkdownLineComposerShapeResources, MarkdownLineComposerWorkerResourceObserver,
    MarkdownLineComposerWorkerResources,
};

pub use font::{
    MARKDOWN_LINE_FONT_SLOTS, MarkdownLineFontCatalog, MarkdownLineFontRegistration,
    MarkdownLineRegisteredFont,
};

pub use item::{
    MarkdownLineComposeBudget, MarkdownLineComposeCancellation, MarkdownLineComposeError,
    MarkdownLineComposeStyle, MarkdownLineFontProbe, MarkdownLineFontProbeSegmentAnswer,
    MarkdownLineFontProbeSegmentStep, MarkdownLineInlineControl, MarkdownLineInlineControlKind,
    MarkdownLineItem, MarkdownLineItemAnswer, MarkdownLineItemCoordinator, MarkdownLineItemFacts,
    MarkdownLineItemStep, MarkdownLineStyleRequest, MarkdownLineStyleResponse,
};

#[cfg(test)]
mod tests;

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
pub use engine::{MarkdownLineTableRowRequest, MarkdownLineTableRowResponse};

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
pub use worker_resources::MarkdownLineComposerTableResources;
