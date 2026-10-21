//! Queue an internal engine style response: complete coverage reuses original
//! charged endpoints, while shortened coverage validates in a later metadata quantum.
use super::*;

impl MarkdownLineItemCoordinator {
    /// Retain one original response without querying an endpoint prematurely.
    /// Public synchronous provide_style retains its separate immediate-validation timing.
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
                // Without frontier mode, preserve the existing synchronous
                // complete-item validation and construction contract.
                return install_style(
                    payload,
                    &self.authority,
                    &self.cancellation,
                    response,
                    StyleCoordinates::default(),
                );
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
                // Require this original outstanding nonce and its contiguous
                // response extent before retaining any candidate style.
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            // Reject oversized style before construction or pending storage;
            // waiting for a quantum cannot retain unbounded features/language.
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
                // Full coverage already owns the real right endpoint charged
                // by ItemScan. Transfer that exact proof rather than querying again.
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
                // ItemScan did not pay for this shortened endpoint. Retain only
                // the actual response/nonce; the next step charges before querying,
                // without relabeling synchronous I/O as later quantum work.
                if payload.pending_style.is_some() {
                    // One original request cannot accumulate multiple retained responses.
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                payload.pending_style = Some(response);
                // Accepting the response revokes old font trials immediately;
                // pending endpoint work cannot restore old style authority.
                payload.font_probe_scope.take();
                payload.stage = Stage::StyleEndpoint;
            }
            Ok(())
        })();
        self.finish(result)
    }
}

#[cfg(test)]
mod tests;
