//! 同一冻结源的多行组合管线；输入分项和最终视觉行是两个不同的完成边界。
//!
//! 分项阶段只封闭真实UBA、Script、GCB与样式响应的交集，不把item末尾当软行。
//! 后继组合阶段继续消费原Measure、受控安全切片和实际Resolver结果；没有Native依赖。

mod controls;
mod font;
pub use controls::{
    MarkdownComposedLinePartKind, MarkdownLineControlResponse, MarkdownLineParagraphStyle,
    MarkdownLineTabAlignment, MarkdownLineTabPlacement, MarkdownLineTabStop,
};
mod item;

#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use item::closed_item::ClosedItem;

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
    MarkdownWordMovePreparation,
};
pub use store::output::{
    MarkdownComposedCaretAssociation, MarkdownComposedCaretSide,
    MarkdownComposedGraphemeOrdinalAnswer, MarkdownComposedLine, MarkdownComposedLineAffinity,
    MarkdownComposedLineFacts, MarkdownComposedLineKind, MarkdownComposedLineLookup,
    MarkdownComposedLineLookupAnswer, MarkdownComposedLineLookupStep,
    MarkdownComposedLineLookupTarget, MarkdownComposedLinePart, MarkdownComposedLinePartFacts,
    MarkdownComposedLineResourceObserver, MarkdownComposedLineResources, MarkdownComposedLines,
    MarkdownComposedVisualAnswer, MarkdownComposedVisualGraphemeAnswer,
    MarkdownComposedVisualGraphemeQuery, MarkdownComposedVisualGraphemeStep,
    MarkdownComposedVisualQuery, MarkdownComposedVisualStep,
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
    MarkdownLineComposeStyle, MarkdownLineFontProbe, MarkdownLineInlineControl,
    MarkdownLineInlineControlKind, MarkdownLineItem, MarkdownLineItemAnswer,
    MarkdownLineItemCoordinator, MarkdownLineItemFacts, MarkdownLineItemStep,
    MarkdownLineStyleRequest, MarkdownLineStyleResponse,
};

#[cfg(test)]
mod tests;
