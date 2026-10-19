//! 原协调器签发单个真实字素的字体试次权限；不建立第二份GCB、Script或UBA索引。

use std::sync::Weak;

use super::*;

/// 只共享原冻结Source，不复制文字；同一请求被响应、取消或归还后，弱scope立即失效。
/// 一个完整GCB可以包含很多标量，凭此票不能宣称底层整形已具有固定时间上界。
#[derive(Clone)]
pub struct MarkdownLineFontProbe {
    source: MarkdownLayoutTextSource,
    request: MarkdownLineStyleRequest,
    range: Range<usize>,
    scope: Weak<()>,
    cancellation: MarkdownLineComposeCancellation,
}

impl MarkdownLineFontProbe {
    /// 优先核原Source首错，再核原请求和取消位；不查活文档或申请任何文本窗口。
    ///
    /// # Errors
    /// 原Source失败、取消或原样式阶段已结束时拒绝，旧探测不能由后继请求重新启用。
    pub fn validate(&self) -> Result<()> {
        self.source.health()?;
        if self.cancellation.is_cancelled() {
            return Err(MarkdownLineComposeError::Cancelled);
        }
        if self.scope.strong_count() == 0 {
            return Err(MarkdownLineComposeError::Poisoned);
        }
        Ok(())
    }

    /// 只借同一个不可变来源供纯Core worker共享；不会再捕获Doc或额外构造索引。
    pub fn source(&self) -> Result<&MarkdownLayoutTextSource> {
        self.validate()?;
        Ok(&self.source)
    }

    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// 字体选择只能服务签发时的原nonce；相同范围、同文本或同Source都不能代替它。
    pub fn accepts_request(&self, request: &MarkdownLineStyleRequest) -> Result<bool> {
        self.validate()?;
        Ok(Arc::ptr_eq(&self.request.nonce, &request.nonce)
            && self.request.source == request.source)
    }

    /// 方向、Script及BOT/EOT沿原UBA/段落事实；调用者仅提供原有限样式。
    ///
    /// # Errors
    /// 原权限失效或样式超出原Core固定预算时拒绝，不截断language/features或回退默认值。
    pub fn shaping_options(
        &self,
        style: &MarkdownLineComposeStyle,
    ) -> Result<MarkdownShapingOptions> {
        self.validate()?;
        if !style.points.is_finite()
            || style.points <= 0.0
            || style.features.len() > crate::MARKDOWN_SHAPING_MAX_FEATURES
            || style
                .language
                .as_ref()
                .is_some_and(|value| value.len() > 64)
        {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        Ok(MarkdownShapingOptions {
            direction: if self.request.level & 1 == 0 {
                MarkdownShapingDirection::LeftToRight
            } else {
                MarkdownShapingDirection::RightToLeft
            },
            script: self.request.script,
            language: style.language.clone(),
            points: style.points,
            beginning_of_text: self.range.start == self.request.paragraph.start,
            end_of_text: self.range.end == self.request.paragraph.end,
            features: style.features.clone(),
        })
    }
}

impl MarkdownLineItemCoordinator {
    /// 从当前唯一NeedStyle的原依赖领取首个完整GCB；固定次数目录查询不扫描字素正文。
    ///
    /// 返回值只证明此一个GCB的Script、UBA和来源，不能将其字体选择扩展到整个item。
    ///
    /// # Errors
    /// 非当前nonce、阶段不符或原依赖不连续时撤销该协调器；失败不能另外创建索引补签。
    pub fn font_probe(
        &mut self,
        request: &MarkdownLineStyleRequest,
    ) -> Result<MarkdownLineFontProbe> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let original = payload
                .request
                .as_ref()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            if !matches!(payload.stage, Stage::Style)
                || !Arc::ptr_eq(&original.nonce, &request.nonce)
                || original.source != request.source
            {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            #[cfg(feature = "paged-shaping-frontier")]
            if original.continuation_font.is_some() {
                // 原Frontier或自然闭项只在缺字/相邻GCB两张原端点票齐备后签发request；它本身
                // 就是完整GCB，继续借同一个Style scope，不再同步重查原索引或正文。
                let scope = payload
                    .font_probe_scope
                    .as_ref()
                    .expect("原字体交接请求必须持试次scope");
                return Ok(MarkdownLineFontProbe {
                    source: payload.source.clone(),
                    request: original.clone(),
                    range: original.range.clone(),
                    scope: Arc::downgrade(scope),
                    cancellation: self.cancellation.clone(),
                });
            }
            // 原起点已由itemization核为GCB；这里仍查询同一目录，避免给旧状态补签端点。
            let start = request.range.start;
            match payload.breaks.grapheme_query(
                &self.authority,
                start,
                MarkdownGraphemeQuery::IsBoundary,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value)) if value.utf16 == start => {}
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            }
            let end = match payload.breaks.grapheme_query(
                &self.authority,
                start,
                MarkdownGraphemeQuery::Next,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value))
                    if value.utf16 > start && value.utf16 <= request.range.end =>
                {
                    value.utf16
                }
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            // Script来自原已完成的同一个Job，不能只看首scalar或新建局部run猜整个GCB。
            #[cfg(feature = "paged-shaping-frontier")]
            let script = match payload.scripts.stable_prefix_at(
                &self.authority,
                MarkdownScriptUnit::Utf16,
                start,
            )? {
                crate::markdown_scripts::MarkdownScriptPrefixAnswer::Known(Some(prefix)) => {
                    Some(prefix.facts()?)
                }
                _ => None,
            };
            #[cfg(not(feature = "paged-shaping-frontier"))]
            let script =
                match payload
                    .scripts
                    .run_at(&self.authority, MarkdownScriptUnit::Utf16, start)?
                {
                    MarkdownScriptAnswer::Known(value) => value,
                    MarkdownScriptAnswer::Pending => None,
                };
            match script {
                Some(run)
                    if run.start_utf16 <= start
                        && run.end_utf16 >= end
                        && run.script.iso15924_tag() == request.script => {}
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            }
            let scope = payload
                .font_probe_scope
                .as_ref()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            Ok(MarkdownLineFontProbe {
                source: payload.source.clone(),
                request: original.clone(),
                range: start..end,
                scope: Arc::downgrade(scope),
                cancellation: self.cancellation.clone(),
            })
        })();
        self.finish(result)
    }
}
