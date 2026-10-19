//! 借用同一Script job的稳定范围；开放前缀不冒充完整run、paragraph或shaping item。
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownScriptPrefixKind {
    /// 原store中已实际提交的完整run。
    ClosedRun,
    /// 唯一脚本和完整cluster只覆盖这个稳定前缀，真实run末端仍未知。
    StableOpenPrefix,
}

/// 私有构造且不Clone；借用阻止原job在消费者持有范围时继续改动当前候选。
pub struct MarkdownStableScriptPrefix<'a> {
    job: &'a MarkdownScriptJob,
    authority: MarkdownLayoutTextAuthority,
    run: MarkdownScriptRun,
    kind: MarkdownScriptPrefixKind,
}

pub enum MarkdownScriptPrefixAnswer<'a> {
    Pending,
    Known(Option<MarkdownStableScriptPrefix<'a>>),
}

impl MarkdownStableScriptPrefix<'_> {
    /// 只读原范围事实；返回前仍核源首错/取消，不能把旧借用当永久权限。
    pub fn facts(&self) -> Result<MarkdownScriptRun> {
        self.job.guard(&self.authority)?;
        let result = self.run;
        self.job.guard(&self.authority)?;
        Ok(result)
    }

    /// 类型区分完整run和稳定开放部分；二者都不授予GCB或Native活文档权限。
    pub fn kind(&self) -> Result<MarkdownScriptPrefixKind> {
        self.job.guard(&self.authority)?;
        Ok(self.kind)
    }

    pub fn source_authority(&self) -> Result<&MarkdownLayoutTextAuthority> {
        self.job.guard(&self.authority)?;
        Ok(&self.authority)
    }
}

impl MarkdownScriptJob {
    /// 仅查询原已提交run或已确定脚本的完整cluster前缀；不推进Source或假造末端。
    pub fn stable_prefix_at(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        unit: MarkdownScriptUnit,
        offset: usize,
    ) -> Result<MarkdownScriptPrefixAnswer<'_>> {
        let result = (|| {
            self.guard(authority)?;
            let closed = self.find_run(unit, offset)?;
            let candidate = match closed {
                MarkdownScriptAnswer::Known(Some(run)) => {
                    Some((run, MarkdownScriptPrefixKind::ClosedRun))
                }
                MarkdownScriptAnswer::Known(None) => None,
                MarkdownScriptAnswer::Pending => {
                    self.machine.stable_open_prefix().and_then(|run| {
                        let (start, end) = match unit {
                            MarkdownScriptUnit::Byte => (run.start_byte, run.end_byte),
                            MarkdownScriptUnit::Utf16 => (run.start_utf16, run.end_utf16),
                        };
                        (start <= offset && offset < end)
                            .then_some((run, MarkdownScriptPrefixKind::StableOpenPrefix))
                    })
                }
            };
            self.guard(authority)?;
            Ok((
                candidate,
                matches!(closed, MarkdownScriptAnswer::Known(None)),
            ))
        })();
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        }; // 即使固定metadata读取失败，也不能遮掉同一源已经确认的首errno。
        let (candidate, eof) = match result {
            Ok(value) => value,
            Err(error) => {
                self.fail(&error);
                return Err(error);
            }
        };
        match candidate {
            Some((run, kind)) => Ok(MarkdownScriptPrefixAnswer::Known(Some(
                MarkdownStableScriptPrefix {
                    job: self,
                    authority: authority.clone(),
                    run,
                    kind,
                },
            ))),
            None if eof => Ok(MarkdownScriptPrefixAnswer::Known(None)),
            None => Ok(MarkdownScriptPrefixAnswer::Pending),
        }
    }
}

#[cfg(test)]
mod tests;
