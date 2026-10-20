//! 一个真实 Root 块的强调匹配栈；记录追加到匿名文件，RAM 只保固定2KiB尾页和当前匹配。

use super::*;
use crate::markdown_document_buffer::table_source::inline_stack::{
    Entry, RecordStep, Scope, StackFile,
};
use pulldown_cmark::global_frontier::{VerifiedInlineEmphasisRun, VerifiedInlineLink};
use pulldown_cmark::inline_emphasis::{self, EmphasisBounds, EmphasisStyle};
const RECORD: usize = 64;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
struct Run {
    start: usize,
    count: usize,
    run_length: usize,
    delimiter: u8,
    can_open: bool,
    both: bool,
}

enum Phase {
    Idle,
    Search,
    Consume,
    Remainder,
    Finish,
}

pub(super) struct EmphasisJob {
    source: Range<usize>,
    link: Option<Scope>,
    bounds: EmphasisBounds,
    head: usize,
    depth: usize,
    search: usize,
    lower: usize,
    run: Option<Run>,
    matched: Option<Entry>,
    match_count: usize,
    remaining: usize,
    open_end: usize,
    close_start: usize,
    options: pulldown_cmark::Options,
    phase: Phase,
}

impl EmphasisJob {
    /// 仅为真实顺序delimiter分配固定匹配状态；匿名页仍延迟到第一次opener入栈。
    pub(super) fn new(options: pulldown_cmark::Options, source: Range<usize>) -> Self {
        Self {
            source,
            link: None,
            bounds: EmphasisBounds::default(),
            head: 0,
            depth: 0,
            search: 0,
            lower: 0,
            run: None,
            matched: None,
            match_count: 0,
            remaining: 0,
            open_end: 0,
            close_start: 0,
            options,
            phase: Phase::Idle,
        }
    }

