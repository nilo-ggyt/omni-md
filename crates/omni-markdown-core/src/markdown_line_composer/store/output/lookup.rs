//! 同一已封口前缀的行查找。每个实际64B读取计一record，没有新正文或GCB索引。

use super::*;

/// 只决定相邻行共享源端点属于哪侧；不授予caret、glyph或Native可见性权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedLineAffinity {
    Upstream,
    Downstream,
}

/// Source坐标可以位于scalar内部：这里只返回真实整行，不能据此签发该位置的编辑权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedLineLookupTarget {
    Source {
        utf16: usize,
        affinity: MarkdownComposedLineAffinity,
    },
    Ordinal(usize),
}

pub enum MarkdownComposedLineLookupAnswer {
    Pending,
    Found(MarkdownComposedLine),
    /// 只说明原快照未覆盖目标，不表示全文EOF，也不从较新的Output补取行。
    PrefixNotCovered,
}

pub struct MarkdownComposedLineLookupStep {
    pub records: usize,
    pub answer: MarkdownComposedLineLookupAnswer,
}

#[derive(Clone, Copy)]
enum Edge {
    Last,
    Skip,
    Previous,
}

/// 固定状态跨quota边界保留已读tail；没有Reader、Source、Font或其他业务强边。
pub struct MarkdownComposedLineLookup {
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    prefix: Range<usize>,
    count: usize,
    last: Option<usize>,
    target: MarkdownComposedLineLookupTarget,
    next: Option<usize>,
    tail: Option<[u8; 64]>,
    edge: Edge,
    current: Option<MarkdownComposedLine>,
    found: Option<MarkdownComposedLine>,
    done: bool,
    failed: bool,
}

#[derive(Clone, Copy)]
enum Relation {
    Before,
    Found,
    After,
}

impl MarkdownComposedLineLookup {
    fn relation(&self, line: &MarkdownComposedLine, source_len: usize) -> Relation {
        match self.target {
            MarkdownComposedLineLookupTarget::Ordinal(target) => {
                match target.cmp(&line.facts.ordinal) {
                    std::cmp::Ordering::Less => Relation::Before,
                    std::cmp::Ordering::Equal => Relation::Found,
                    std::cmp::Ordering::Greater => Relation::After,
                }
            }
            MarkdownComposedLineLookupTarget::Source { utf16, affinity } => {
                let range = &line.facts.source_utf16;
                if utf16 < range.start
                    || (utf16 == range.start
                        && utf16 != 0
                        && line.facts.ordinal != 0
                        && affinity == MarkdownComposedLineAffinity::Upstream)
                {
                    Relation::Before // 上游侧属于前一真实行，不能取后继行相同整数端点。
                } else if utf16 < range.end
                    || (utf16 == range.end
                        && (affinity == MarkdownComposedLineAffinity::Upstream
                            || utf16 == source_len))
                {
                    Relation::Found // 真EOF无后继时可取最后实行；若有尾空行，已从它开始查。
                } else {
                    Relation::After // 下游未布局端点不是完整EOF；不能把前缀尾行补签为下一行。
                }
            }
        }
    }

    fn answer(&self) -> MarkdownComposedLineLookupAnswer {
        if let Some(line) = &self.found {
            // 只复制已经认证过的弱身份和固定事实；重复终态查询仍先经过Output健康门。
            MarkdownComposedLineLookupAnswer::Found(MarkdownComposedLine {
                output: line.output.clone(),
                header: line.header,
                end: line.end,
                previous: line.previous,
                skip: line.skip,
                base_level: line.base_level,
                facts: line.facts.clone(),
            })
        } else if self.done {
            MarkdownComposedLineLookupAnswer::PrefixNotCovered
        } else {
            MarkdownComposedLineLookupAnswer::Pending
        }
    }

    fn previous(&mut self) -> Result<()> {
        let line = self.current.as_ref().ok_or(Error::InconsistentCoverage)?;
        self.next = line.previous;
        self.edge = Edge::Previous;
        if self.next.is_none() {
            return Err(Error::InconsistentCoverage);
        } // 合法目标不可能早于ordinal0；断链不能变成“前缀未覆盖”。
        Ok(())
    }

