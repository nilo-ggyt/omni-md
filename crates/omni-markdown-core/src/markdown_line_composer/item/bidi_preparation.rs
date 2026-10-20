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
        let memory = BidiMemoryBudget::new(64 * 1024)?;
        #[cfg(feature = "paged-shaping-frontier")]
        let (preparation, observer) =
            observed_preparation(payload, authority, cancellation, memory);
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let (preparation, observer) = MarkdownBidiPrepareJob::new_layout_source_observed(
            payload.source.clone(),
            authority,
            BidiBaseDirection::Auto,
            memory,
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

/// 相同真实装配只按明确显示域选择原 UBA 入口；没有表格权限的 Source 不走表格准备。
#[cfg(feature = "paged-shaping-frontier")]
fn observed_preparation(
    payload: &Payload,
    authority: &MarkdownLayoutTextAuthority,
    cancellation: &MarkdownLineComposeCancellation,
    memory: BidiMemoryBudget,
) -> (
    std::result::Result<MarkdownBidiPrepareJob, BidiError>,
    crate::MarkdownBidiResourceObserver,
) {
    #[cfg(feature = "s2-table-window-prototype")]
    if let Some(tables) = payload.tables.as_ref().map(|writer| writer.plan()) {
        return MarkdownBidiPrepareJob::new_layout_source_table_observed(
            payload.source.clone(),
            authority,
            tables,
            payload.folds.shared(),
            BidiBaseDirection::Auto,
            memory,
            &cancellation.bidi,
        );
    }
    if let Some(folds) = payload.folds.shared() {
        MarkdownBidiPrepareJob::new_layout_source_folded_observed(
            payload.source.clone(),
            authority,
            folds,
            BidiBaseDirection::Auto,
            memory,
            &cancellation.bidi,
        )
    } else {
        MarkdownBidiPrepareJob::new_layout_source_observed(
            payload.source.clone(),
            authority,
            BidiBaseDirection::Auto,
            memory,
            &cancellation.bidi,
        )
    }
}
