//! 显式ASCII前沿的依赖调度；不改变原LB稳定前缀、机会查询或完整EOF的含义。
use super::*;

/// 只能由原GCB成功的IsBoundary或同Source的真实GB2末端事实产生；整数不能重造权限。
/// 原scope和Source都是弱身份，票不延长正文、FD、索引页或BreakJob的生命。
pub(crate) struct MarkdownCertifiedGraphemeEndpoint {
    source: MarkdownLayoutTextAuthority,
    scope: Weak<()>,
    boundary: crate::MarkdownGraphemeBoundary,
}
impl MarkdownCertifiedGraphemeEndpoint {
    pub(crate) fn same_scope(&self, other: &Self) -> bool {
        self.source == other.source && Weak::ptr_eq(&self.scope, &other.scope)
    }

    pub(crate) fn boundary(
        &self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<crate::MarkdownGraphemeBoundary> {
        if authority != &self.source {
            return Err(MarkdownLayoutBreakError::SourceMismatch);
        }
        if self.scope.strong_count() == 0 {
            return Err(MarkdownLayoutBreakError::Poisoned);
        }
        Ok(self.boundary)
    }
}

impl MarkdownLayoutBreakJob {
    /// 非末端复用实际GCB查询；真实EOF只取原机器的GB2坐标，不重扫尚未索引的正文。
    /// GB2不授予末ordinal、Previous或完整Ready；旧Pending需求保持，Known(None)不签票。
    pub(crate) fn certified_grapheme_endpoint(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        offset: usize,
    ) -> Result<(GraphemeAnswer, Option<MarkdownCertifiedGraphemeEndpoint>)> {
        let answer = if offset == self.maximum[1] {
            // 长文即使含CRLF而不能使用Dense，GB2仍只依赖同一不可变Source的真实末端。
            // 先后核原健康和身份；稳定前缀借用在检查前结束，不新增读者或推进原扫描。
            self.require_source(authority)?;
            let result = self
                .payload
                .as_mut()
                .ok_or(MarkdownLayoutBreakError::Poisoned)?
                .grapheme
                .stable_prefix(authority)
                .and_then(|mut prefix| prefix.source_end_boundary());
            self.require_source(authority)?;
            match result {
                // 只把原GB2坐标交给下方同scope签票，原索引的完成状态保持不动。
                Ok(boundary) => GraphemeAnswer::Known(Some(boundary)),
                // 子任务取消等失败同样撤销原scope；不能把已知长度当成可复活的末端票。
                Err(error) => return self.poison(error.into()),
            }
        } else {
            // 内部UTF16位置仍须真实字素分类，不能从EOF事实推断低代理项或CRLF内部。
            self.grapheme_query(authority, offset, MarkdownGraphemeQuery::IsBoundary)?
        };
        self.require_source(authority)?;
        let endpoint = match answer {
            GraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == offset => {
                Some(MarkdownCertifiedGraphemeEndpoint {
                    source: authority.clone(),
                    scope: Rc::downgrade(
                        &self
                            .payload
                            .as_ref()
                            .ok_or(MarkdownLayoutBreakError::Poisoned)?
                            .endpoint_scope,
                    ),
                    boundary,
                })
            }
            GraphemeAnswer::Known(Some(_)) => {
                return self.poison(MarkdownLayoutBreakError::InconsistentCoordinates);
            }
            _ => None,
        };
        Ok((answer, endpoint))
    }