    /// run来自同一个Root游标的最终顺序，HTML/code内容没有进入此栈的入口。
    pub(super) fn start(
        &mut self,
        proof: VerifiedInlineEmphasisRun,
    ) -> Result<(), MarkdownTableRequestError> {
        let scope = self.link.as_ref().map_or(&self.source, |link| &link.label);
        let range = proof.source_range();
        if self.busy()
            || proof.scope_range() != *scope
            || range.start < scope.start
            || range.end > scope.end
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        } // scope 来自原 cursor 的已认证 Root/Link label，不能跨 label 与外层 opener 配对。
        let both = proof.can_open() && proof.can_close();
        self.run = Some(Run {
            start: proof.source_range().start,
            count: proof.source_range().len(),
            run_length: proof.source_range().len(),
            delimiter: proof.delimiter(),
            can_open: proof.can_open(),
            both,
        });
        self.phase = if proof.can_close() {
            self.begin_search();
            Phase::Search
        } else {
            Phase::Finish
        };
        Ok(())
    }

    /// 原 Link proof 先于其 label 发出，停放外层已完成 run 的匹配头，再用同文件开始独立配对。
    /// 没有新的 FD/正文 owner；label 未匹配 opener 在 LinkEnd 被丢弃，绝不泄露到外层。
    pub(super) fn enter_link(
        &mut self,
        proof: &VerifiedInlineLink,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
        file: &mut StackFile,
    ) -> Result<(SemanticWork, bool), MarkdownTableRequestError> {
        let source = proof.source_range();
        let parent = self
            .link
            .as_ref()
            .map_or(&self.source, |scope| &scope.label);
        if self.busy() || source.start < parent.start || source.end > parent.end {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        // 嵌套Image/Link保存的是原外层scope完整匹配头与九个失败下界；只把最顶层留RAM。
        // 页刷写未完成时不改变当前scope，后继仍消费同一proof，绝不再次接纳另一个Begin。
        let previous = if let Some(outer) = self.link.as_ref() {
            match file.append_scope(outer, quota, budget)? {
                RecordStep::Progress { work } => return Ok((work, false)),
                RecordStep::Ready {
                    value,
                    work: measured,
                } => {
                    work = measured;
                    Some(value)
                }
            }
        } else {
            None
        };
        self.link = Some(Scope {
            source,
            label: proof.label_range(),
            head: self.head,
            depth: self.depth,
            bounds: std::mem::take(&mut self.bounds),
            previous,
        });
        self.head = 0;
        self.depth = 0;
        Ok((work, true))
    }

    /// End必须与当前真实owner/label严格相同；只从同文件恢复一个原scope，不搜索祖先或重建AST。
    /// 先成功读取旧checkpoint再移交RAM状态，短读/IO失败不会把部分恢复状态交给后继run。
    pub(super) fn leave_link(
        &mut self,
        source: &Range<usize>,
        label: &Range<usize>,
        budget: usize,
        file: &mut StackFile,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if self.busy()
            || self
                .link
                .as_ref()
                .is_none_or(|scope| &scope.source != source || &scope.label != label)
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        let previous = if let Some(id) = self.link.as_ref().unwrap().previous {
            work.structural_transitions += 3 * RECORD;
            if work.structural_transitions > budget {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            Some(file.read_scope(id)?)
        } else {
            None
        };
        let outer = self.link.take().unwrap();
        self.head = outer.head;
        self.depth = outer.depth;
        self.bounds = outer.bounds;
        self.link = previous;
        Ok(work)
    }

    /// 每次重新匹配都用剩余栈的真实深度和原run_length，不以消费后的count改变mod3规则。
    fn begin_search(&mut self) {
        let run = self.run.as_ref().unwrap();
        self.search = self.head;
        self.lower = self
            .bounds
            .lower_bound(run.delimiter, run.run_length, run.both)
            .min(self.depth);
    }

    /// Source游标只在本run完全处理后继续；不能把中途匹配当成全块样式已经闭合。
    pub(super) fn busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// 一步最多一次record读写或一个真实匹配span；不在量子内遍历整栈或整条巨大delimiter run。
    pub(super) fn advance(
        &mut self,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
        file: &mut StackFile,
    ) -> Result<(SemanticWork, Option<SpanProof>), MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        match self.phase {
            Phase::Idle => {}
            Phase::Search => {
                if self.search == 0 {
                    let run = self.run.as_ref().unwrap();
                    self.bounds
                        .failed(run.delimiter, run.run_length, run.both, self.depth);
                    self.phase = Phase::Finish;
                } else {
                    let entry = file.read(self.search)?;
                    work.structural_transitions += RECORD;
                    let run = self.run.as_ref().unwrap();
                    if entry.depth < self.lower {
                        self.bounds
                            .failed(run.delimiter, run.run_length, run.both, self.depth);
                        self.phase = Phase::Finish;
                    } else if inline_emphasis::matches(
                        run.delimiter,
                        run.run_length,
                        run.both,
                        entry.delimiter,
                        entry.run_length,
                        entry.both,
                    ) {
                        self.head = entry.previous;
                        self.depth = entry.depth;
                        self.bounds.truncate(self.depth);
                        self.match_count = run.count.min(entry.count);
                        self.remaining = self.match_count;
                        self.open_end = entry.start + entry.count;
                        self.close_start = run.start;
                        self.matched = Some(entry);
                        self.phase = Phase::Consume;
                    } else {
                        self.search = entry.previous;
                    }
                }
            }
            Phase::Consume => {
                let count = inline_emphasis::consume_count(self.remaining);
                let run = self.run.as_ref().unwrap();
                let style = inline_emphasis::style(run.delimiter, count, self.options);
                let whole = self.open_end - count..self.close_start + count;
                self.open_end -= count;
                self.close_start += count;
                self.remaining -= count;
                if self.remaining == 0 {
                    self.phase = Phase::Remainder;
                }
                if let Some(styles) = presentation_style(style) {
                    let width = match style {
                        EmphasisStyle::Strong | EmphasisStyle::Strikethrough => 2,
                        _ => 1,
                    };
                    let body = if width <= count {
                        whole.start + width..whole.end - width
                    } else {
                        whole.clone() // 原presentation无法识别这个delimiter宽度时只保style，绝不猜marker。
                    };
                    return Ok((
                        work,
                        Some(SpanProof {
                            whole,
                            body,
                            styles,
                        }),
                    ));
                }
            }
            Phase::Remainder => {
                let entry = *self.matched.as_ref().unwrap();
                if entry.count > self.match_count {
                    self.bounds.before_push(entry.delimiter, self.depth);
                    let stored = file.append(
                        Entry {
                            previous: self.head,
                            depth: self.depth,
                            count: entry.count - self.match_count,
                            ..entry
                        },
                        quota,
                        budget,
                    )?;
                    let (head, measured) = match stored {
                        RecordStep::Progress { work } => return Ok((work, None)),
                        RecordStep::Ready { value, work } => (value, work),
                    };
                    self.head = head;
                    self.depth += 1;
                    work = measured;
                }
                self.matched = None;
                let run = self.run.as_mut().unwrap();
                run.start += self.match_count;
                run.count -= self.match_count;
                if run.count == 0 {
                    self.run = None;
                    self.phase = Phase::Idle;
                } else {
                    self.begin_search();
                    self.phase = Phase::Search;
                }
            }
            Phase::Finish => {
                let run = *self.run.as_ref().unwrap();
                if run.can_open && run.count > 0 {
                    self.bounds.before_push(run.delimiter, self.depth);
                    let stored = file.append(
                        Entry {
                            previous: self.head,
                            depth: self.depth,
                            start: run.start,
                            count: run.count,
                            run_length: run.run_length,
                            delimiter: run.delimiter,
                            both: run.both,
                        },
                        quota,
                        budget,
                    )?;
                    let (head, measured) = match stored {
                        RecordStep::Progress { work } => return Ok((work, None)),
                        RecordStep::Ready { value, work } => (value, work),
                    };
                    self.head = head;
                    self.depth += 1;
                    work = measured;
                }
                self.run = None;
                self.phase = Phase::Idle;
            }
        }
        Ok((work, None))
    }

    /// 原块结束后算法状态不再授予任何span；唯一文件由共同underline事务在最后消费者后关闭。
    pub(super) fn finish(&mut self) {
        self.link = None;
    }
}

/// 只映射原kernel签发的标签类别；Source权限和完整块终点仍由父job保有。
fn presentation_style(style: EmphasisStyle) -> Option<MarkdownPresentationStyles> {
    Some(match style {
        EmphasisStyle::Literal => return None,
        EmphasisStyle::Emphasis => MarkdownPresentationStyles::EMPHASIS,
        EmphasisStyle::Strong => MarkdownPresentationStyles::STRONG,
        EmphasisStyle::Strikethrough => MarkdownPresentationStyles::STRIKETHROUGH,
        EmphasisStyle::Subscript => MarkdownPresentationStyles::SUBSCRIPT,
        // 原kernel只可由启用的选项签发此标签；本产品不启用该扩展，也不将其伪装成普通完成态。
        EmphasisStyle::Superscript => unreachable!("未启用上标扩展"),
    })
}
