//! 一个真实 Root 块的强调匹配栈；记录追加到匿名文件，RAM 只保固定2KiB尾页和当前匹配。

use super::*;
use pulldown_cmark::global_frontier::VerifiedInlineEmphasisRun;
use pulldown_cmark::inline_emphasis::{self, EmphasisBounds, EmphasisStyle};
use std::{fs::File, io, os::fd::IntoRawFd, os::unix::fs::FileExt};

const PAGE: usize = 2048;
const RECORD: usize = 64;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
struct Entry {
    previous: usize,
    depth: usize,
    start: usize,
    count: usize,
    run_length: usize,
    delimiter: u8,
    both: bool,
}

/// 不可变prev链只追加；退栈不改旧record，也不复制或截取历史目录。
struct StackFile {
    file: Option<File>,
    page: Option<Box<[u8; PAGE]>>,
    count: usize,
}
impl StackFile {
    /// 未遇有效opener时不分配页或FD；未知语法不为备用能力占资源。
    fn new() -> Self {
        Self {
            file: None,
            page: None,
            count: 0,
        }
    }

    /// 只在实际换页时写出旧尾页；源正文从不写进这个固定record文件。
    fn append(
        &mut self,
        entry: Entry,
        quota: &MarkdownTableWindowQuota,
    ) -> Result<(usize, SemanticWork), MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL + RECORD,
            ..Default::default()
        };
        let slot = self.count % (PAGE / RECORD);
        if let Some(page) = self.page.as_ref() {
            if slot == 0 {
                if self.file.is_none() {
                    self.file = Some(tempfile::tempfile().map_err(storage_error)?);
                }
                let offset = (self.count * RECORD - PAGE) as u64;
                self.file
                    .as_ref()
                    .unwrap()
                    .write_all_at(page.as_slice(), offset)
                    .map_err(storage_error)?;
                work.structural_transitions += PAGE;
            }
        } else {
            quota
                .admit_metadata(PAGE)
                .map_err(MarkdownTableRequestError::Window)?;
            self.page = Some(Box::new([0; PAGE]));
            work.metadata_bytes = PAGE;
        }
        let bytes = &mut self.page.as_mut().unwrap()[slot * RECORD..(slot + 1) * RECORD];
        for (index, value) in [
            entry.previous,
            entry.depth,
            entry.start,
            entry.count,
            entry.run_length,
        ]
        .into_iter()
        .enumerate()
        {
            bytes[index * 8..index * 8 + 8].copy_from_slice(&(value as u64).to_le_bytes());
        }
        bytes[40] = entry.delimiter;
        bytes[41] = u8::from(entry.both);
        self.count += 1;
        Ok((self.count, work))
    }

    /// 历史只读一个固定record；命中当前尾页时借原页，不建立第二页缓存。
    fn read(&self, index: usize) -> Result<Entry, MarkdownTableRequestError> {
        let offset = (index - 1) * RECORD;
        let mut copied = [0; RECORD];
        let bytes = if offset / PAGE == (self.count - 1) * RECORD / PAGE {
            &self.page.as_ref().unwrap()[offset % PAGE..offset % PAGE + RECORD]
        } else {
            self.file
                .as_ref()
                .unwrap()
                .read_exact_at(&mut copied, offset as u64)
                .map_err(storage_error)?;
            &copied
        };
        let field = |at: usize| {
            usize::try_from(u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())).unwrap()
        };
        Ok(Entry {
            previous: field(0),
            depth: field(8),
            start: field(16),
            count: field(24),
            run_length: field(32),
            delimiter: bytes[40],
            both: bytes[41] != 0,
        })
    }

    /// 成功发布之前显式归还唯一FD；close失败不重试可能已被内核复用的descriptor。
    fn close(&mut self) -> Result<(), MarkdownTableRequestError> {
        self.page = None;
        if let Some(file) = self.file.take() {
            let descriptor = file.into_raw_fd();
            // SAFETY: into_raw_fd移交此匿名文件的唯一close义务；不存在克隆FD或后续读取者。
            if unsafe { libc::close(descriptor) } != 0 {
                return Err(storage_error(io::Error::last_os_error()));
            }
        }
        Ok(())
    }
}

/// OS首错进入原请求的sticky错误槽；无errno的真实短读按IO错误报告，不恢复成空record。
fn storage_error(error: io::Error) -> MarkdownTableRequestError {
    MarkdownTableRequestError::InlineStorageUnavailable(error.raw_os_error().unwrap_or(libc::EIO))
}

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
    file: StackFile,
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
    pub(super) fn new(options: pulldown_cmark::Options) -> Self {
        Self {
            file: StackFile::new(),
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
    pub(super) fn start(&mut self, proof: VerifiedInlineEmphasisRun) {
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
                    let entry = self.file.read(self.search)?;
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
                let entry = self.matched.take().unwrap();
                if entry.count > self.match_count {
                    self.bounds.before_push(entry.delimiter, self.depth);
                    let (head, measured) = self.file.append(
                        Entry {
                            previous: self.head,
                            depth: self.depth,
                            count: entry.count - self.match_count,
                            ..entry
                        },
                        quota,
                    )?;
                    self.head = head;
                    self.depth += 1;
                    work = measured;
                }
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
                let run = self.run.take().unwrap();
                if run.can_open && run.count > 0 {
                    self.bounds.before_push(run.delimiter, self.depth);
                    let (head, measured) = self.file.append(
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
                    )?;
                    self.head = head;
                    self.depth += 1;
                    work = measured;
                }
                self.phase = Phase::Idle;
            }
        }
        Ok((work, None))
    }

    /// 原块真实结束后未匹配opener继续字面；关闭私有文件成功才允许父owner发布有限输出。
    pub(super) fn finish(&mut self) -> Result<(), MarkdownTableRequestError> {
        self.file.close()
    }

    /// 测试只借fd数值观察真实关闭，不克隆文件或取得第二份close归属。
    #[cfg(test)]
    pub(super) fn descriptor_for_test(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.file.file.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// 真实只读FD替换私有测试文件，使后继原分页写入报告系统错误；生产没有此入口。
    #[cfg(test)]
    pub(super) fn replace_file_for_test(&mut self, file: File) {
        self.file.file = Some(file);
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
        EmphasisStyle::Superscript => MarkdownPresentationStyles::SUPERSCRIPT,
    })
}
