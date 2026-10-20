//! Search the original FD links one actual 64-byte read at a time; never borrow a later prefix or copy text.
use super::{links::BoundaryRecord, *};

#[derive(Clone, Copy)]
enum Target {
    Offset(usize),
    Ordinal(usize),
}

#[derive(Clone, Copy)]
enum Next {
    First,
    Choose,
    Skip,
    Previous,
}

/// Retain one authenticated directory record and its next link, without a reader or a second GCB index.
/// 本次行组合可复制已解码的右端，再各自向左查真实 part；副本不持资源，也不跨行保留。
#[derive(Clone)]
pub(in crate::markdown_line_composer::store) struct BoundarySearch {
    target: Target,
    current: Option<BoundaryRecord>,
    next: Next,
    answer: Option<(usize, Option<MarkdownGraphemeBoundary>)>,
}

pub(in crate::markdown_line_composer::store) struct BoundarySearchStep {
    pub records: usize,
    pub answer: Option<(usize, Option<MarkdownGraphemeBoundary>)>,
}

impl BoundarySearch {
    pub(in crate::markdown_line_composer::store) fn offset(offset: usize) -> Self {
        Self {
            target: Target::Offset(offset),
            current: None,
            next: Next::First,
            answer: None,
        }
    }

    pub(in crate::markdown_line_composer::store) fn ordinal(ordinal: usize) -> Self {
        Self {
            target: Target::Ordinal(ordinal),
            current: None,
            next: Next::First,
            answer: None,
        }
    }

    /// 同一次范围查询先查右端，再从已经解码的原记录查左端；不跨调用保留游标或来源。
    /// 只接受已完成的 offset 查询及更早目标，原 current 必须仍在目标右侧；后续每条
    /// previous/skip 仍由 step 核 ordinal、字节/UTF16 严格顺序，不从旧答案猜新边界。
    pub(in crate::markdown_line_composer::store) fn preceding_offset(
        &mut self,
        offset: usize,
    ) -> Result<()> {
        let Target::Offset(previous) = self.target else {
            return Err(Error::InvalidRequest);
        };
        if offset > previous
            || self.answer.is_none()
            || self
                .current
                .is_none_or(|current| current.value.utf16 < offset)
        {
            return Err(Error::InvalidRequest);
        }
        self.target = Target::Offset(offset);
        self.answer = None;
        self.next = Next::Choose;
        Ok(())
    }

    /// Authenticate each original skip/previous rank and source order; quota one retains the decoded current record.
    pub(in crate::markdown_line_composer::store) fn step(
        &mut self,
        directory: GraphemeDirectory,
        bytes: usize,
        utf16: usize,
        limit: usize,
        mut read: impl FnMut(usize) -> Result<[u8; 64]>,
    ) -> Result<BoundarySearchStep> {
        if self.answer.is_none() {
            match self.target {
                Target::Ordinal(ordinal) if ordinal >= directory.count => {
                    self.answer = Some((ordinal, None));
                }
                Target::Offset(offset) if offset > directory.covered.utf16 => {
                    self.answer = Some((directory.count, None));
                }
                _ => {} // Covered targets still require their real record; UTF-16 length cannot establish a rank.
            }
        }
        let mut records = 0;
        while self.answer.is_none() {
            if matches!(self.next, Next::Choose) {
                let current = self.current.ok_or(Error::InconsistentCoverage)?;
                match self.target {
                    Target::Offset(offset)
                        if current.value.utf16 == offset || current.ordinal == 0 =>
                    {
                        self.answer = Some((current.ordinal, Some(current.value)));
                        break;
                    }
                    Target::Ordinal(ordinal) if current.ordinal == ordinal => {
                        self.answer = Some((ordinal, Some(current.value)));
                        break;
                    }
                    Target::Offset(_) => self.next = Next::Skip,
                    Target::Ordinal(ordinal) => {
                        self.next = if current.skip.is_some_and(|(_, rank)| rank >= ordinal) {
                            Next::Skip
                        } else {
                            Next::Previous
                        };
                    }
                }
            }
            if records == limit {
                break;
            } // Suspend before the next real read; pure transitions do not consume fabricated record work.
            let (record, ordinal) = match self.next {
                Next::First => (
                    directory.last,
                    directory
                        .count
                        .checked_sub(1)
                        .ok_or(Error::InconsistentCoverage)?,
                ),
                Next::Skip => self
                    .current
                    .ok_or(Error::InconsistentCoverage)?
                    .skip
                    .ok_or(Error::InconsistentCoverage)?,
                Next::Previous => {
                    let current = self.current.ok_or(Error::InconsistentCoverage)?;
                    (
                        current.previous.ok_or(Error::InconsistentCoverage)?,
                        current
                            .ordinal
                            .checked_sub(1)
                            .ok_or(Error::InconsistentCoverage)?,
                    )
                }
                Next::Choose => unreachable!("纯选择阶段已经收束到实际读取或终态"),
            };
            let value = links::decode(read(record)?, record, directory.count, bytes, utf16)?;
            records += 1;
            if let Some(current) = self.current {
                if value.ordinal != ordinal
                    || value.ordinal >= current.ordinal
                    || value.value.byte >= current.value.byte
                    || value.value.utf16 >= current.value.utf16
                    || (ordinal.checked_add(1) == Some(current.ordinal)
                        && current.previous != Some(record))
                {
                    return Err(Error::InconsistentCoverage);
                }
                match self.target {
                    Target::Offset(offset) if value.value.utf16 < offset => {
                        if matches!(self.next, Next::Previous)
                            || value.ordinal.checked_add(1) == Some(current.ordinal)
                        {
                            self.answer = Some((current.ordinal, Some(current.value)));
                        } else {
                            self.next = Next::Previous;
                        }
                        continue; // An overshooting skip leaves current intact and follows its actual predecessor instead.
                    }
                    _ => {} // The validated ordinal or offset still on the target side may replace current.
                }
            } else if value.ordinal.checked_add(1) != Some(directory.count)
                || value.value != directory.covered
            {
                return Err(Error::InconsistentCoverage);
            }
            self.current = Some(value);
            self.next = Next::Choose;
        }
        Ok(BoundarySearchStep {
            records,
            answer: self.answer,
        })
    }
}

/// Legacy synchronous consumers share this cursor; budgeted interaction calls step without an outer IO loop.
pub(in crate::markdown_line_composer::store) fn ordinal_boundary(
    directory: GraphemeDirectory,
    ordinal: usize,
    bytes: usize,
    utf16: usize,
    read: impl FnMut(usize) -> Result<[u8; 64]>,
) -> Result<Option<MarkdownGraphemeBoundary>> {
    let step = BoundarySearch::ordinal(ordinal).step(directory, bytes, utf16, usize::MAX, read)?;
    Ok(step.answer.ok_or(Error::InconsistentCoverage)?.1)
}

pub(in crate::markdown_line_composer::store) fn lower_bound(
    directory: GraphemeDirectory,
    offset: usize,
    bytes: usize,
    utf16: usize,
    read: impl FnMut(usize) -> Result<[u8; 64]>,
) -> Result<(usize, Option<MarkdownGraphemeBoundary>)> {
    BoundarySearch::offset(offset)
        .step(directory, bytes, utf16, usize::MAX, read)?
        .answer
        .ok_or(Error::InconsistentCoverage)
}
