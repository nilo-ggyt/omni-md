//! 原完整软行的尾空白查询；只保两个真实端点、一个64字节读片与至多四字节UTF8续体。

use super::*;
use crate::MarkdownLineComposeBudget;

/// None 表示仍需原量子；Some 仅是对齐时可去除的完整尾空白 GCB 之前的 UTF16 末端事实。
/// 含非空白 Prepend/组合标量的 GCB 保留完整宽度；本事实不授编辑或局部字形切片权限，
/// 也不改变 LineEnd 已封存的完整 advance。
pub struct MarkdownComposedLineTrailingWhitespaceStep {
    /// 本轮真正借到的源字节；包含固定片内尚未分类的预读字节。
    pub bytes: usize,
    /// 本轮完整解码的 Unicode 标量；半标量留在原四字节续体，不提前收费为完整字符。
    pub scalars: usize,
    /// 原 GCB 目录真正读取的64字节记录，与正文字节分开计量。
    pub records: usize,
    /// 原折叠事实与 GCB 记录消费的元数据配额，不能在 records 之外隐藏目录遍历。
    pub metadata_units: usize,
    /// 仅在原标量尾空白与完整 GCB 两种事实均完成后发布；不是可独立使用的权限票。
    pub answer: Option<usize>,
}

#[derive(Clone, Copy)]
enum Phase {
    Folds,
    End,
    Start,
    Tail,
    Trim,
    Done,
}

/// 查询只绑定原 Output/Line/Source 的弱身份；不保活 reader、正文、字体或新目录。
/// 读片是本次反向扫描的有限输入续体，归还查询即结束，不随行数或文档长度增长。
pub struct MarkdownComposedLineTrailingWhitespace {
    // 同一 Source 的另一排版宽度也不能接续本行；Weak 不延长 Output 的 FD 生命周期。
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    // 保原签发行的目录身份；相同 UTF16 范围不代表相同行记录或相同完整 LineEnd。
    header: usize,
    end: usize,
    range: Range<usize>,
    phase: Phase,
    fold_index: usize,
    // 三次查找顺序复用一个真实目录续体：行尾字节、行首字节、最终完整字素上界。
    boundary: graphemes::BoundarySearch,
    first_byte: usize,
    next_byte: usize,
    utf16: usize,
    // 每次只向前归还一个片；remaining 指向尚未反向分类的片内字节，不是新源位置。
    bytes: [u8; 64],
    remaining: usize,
    // 一个源标量可能横跨两个 piece 或四个一字节量子；只保它的反向原字节。
    scalar: [u8; 4],
    scalar_len: usize,
    answer: Option<usize>,
    failed: bool,
}

impl MarkdownComposedLines {
    /// 只接本 Output 实际签发的完整行；构造时不读取行记录，也不重新定位长正文端点。
    pub fn begin_line_trailing_whitespace(
        &self,
        line: &MarkdownComposedLine,
    ) -> Result<MarkdownComposedLineTrailingWhitespace> {
        self.require(&self.authority)?;
        if !Weak::ptr_eq(&line.output, &Arc::downgrade(&self.identity)) {
            return Err(Error::SourceMismatch);
        }
        if line.facts.kind == MarkdownComposedLineKind::TableRow {
            return Err(Error::TableRange);
        } // 原 Grid 行没有普通正文的内部 GCB；只能由真实格显示源查询它自己的软行。
        let range = line.facts.source_utf16.clone();
        let empty = range.is_empty();
        // 原 EmptyParagraph 已有真实封口行；没有正文或 GCB 目录读取需要，仍保来源撤权检查。
        let query = MarkdownComposedLineTrailingWhitespace {
            output: Arc::downgrade(&self.identity),
            source: self.authority.clone(),
            header: line.header,
            end: line.end,
            phase: if empty { Phase::Done } else { Phase::Folds },
            fold_index: 0,
            boundary: graphemes::BoundarySearch::offset(range.end),
            first_byte: 0,
            next_byte: 0,
            utf16: range.end,
            bytes: [0; 64],
            remaining: 0,
            scalar: [0; 4],
            scalar_len: 0,
            answer: empty.then_some(range.start),
            range,
            failed: false,
        };
        self.require(&self.authority)?;
        Ok(query)
    }

