//! 原GCB的真实稳定记录前缀；GB2位置事实不冒充末ordinal、末cluster或完整扫描。
use super::*;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGraphemePrefixFacts {
    /// 已证明且按全局ordinal连续的边界数；Dense为逻辑记录，物理IO另由原profile计量。
    pub known_boundary_count: usize,
    /// 这个位置及之前的单点分类已完成；它本身不一定是GCB。
    pub confirmed_through: Option<MarkdownGraphemeBoundary>,
    pub last_committed_boundary: Option<MarkdownGraphemeBoundary>,
    pub complete: bool,
    /// 仅原全算法及索引封存完成后才给出最终总数。
    pub total_count: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemePrefixRead {
    Pending,
    Read,
}

/// 独占借用原job，不能Clone或在持有期间推进machine；内部不创建FD、正文副本或新索引。
pub struct MarkdownGraphemePrefix<'a> {
    job: &'a mut MarkdownGraphemeJob,
    authority: MarkdownLayoutTextAuthority,
    facts: MarkdownGraphemePrefixFacts,
}

impl MarkdownGraphemeJob {
    /// 借出当前真实记录覆盖；没有已读字节就没有稳定前缀，原完整Ready含义保持不变。
    pub fn stable_prefix(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<MarkdownGraphemePrefix<'_>> {
        self.guard(authority)?;
        let complete = self.machine.ready;
        let known_boundary_count = self.index.len();
        let last_committed_boundary = if self.index.is_dense() {
            Some(MarkdownGraphemeBoundary {
                byte: self.source.byte_len(),
                utf16: self.source.utf16_len(),
            })
        } else {
            self.machine.last_committed_boundary()
        };
        let facts = MarkdownGraphemePrefixFacts {
            known_boundary_count,
            confirmed_through: self.machine.frontier,
            last_committed_boundary,
            complete,
            total_count: complete.then_some(known_boundary_count),
        };
        self.guard(authority)?;
        Ok(MarkdownGraphemePrefix {
            job: self,
            authority: authority.clone(),
            facts,
        })
    }
}

impl MarkdownGraphemePrefix<'_> {
    pub fn facts(&mut self) -> Result<MarkdownGraphemePrefixFacts> {
        self.job.guard(&self.authority)?;
        Ok(self.facts)
    }

    /// GB2只认证真实不可变Source末位置；不读取未知尾部，也不推算最终边界序号。
    /// 即使返回此位置，Next、Previous及末cluster范围仍必须消费原机器实际记录。
    pub fn source_end_boundary(&mut self) -> Result<MarkdownGraphemeBoundary> {
        self.job.guard(&self.authority)?;
        let end = MarkdownGraphemeBoundary {
            byte: self.job.source.byte_len(),
            utf16: self.job.source.utf16_len(),
        };
        self.job.guard(&self.authority)?;
        Ok(end)
    }

    /// 一次只读取≤256条原已提交记录；未知记录保持Pending且不改输出，真EOF越界才失败。
    pub fn read_boundaries(
        &mut self,
        range: Range<usize>,
        output: &mut [MarkdownGraphemeBoundary],
    ) -> Result<MarkdownGraphemePrefixRead> {
        let result = (|| {
            self.job.guard(&self.authority)?;
            let maximum = self
                .job
                .source
                .utf16_len()
                .checked_add(1)
                .ok_or(MarkdownGraphemeError::CoordinateOverflow)?;
            if range.end.checked_sub(range.start) != Some(output.len())
                || output.len() > 256
                || range.end > maximum
            {
                return Err(MarkdownGraphemeError::InvalidRange);
            }
            if range.end > self.facts.known_boundary_count {
                return if self.facts.complete {
                    Err(MarkdownGraphemeError::InvalidRange)
                } else {
                    Ok(MarkdownGraphemePrefixRead::Pending)
                };
            }
            self.job.index.read_batch(range, output)?;
            Ok(MarkdownGraphemePrefixRead::Read)
        })();
        // 首源错误优先于参数/metadata结果；失败后的整个batch均不可发布。
        let result = match self.job.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result.and_then(|answer| {
                self.job.guard(&self.authority)?;
                Ok(answer)
            }),
        };
        if result.is_err() {
            self.job.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests;
