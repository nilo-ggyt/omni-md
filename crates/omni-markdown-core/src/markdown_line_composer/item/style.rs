//! Engine内部样式提交：完整范围复用已收费端点，缩短范围在下一metadata量子验证。
use super::*;

impl MarkdownLineItemCoordinator {
    /// 只保留一份真实原响应。公开同步provide_style不改时序，此内部入口不提前查询索引。
    pub(in crate::markdown_line_composer) fn queue_style(
        &mut self,
        response: MarkdownLineStyleResponse,
    ) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !payload.frontier_enabled {
                return install_style(
                    payload,
                    &self.authority,
                    &self.cancellation,
                    response,
                    StyleCoordinates::default(),
                ); // 未启用候选时保持原同步完整item合同。
            }
            let request = payload
                .request
                .as_ref()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            if !matches!(payload.stage, Stage::Style)
                || !Arc::ptr_eq(&request.nonce, &response.request.nonce)
                || response.request.source != self.authority
                || response.coverage.start != request.range.start
                || response.coverage.end <= response.coverage.start
                || response.coverage.end > request.range.end
            {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            // 响应尚未开始装配；先拒绝过大样式，不能在等待配额时保留无界features/language。
            if response.style.features.len() > crate::MARKDOWN_SHAPING_MAX_FEATURES
                || response
                    .style
                    .language
                    .as_ref()
                    .is_some_and(|value| value.len() > 64)
            {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            if response.coverage.end == request.range.end {
                let endpoint = payload
                    .scan
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                    .endpoint_proofs[1]
                    .take()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                install_style(
                    payload,
                    &self.authority,
                    &self.cancellation,
                    response,
                    StyleCoordinates {
                        endpoint: Some(endpoint),
                    },
                )?;
            } else {
                // 缩短coverage的端点尚未由原ItemScan收费。此处只保真实响应和原nonce，
                // 下一step在查询前收费；不事后把同步IO虚记成下个量子的工作。
                if payload.pending_style.is_some() {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                payload.pending_style = Some(response);
                payload.font_probe_scope.take(); // 已接受响应立刻撤旧试次，等待端点不恢复旧Style权限。
                payload.stage = Stage::StyleEndpoint;
            }
            Ok(())
        })();
        self.finish(result)
    }
}

#[cfg(test)]
mod tests;