    /// 端点沿原 GCB FD 目录分量子认证，正文每片仅反向借至多64字节；一字节预算可跨轮解码。
    /// 错误沿原输出 finish 处理，真实 Source 首错优先且永久结束本查询的读取责任。
    pub fn step_line_trailing_whitespace(
        &mut self,
        line: &MarkdownComposedLine,
        query: &mut MarkdownComposedLineTrailingWhitespace,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedLineTrailingWhitespaceStep> {
        let result = self.advance_line_trailing_whitespace(line, query, budget);
        let result = self.finish(result);
        if result.is_err() {
            query.failed = true;
        }
        result
    }

    fn advance_line_trailing_whitespace(
        &mut self,
        line: &MarkdownComposedLine,
        query: &mut MarkdownComposedLineTrailingWhitespace,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedLineTrailingWhitespaceStep> {
        self.require(&query.source)?;
        if query.failed {
            return Err(Error::Poisoned);
        }
        if !Weak::ptr_eq(&query.output, &Arc::downgrade(&self.identity))
            || !Weak::ptr_eq(&line.output, &query.output)
            || line.header != query.header
            || line.end != query.end
            || line.facts.source_utf16 != query.range
        {
            return Err(Error::SourceMismatch);
        } // 不能以另一行的相同坐标、另一宽度的输出或较新的前缀接续这次字节进度。
        if budget.bytes == 0
            || budget.scalars == 0
            || budget.records == 0
            || budget.metadata_units == 0
        {
            return Err(Error::InvalidRequest);
        }
        let budget = MarkdownLineComposeBudget {
            bytes: budget.bytes.min(4096),
            scalars: budget.scalars.min(256),
            records: budget.records.min(256),
            metadata_units: budget.metadata_units.min(256),
        };
        // 限制只决定本轮实际推进量；配额耗尽不能被当作行末、标量末或可删空白边界。
        let mut step = MarkdownComposedLineTrailingWhitespaceStep {
            bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            answer: query.answer,
        };
        loop {
            match query.phase {
                Phase::Done => break,
                Phase::Folds => {
                    let folds = self
                        .payload
                        .as_ref()
                        .ok_or(Error::Poisoned)?
                        .folds
                        .as_ref()
                        .map(|folds| folds.spans());
                    let Some(fold) = folds.and_then(|folds| folds.get(query.fold_index)) else {
                        query.phase = Phase::End;
                        continue;
                    };
                    if step.metadata_units == budget.metadata_units {
                        break;
                    }
                    step.metadata_units += 1;
                    query.fold_index += 1;
                    if fold.start.utf16 < query.range.end && query.range.start < fold.end.utf16 {
                        return Err(Error::FoldedRange(fold.start.utf16..fold.end.utf16));
                    } // 隐藏正文没有本输出的内部字素权限，不能为对齐而扫描其原始尾空白。
                    if fold.start.utf16 >= query.range.end {
                        query.phase = Phase::End;
                    }
                }
                Phase::End | Phase::Start | Phase::Trim => {
                    // 同一条记录同时占记录和元数据两份额度；一份用尽就将原查找续体交还调用方。
                    let limit = (budget.records - step.records)
                        .min(budget.metadata_units - step.metadata_units);
                    if limit == 0 {
                        break;
                    }
                    let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
                    let actual = query.boundary.step(
                        self.graphemes,
                        source.byte_len(),
                        source.utf16_len(),
                        limit,
                        |record| self.row(record),
                    )?;
                    step.records += actual.records;
                    step.metadata_units += actual.records;
                    let Some((_, Some(boundary))) = actual.answer else {
                        if actual.answer.is_some() {
                            return Err(Error::InconsistentCoverage);
                        }
                        break;
                    };
                    if matches!(query.phase, Phase::Trim) {
                        if boundary.utf16 < query.utf16 || boundary.utf16 > query.range.end {
                            return Err(Error::InconsistentCoverage);
                        }
                        query.answer = Some(boundary.utf16);
                        query.phase = Phase::Done;
                        continue;
                    } // 只去掉完整尾空白 GCB；末个非空白标量与其后空白同簇时保留该簇完整几何。
                    let end = matches!(query.phase, Phase::End);
                    if boundary.utf16
                        != if end {
                            query.range.end
                        } else {
                            query.range.start
                        }
                    {
                        return Err(Error::InconsistentCoverage);
                    } // 只认真实整行两端，不把 lower_bound 的后继边界冒充当前 UTF16 的字节坐标。
                    if end {
                        query.next_byte = boundary.byte;
                        query.boundary = graphemes::BoundarySearch::offset(query.range.start);
                        query.phase = Phase::Start;
                    } else {
                        query.first_byte = boundary.byte;
                        query.phase = Phase::Tail;
                    }
                }
                Phase::Tail => {
                    let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
                    query.scan_tail(source, budget, &mut step)?;
                    if matches!(query.phase, Phase::Tail) {
                        break;
                    }
                }
            }
        }
        step.answer = query.answer;
        Ok(step)
    }
}

impl MarkdownComposedLineTrailingWhitespace {
    /// 字节归当前查询所有，Source 只在本次借用中存在；不能借这段续体重新定位正文或越过行首。
    /// 配额用尽仅保留至多一个读片和半标量，首个非空白出现即回原目录核完整 GCB。
    fn scan_tail(
        &mut self,
        source: &MarkdownLayoutTextSource,
        budget: MarkdownLineComposeBudget,
        step: &mut MarkdownComposedLineTrailingWhitespaceStep,
    ) -> Result<()> {
        while step.scalars < budget.scalars {
            if self.remaining == 0 {
                if self.next_byte == self.first_byte {
                    if self.scalar_len != 0 || self.utf16 != self.range.start {
                        return Err(Error::InconsistentCoverage);
                    }
                    self.answer = Some(self.range.start);
                    self.phase = Phase::Done;
                    return Ok(());
                }
                if !self.read_fragment(source, budget.bytes, step)? {
                    return Ok(());
                }
            }
            let Some(character) = self.receive_scalar()? else {
                continue;
            };
            step.scalars += 1;
            if character.is_whitespace() {
                self.utf16 = self
                    .utf16
                    .checked_sub(character.len_utf16())
                    .filter(|value| *value >= self.range.start)
                    .ok_or(Error::InconsistentCoverage)?;
                continue;
            }
            if self.utf16 == self.range.end {
                self.answer = Some(self.utf16);
                self.phase = Phase::Done;
            } else {
                self.boundary = graphemes::BoundarySearch::offset(self.utf16);
                self.phase = Phase::Trim;
            }
            return Ok(()); // L1 的方向控制 reset 范围不属于 Unicode 空白，不能继续向前吞掉它。
        }
        Ok(())
    }

    /// 只读取本行尚未消费的实际字节；预读后未分类的字节也计入本轮读量，不能藏在下一轮。
    /// 失败不发布部分片，外层原 finish 仍负责 Source 首错、取消和唯一输出 reader 的释放。
    fn read_fragment(
        &mut self,
        source: &MarkdownLayoutTextSource,
        byte_budget: usize,
        step: &mut MarkdownComposedLineTrailingWhitespaceStep,
    ) -> Result<bool> {
        let count = (byte_budget - step.bytes)
            .min(self.bytes.len())
            .min(self.next_byte - self.first_byte);
        if count == 0 {
            return Ok(false);
        }
        let start = self.next_byte - count;
        let mut copied = 0;
        source.visit_byte_range(start..self.next_byte, |bytes| {
            self.bytes[copied..copied + bytes.len()].copy_from_slice(bytes);
            copied += bytes.len();
            Ok(())
        })?;
        step.bytes += copied;
        if copied != count {
            return Err(Error::InconsistentCoverage);
        }
        self.remaining = count;
        self.next_byte = start;
        Ok(true)
    }

    /// 反向读片可以从 UTF8 内部开始；只在收到真实首字节后解码完整标量并清空固定续体。
    /// 标量尚不完整时不移动 UTF16，不把一个高位字节猜成空白或新的源码边界。
    fn receive_scalar(&mut self) -> Result<Option<char>> {
        if self.scalar_len == self.scalar.len() {
            return Err(crate::MarkdownDocumentBufferError::InvalidUtf8Document.into());
        }
        self.remaining -= 1;
        let byte = self.bytes[self.remaining];
        self.scalar[self.scalar_len] = byte;
        self.scalar_len += 1;
        if byte & 0xc0 == 0x80 {
            return Ok(None);
        }
        self.scalar[..self.scalar_len].reverse();
        let scalar = std::str::from_utf8(&self.scalar[..self.scalar_len])
            .map_err(|_| crate::MarkdownDocumentBufferError::InvalidUtf8Document)?;
        let character = scalar.chars().next().ok_or(Error::InconsistentCoverage)?;
        if character.len_utf8() != self.scalar_len {
            return Err(Error::InconsistentCoverage);
        }
        self.scalar_len = 0;
        Ok(Some(character))
    }
}