    /// 只借原GCB已经提交的记录事实；已知GB2坐标不补出末ordinal或完整Ready。
    pub(crate) fn grapheme_prefix_facts(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<crate::markdown_graphemes::MarkdownGraphemePrefixFacts> {
        self.require_source(authority)?;
        let total = self.ready_grapheme_count(authority)?;
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .stable_prefix(authority)
            .and_then(
                |mut prefix: crate::markdown_graphemes::MarkdownGraphemePrefix<'_>| prefix.facts(),
            );
        self.require_source(authority)?;
        match result {
            Ok(value) if value.total_count == total => Ok(value),
            Ok(_) => self.poison(MarkdownLayoutBreakError::InconsistentCoordinates),
            Err(error) => self.poison(error.into()),
        }
    }

    /// 每次至多256条真实原记录；未知ordinal留下GCB需求，不能被当作空表或EOF。
    pub(crate) fn read_grapheme_prefix(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        range: std::ops::Range<usize>,
        output: &mut [crate::MarkdownGraphemeBoundary],
    ) -> Result<bool> {
        self.require_source(authority)?;
        if self
            .payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .is_ready()
        {
            return self.read_ready_graphemes(authority, range, output);
        } // 真完整表沿原有限读取入口；未完成表才消费稳定前缀票，二者不互换EOF含义。
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .stable_prefix(authority)
            .and_then(|mut prefix| prefix.read_boundaries(range, output));
        self.require_source(authority)?;
        match result {
            Ok(crate::markdown_graphemes::MarkdownGraphemePrefixRead::Read) => Ok(true),
            Ok(crate::markdown_graphemes::MarkdownGraphemePrefixRead::Pending) => {
                self.grapheme_source_demand = true;
                Ok(false)
            }
            Err(error) => self.poison(error.into()),
        }
    }

    pub(crate) fn take_grapheme_source_demand(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<bool> {
        self.require_source(authority)?;
        Ok(std::mem::take(&mut self.grapheme_source_demand))
    }

    /// 下一量子只推进真实阻塞的子算法；不会给LB和GCB各花一份调用方预算。
    pub(crate) fn step_dependency(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        grapheme: bool,
        budget: MarkdownLineBreakBudget,
    ) -> Result<MarkdownLayoutBreakStep> {
        let result = (|| {
            self.require_source(authority)?;
            if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 {
                return Err(MarkdownLayoutBreakError::InvalidRequest);
            }
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLayoutBreakError::Poisoned)?;
            let progress = if grapheme {
                let value = payload.grapheme.step(
                    authority,
                    MarkdownGraphemeBudget {
                        bytes: budget.bytes,
                        scalars: budget.scalars,
                        records: budget.records,
                    },
                )?;
                MarkdownLayoutBreakStep {
                    bytes: value.bytes,
                    scalars: value.scalars,
                    records: value.records,
                    ready: false,
                    needs_dictionary: payload.line.storage_profile().needs_dictionary,
                }
            } else {
                let value = payload.line.step(authority, budget)?;
                MarkdownLayoutBreakStep {
                    bytes: value.bytes,
                    scalars: value.scalars,
                    records: value.records,
                    ready: false,
                    needs_dictionary: value.needs_dictionary,
                }
            };
            let ready = payload.grapheme.is_ready() && payload.line.is_ready();
            self.require_source(authority)?;
            Ok(MarkdownLayoutBreakStep { ready, ..progress })
        })();
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.poison(error),
        }
    }

    /// 只有原全源精确Printable证书生成的Dense GCB完成，才允许LB保持未完成前缀。
    /// 此处复用原GCB表示的来源证明，不重新读正文、不以长度或代次猜测字符域。
    pub(crate) fn ascii_prefix_ready(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<bool> {
        self.require_source(authority)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        let ready =
            payload.grapheme.is_ready() && payload.grapheme.storage_profile().dense_printable_ascii;
        self.require_source(authority)?;
        Ok(ready)
    }

    /// 取走最近原查询的实际LB源等待。该标量不授予边界、range或EOF权限。
    /// 消费方先结束自己的借用，下一量子才推进同job；错误沿原poison路径关闭索引。
    pub(crate) fn take_line_source_demand(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<bool> {
        self.require_source(authority)?;
        Ok(std::mem::take(&mut self.line_source_demand))
    }

    #[cfg(test)]
    pub(crate) fn line_source_bytes_read_for_test(&mut self) -> usize {
        self.payload
            .as_ref()
            .expect("测试只检查仍活的原LB任务")
            .line
            .storage_profile()
            .source_bytes_read
    }
}

#[cfg(test)]
mod tests;
