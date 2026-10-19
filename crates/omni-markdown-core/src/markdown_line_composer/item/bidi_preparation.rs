//! 原Coordinator唯一全源UBA准备；布局阶段与按词移动共享同一owner、取消位和量子。

use super::*;

pub(super) fn advance(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    cancellation: &MarkdownLineComposeCancellation,
    budget: MarkdownLineComposeBudget,
    step: &mut MarkdownLineItemStep,
) -> Result<()> {
    if payload.bidi_preparation.is_none() {
        // 先登记真实owner再允许匿名FD/页分配；失败同样保留原资源收据。
        payload.resources.begin_bidi();
        let (preparation, observer) = MarkdownBidiPrepareJob::new_layout_source_observed(
            payload.source.clone(),
            authority,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(64 * 1024)?,
            &cancellation.bidi,
        );
        payload.resources.register_bidi(observer);
        payload.bidi_preparation = Some(preparation?);
    }
    let preparation = payload
        .bidi_preparation
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    let progress = preparation.step(
        authority,
        MarkdownBidiPrepareBudget {
            bytes: budget.bytes.min(4096),
            scalars: budget.scalars,
            metadata_ops: budget.metadata_units.min(METADATA_UNITS),
        },
    )?;
    step.bytes = progress.bytes;
    step.scalars = progress.scalars;
    step.metadata_units = progress.metadata_ops;
    if progress.status == MarkdownBidiPrepareStatus::Ready {
        // 真EOF、完整规则和真实flush完成才移交原Paragraph；Pending不授level权限。
        payload.bidi = Some(
            preparation
                .take_ready(authority)?
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
        );
        payload.bidi_preparation.take();
    }
    Ok(())
}
