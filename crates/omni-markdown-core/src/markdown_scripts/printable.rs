//! 原完整ASCII摘要与Latn上下文后缀证明；真实B仍由原Machine消费，EOF才封存。

use super::*;

enum Phase {
    Prove,
    Append,
    SealFrames,
    SealRuns,
    Ready,
}

pub(super) enum Progress {
    Pending,
    Ready,
    Rejected,
    Resume(MarkdownScriptRun),
}

pub(super) struct Preparation {
    proof: Option<crate::markdown_document_buffer::FrozenPrintableAsciiSuffix>,
    run: Option<MarkdownScriptRun>,
    phase: Phase,
    eof: bool,
}

impl Preparation {
    pub(super) fn capture(source: &MarkdownLayoutTextSource) -> Result<Option<Self>> {
        let Some(has_letters) = source.printable_ascii_has_letters()? else {
            return Ok(None); // Unicode或控制符没有此证书，必须交回原SCX/括号Machine。
        };
        if source.byte_len() != source.utf16_len() {
            return Err(MarkdownScriptError::CorruptMetadata);
        } // 完整ASCII每scalar恰好一个byte和UTF16单位，长度矛盾不能降为可疑成功。
        let run = if source.byte_len() == 0 {
            None // 真空源只有两个owner的封存，没有空run或虚构source coverage。
        } else {
            let script = if has_letters {
                // 原精确聚合已证明存在ASCII字母；其唯一specific脚本取自同一Unicode表，
                // 其余Common均为通配。ASCII配对括号不引入第二个specific或段落边界。
                MarkdownScript(properties::get('a').primary)
            } else {
                MarkdownScript(unicode_data::COMMON)
            };
            Some(MarkdownScriptRun {
                start_byte: 0,
                end_byte: source.byte_len(),
                start_utf16: 0,
                end_utf16: source.utf16_len(),
                script,
            })
        };
        source.health()?; // 精确root事实仍属于原Source，晚到的原errno不能被无读取路径绕过。
        Ok(Some(Self {
            proof: None,
            run,
            phase: Phase::Append,
            eof: true,
        }))
    }

    /// 原Machine已确定Latn且没有待配对上下文时，证明该run直到同源真实正文端点的后缀。
    pub(super) fn suffix(
        source: &MarkdownLayoutTextSource,
        mut run: MarkdownScriptRun,
        content_end: usize,
    ) -> Result<Self> {
        let proof = source.begin_printable_ascii_suffix(run.start_utf16..content_end)?;
        // ASCII正证明后两种坐标一一对应；候选尚未知时不把该byte坐标用于任何Source借用。
        run.end_byte = run
            .start_byte
            .checked_add(content_end - run.start_utf16)
            .ok_or(MarkdownScriptError::CoordinateOverflow)?;
        run.end_utf16 = content_end;
        Ok(Self {
            proof: Some(proof),
            run: Some(run),
            phase: Phase::Prove,
            eof: content_end == source.utf16_len(),
        })
    }

    pub(super) fn range(&self) -> Option<std::ops::Range<usize>> {
        self.run.map(|run| run.start_utf16..run.end_utf16)
    }

    pub(super) fn advance(
        &mut self,
        source: &MarkdownLayoutTextSource,
        maximum_borrow: &mut usize,
        store: &mut store::Store,
        step: &mut MarkdownScriptStep,
        budget: MarkdownScriptBudget,
    ) -> Result<Progress> {
        loop {
            match self.phase {
                Phase::Prove => {
                    let actual = source.advance_printable_ascii_suffix(
                        self.proof.as_mut().expect("原后缀证明处于Prove阶段"),
                        budget.bytes - step.bytes,
                        budget.scalars - step.scalars,
                        budget.stack_ops - step.stack_ops,
                    )?;
                    step.bytes += actual.bytes;
                    step.scalars += actual.scalars;
                    step.stack_ops += actual.metadata;
                    *maximum_borrow = (*maximum_borrow).max(actual.maximum_borrow);
                    match actual.answer {
                        None => return Ok(Progress::Pending),
                        Some(false) => return Ok(Progress::Rejected),
                        Some(true) => {
                            self.proof = None;
                            if !self.eof {
                                return Ok(Progress::Resume(
                                    self.run.expect("非EOF证明保原当前run"),
                                ));
                            } // 真B前正文只可推进原Machine，不能在这里直接append自然闭项。
                            self.phase = Phase::Append;
                        }
                    } // 真反例不改原Machine或已完成run；只有整个后缀证明成功才取得封存资格。
                }
                Phase::Append => {
                    if let Some(run) = self.run {
                        if step.records == budget.records {
                            return Ok(Progress::Pending); // 原单run尚未写入，保留候选与原phase等待下次量子。
                        }
                        store.append_run(run)?;
                        step.records += 1;
                    }
                    self.phase = Phase::SealFrames;
                }
                Phase::SealFrames => {
                    if step.stack_ops == budget.stack_ops {
                        return Ok(Progress::Pending);
                    }
                    store.seal_frames()?; // 原空括号FD真实close，不以跳过栈扫描伪造已归还。
                    step.stack_ops += 1;
                    self.phase = Phase::SealRuns;
                }
                Phase::SealRuns => {
                    if step.stack_ops == budget.stack_ops {
                        return Ok(Progress::Pending);
                    }
                    store.seal_runs()?; // 原64B记录真实封存/归还写映射后才可进入最终Ready。
                    step.stack_ops += 1;
                    self.phase = Phase::Ready;
                }
                Phase::Ready => return Ok(Progress::Ready),
            }
        }
    }
}

#[cfg(test)]
mod tests;
