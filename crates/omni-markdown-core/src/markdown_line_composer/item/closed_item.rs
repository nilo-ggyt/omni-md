//! 自然闭合小项的单次坐标票；不把开放前缀或预算切片提升为完整排版范围。

use super::*;
use crate::markdown_layout_breaks::MarkdownCertifiedGraphemeEndpoint;

mod fonts;
pub(super) use fonts::{
    FontRuns, advance_fonts, inspect_shaped, lend_font_item, provide_font_style,
};

// UTF16上限同时界定标量上限；byte上限来自原GCB票，不为准入再读取文字或定位端点。
pub(super) const MAX_CLOSED_UTF16: usize = 256;
pub(super) const MAX_CLOSED_BYTES: usize = 1024;

/// 只有当前真实Style阶段能构造；不可Clone且保原端点弱scope，公开Range不能重放。
pub(crate) struct ClosedItem {
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    scope: std::sync::Weak<()>,
    endpoints: [MarkdownCertifiedGraphemeEndpoint; 2],
    cancellation: MarkdownShapingCancellation,
}

pub(super) enum Admission {
    Closed(ClosedItem),
    Frontier(MarkdownCertifiedGraphemeEndpoint),
}

/// 原唯一item槽先保存有限准备续体；Font/样式/段落事实直到原Job装配后原样移交。
pub(super) struct PendingItem {
    pub(super) shaping: crate::markdown_shaping::PendingClosedShaping,
    pub(super) font: MarkdownLayoutFontAuthority,
    pub(super) registration: Option<MarkdownLineFontRegistration>,
    pub(super) replay_style: MarkdownLineComposeStyle,
    pub(super) facts: MarkdownLineItemFacts,
}

impl ClosedItem {
    /// 原Font确实变化，右端由实际字体选择或自然项端点封闭；普通短响应没有此入口。
    pub(super) fn from_terminal_font_change(
        terminal: &crate::markdown_shaping::frontier::MarkdownShapingTerminalGrapheme,
        endpoints: [MarkdownCertifiedGraphemeEndpoint; 2],
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Self> {
        let range = terminal.source_range();
        let bytes = terminal.byte_range();
        let first = endpoints[0].boundary(authority)?;
        let last = endpoints[1].boundary(authority)?;
        if terminal.source_authority() != authority
            || terminal.original_font().same_payload(font)
            || !endpoints[0].same_scope(&endpoints[1])
            || first.utf16 != range.start
            || last.utf16 < range.end
            || last.utf16 > terminal.item_end()
            || first.byte != bytes.start
            || last.byte < bytes.end
            || last.utf16 - first.utf16 > MAX_CLOSED_UTF16
            || last.byte - first.byte > MAX_CLOSED_BYTES
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        } // 原阻塞GCB必须完整包含；连续回退字素只按真实右边界合并，不按预算截断。
        let scope = terminal.owner_scope();
        if scope.strong_count() == 0 {
            return Err(MarkdownLineComposeError::Poisoned);
        }
        Ok(Self {
            source: authority.clone(),
            font: font.clone(),
            scope,
            endpoints,
            cancellation: terminal.cancellation(),
        })
    }

    pub(super) fn admit(
        payload: &mut Payload,
        authority: &MarkdownLayoutTextAuthority,
        response: &MarkdownLineStyleResponse,
        end: MarkdownCertifiedGraphemeEndpoint,
        cancellation: &MarkdownLineComposeCancellation,
    ) -> Result<Admission> {
        payload.source.health()?;
        let request = payload
            .request
            .as_ref()
            .ok_or(MarkdownLineComposeError::InvalidRequest)?;
        if payload.open_script || response.coverage != request.range {
            return Ok(Admission::Frontier(end)); // 临时SCX上界和缩短响应都不证明自然项结束。
        }
        let scan = payload
            .scan
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let start = scan.endpoint_proofs[0]
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let first = start.boundary(authority)?;
        let last = end.boundary(authority)?;
        if !start.same_scope(&end)
            || !matches!(payload.stage, Stage::Style)
            || !Arc::ptr_eq(&request.nonce, &response.request.nonce)
            || request.source != *authority
            || !payload.source.permits(authority)
            || scan.start != request.range.start
            || scan.next != request.range.end
            || first.utf16 != request.range.start
            || last.utf16 != request.range.end
            || first.byte >= last.byte
            || last.byte > payload.source.byte_len()
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        } // 两张真实GCB端点票和原扫描必须指向同一来源；失配不能降格成普通前沿待决。
        if request.range.len() > MAX_CLOSED_UTF16 || last.byte - first.byte > MAX_CLOSED_BYTES {
            return Ok(Admission::Frontier(end)); // 真实大项继续原Frontier，不缩短样式或段落。
        }
        let scope = Arc::downgrade(
            payload
                .font_probe_scope
                .as_ref()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
        );
        payload.source.health()?;
        let start = payload
            .scan
            .as_mut()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .endpoint_proofs[0]
            .take()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        Ok(Admission::Closed(Self {
            source: authority.clone(),
            font: response.font.authority(),
            scope,
            endpoints: [start, end],
            cancellation: cancellation.shape.clone(),
        })) // 成功才移动原票；没有第二Source、Font强边、Reader或索引。
    }

    /// 消费原票、样式scope和原取消位；裸坐标不能进入这个封闭转换。
    pub(crate) fn into_input(
        self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> std::result::Result<
        (Range<usize>, Range<usize>, MarkdownShapingCancellation),
        MarkdownShapingError,
    > {
        if &self.source != source {
            return Err(MarkdownShapingError::SourceMismatch);
        } // 相同文字、相同坐标的新Source不能继承原Style阶段的闭项权限。
        if !self.font.same_payload(font) {
            return Err(MarkdownShapingError::FontMismatch);
        } // 同名Font不能替代原响应实际选定的payload。
        if self.scope.strong_count() == 0 {
            return Err(MarkdownShapingError::Poisoned);
        } // 原Style阶段结束后，即使外部仍持坐标也不得装配Job。
        let [start, end] = self.endpoints;
        let first = start
            .boundary(source)
            .map_err(|_| MarkdownShapingError::SourceMismatch)?;
        let last = end
            .boundary(source)
            .map_err(|_| MarkdownShapingError::SourceMismatch)?;
        // 端点getter只检查来源和弱scope，不执行I/O；原Source健康门由装配入口先核。
        if !start.same_scope(&end) {
            return Err(MarkdownShapingError::SourceMismatch);
        }
        Ok((
            first.utf16..last.utf16,
            first.byte..last.byte,
            self.cancellation,
        ))
    }
}

#[cfg(test)]
mod tests;
