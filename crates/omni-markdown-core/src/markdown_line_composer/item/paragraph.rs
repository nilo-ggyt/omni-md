//! 原Item起点只准备正在消费的真实B段；依赖与段端点查询共用原量子，不复制正文或段目录。

use super::*;

pub(super) fn prepare(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    step: &mut MarkdownLineItemStep,
) -> Result<bool> {
    let bidi = payload
        .bidi
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    if !bidi.uses_prepared_paragraphs() {
        return Ok(true); // 原单段/Paged路径没有端点查询工作；沿外层原Source/取消guard继续，不重复授权。
    }
    let progress = bidi.prepare_uniform_paragraph_range(
        authority,
        payload.cursor,
        MarkdownBidiPrepareBudget {
            bytes: budget.bytes.min(4096),
            scalars: budget.scalars.min(256),
            metadata_ops: budget.metadata_units.min(256),
        },
    )?;
    step.bytes = progress.bytes;
    step.scalars = progress.scalars;
    step.metadata_units = progress.metadata;
    Ok(progress.ready)
}