    fn accept(&mut self, line: MarkdownComposedLine, source_len: usize) -> Result<()> {
        if let Some(current) = &self.current {
            let ordinal = line.facts.ordinal;
            if ordinal >= current.facts.ordinal
                || line.end >= current.header
                || line.facts.source_utf16.end > current.facts.source_utf16.start
            {
                return Err(Error::InconsistentCoverage);
            } // 即使跳过多行，原记录顺序和Source单调性也必须严格成立。
            match self.edge {
                Edge::Skip if current.skip != Some((line.end, ordinal)) => {
                    return Err(Error::InconsistentCoverage);
                }
                Edge::Previous
                    if current.previous != Some(line.end)
                        || ordinal.checked_add(1) != Some(current.facts.ordinal)
                        || line.facts.source_utf16.end != current.facts.source_utf16.start =>
                {
                    return Err(Error::InconsistentCoverage);
                }
                _ => {} // 非相邻skip只核它实际声明的目标；相邻跳链仍须连续源覆盖。
            }
            if ordinal.checked_add(1) == Some(current.facts.ordinal)
                && line.facts.source_utf16.end != current.facts.source_utf16.start
            {
                return Err(Error::InconsistentCoverage);
            }
        } else if !matches!(self.edge, Edge::Last)
            || line.facts.ordinal.checked_add(1) != Some(self.count)
            || Some(line.end) != self.last
            || line.facts.source_utf16.end != self.prefix.end
        {
            return Err(Error::InconsistentCoverage);
        }
        match self.relation(&line, source_len) {
            Relation::Found => {
                self.found = Some(line);
                self.done = true;
                self.current = None;
                self.next = None;
            }
            Relation::Before => {
                let skip = line.skip.map(|value| value.0);
                self.current = Some(line);
                if let Some(skip) = skip {
                    self.next = Some(skip);
                    self.edge = Edge::Skip;
                } else {
                    self.previous()?;
                }
            }
            Relation::After if matches!(self.edge, Edge::Skip) => {
                self.previous()?; // skip越过目标时只退原current的真实前驱，不从错误候选继续。
            }
            Relation::After if matches!(self.edge, Edge::Last) => {
                self.done = true;
                self.next = None;
            }
            Relation::After => return Err(Error::InconsistentCoverage),
        }
        Ok(())
    }
}

impl MarkdownComposedLines {
    /// 捕获原已封口前缀；不读行记录、不定位正文端点，也不借较新的writer重签范围。
    ///
    /// # Errors
    /// 原Source/取消/FD失败或Source目标越界时沿原输出错误门撤权。
    pub fn line_lookup(
        &mut self,
        target: MarkdownComposedLineLookupTarget,
    ) -> Result<MarkdownComposedLineLookup> {
        let result = (|| {
            self.require(&self.authority)?;
            let len = self
                .payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .source
                .utf16_len();
            let outside = match target {
                MarkdownComposedLineLookupTarget::Ordinal(ordinal) => ordinal >= self.count,
                MarkdownComposedLineLookupTarget::Source { utf16, affinity } => {
                    if utf16 > len || utf16 < self.range.start {
                        return Err(Error::InvalidRequest);
                    }
                    utf16 > self.range.end
                        || (utf16 == self.range.end
                            && utf16 < len
                            && affinity == MarkdownComposedLineAffinity::Downstream)
                }
            };
            Ok(MarkdownComposedLineLookup {
                output: Arc::downgrade(&self.identity),
                source: self.authority.clone(),
                prefix: self.range.clone(),
                count: self.count,
                last: self.last,
                target,
                next: (!outside).then_some(self.last).flatten(),
                tail: None,
                edge: Edge::Last,
                current: None,
                found: None,
                done: outside,
                failed: false,
            })
        })();
        self.finish(result)
    }

    /// 每次实际64B行记录读取计1；tail/head可跨两次quota1调用，不把空白调度算成读取。
    /// 原prefix的行证明只定位行，不授予glyph、caret或Native可见性。
    ///
    /// # Errors
    /// 外来/过期游标、零或过大配额、损坏跳链、Source首错及取消均永久拒绝。
    pub fn step_line_lookup(
        &mut self,
        lookup: &mut MarkdownComposedLineLookup,
        record_budget: usize,
    ) -> Result<MarkdownComposedLineLookupStep> {
        let result = (|| {
            self.require(&self.authority)?; // 元数据命中与终态重复查询也必须让原Source错误优先。
            if lookup.failed {
                return Err(Error::Poisoned);
            }
            if !Weak::ptr_eq(&lookup.output, &Arc::downgrade(&self.identity))
                || lookup.source != self.authority
                || lookup.prefix != self.range
                || lookup.count != self.count
                || lookup.last != self.last
            {
                return Err(Error::SourceMismatch);
            }
            if record_budget == 0 || record_budget > STEP {
                return Err(Error::InvalidRequest);
            }
            let source_len = self
                .payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .source
                .utf16_len();
            let mut records = 0;
            while records < record_budget && !lookup.done {
                let end = lookup.next.ok_or(Error::InconsistentCoverage)?;
                if let Some(tail) = lookup.tail.take() {
                    let header = self.line_header(end, &tail)?;
                    records += 1;
                    let head = self.row(header)?;
                    let line = self.decode_line(end, tail, head)?;
                    lookup.accept(line, source_len)?;
                } else {
                    records += 1;
                    let tail = self.row(end)?;
                    self.line_header(end, &tail)?;
                    lookup.tail = Some(tail);
                } // tail保留至下个真实head读取；任何await/Native均不在此Core同步入口内发生。
            }
            self.require(&self.authority)?;
            Ok(MarkdownComposedLineLookupStep {
                records,
                answer: lookup.answer(),
            })
        })();
        let result = self.finish(result);
        if result.is_err() {
            lookup.failed = true;
        }
        result
    }
}

#[cfg(test)]
#[path = "lookup_tests.rs"]
mod tests;
