//! 一个真实超长标题的行内投影；只保存原语法游标、固定页和两位匿名掩码，不保存标题正文。
//! 原代码/强调/链接事实先全部闭合，再按源序解码可见字符；后继 delimiter 不会改已输出前缀。

use super::*;
use crate::markdown_document_buffer::{
    table_source::{inline_reference::ReferenceLabel, inline_stack::StackFile},
    table_window_request::{
        MarkdownTableRequestError, SemanticWork,
        ordinary_source::semantic::code::emphasis::EmphasisJob,
    },
};
use pulldown_cmark::global_frontier::{
    InlineCodeCursor, InlineCodeStep, InlineLinkStorageRequest, InlineLinkStorageStep,
    InlineReferenceKind, InlineReferenceResolution, InlineTextDecoder, VerifiedGlobalInlineBlock,
    VerifiedInlineCodeSpan, VerifiedInlineLink,
};

pub(super) enum HeadingSource {
    Atx(VerifiedGlobalLeafBlock),
    Setext(VerifiedGlobalInlineBlock),
}

impl HeadingSource {
    /// 范围只来自原闭合标题；ATX closing hashes 和 Setext underline 永不进入行内正文。
    fn content(&self) -> Result<Range<usize>, Error> {
        match self {
            Self::Atx(leaf) => match leaf.details() {
                LeafBlockDetails::AtxHeading { trim_target, .. } => {
                    Ok(leaf.physical_start()..*trim_target)
                }
                _ => Err(Error::UnsupportedInline),
            },
            Self::Setext(block) => {
                Ok(block.physical_range().start..block.terminal_eol_range().start)
            }
        }
    }

    fn options(&self) -> pulldown_cmark::Options {
        match self {
            Self::Atx(leaf) => leaf.options(),
            Self::Setext(block) => block.options(),
        }
    }

    fn scope(&self) -> Result<Range<usize>, Error> {
        // 强调匹配域从原真实正文开始；物理前缀不参与delimiter匹配，却仍在mask的Source坐标域内。
        // 两个范围各有职责，不能为省坐标转换把quote/list标记送进可见标题或强调栈。
        match self {
            Self::Atx(leaf) => match leaf.details() {
                LeafBlockDetails::AtxHeading {
                    lexical_content,
                    trim_target,
                    ..
                } => Ok(lexical_content.start..*trim_target),
                _ => Err(Error::UnsupportedInline),
            },
            Self::Setext(block) => Ok(block.source_range().start..block.terminal_eol_range().start),
        }
    }

    fn source_end(&self) -> usize {
        match self {
            Self::Atx(leaf) => leaf.source_end(),
            Self::Setext(block) => block.source_end(),
        }
    }
}

struct Fill {
    range: Range<usize>,
    flags: u8,
}

/// 每四个源字节共用一个mask字节：bit0隐藏、bit1代码字面；隐藏始终优先于代码字面。
/// 匿名文件按实际首个标记才创建；全plain标题没有第二个文件、正文拷贝或全范围清零。
struct Mask {
    file: Option<File>,
    source: Range<usize>,
    pending: [Option<Fill>; 6],
    index: usize,
}

impl Mask {
    fn new(source: Range<usize>) -> Self {
        // 源域只留原Fact的两个偏移；文件创建和稀疏长度设置推迟到第一条真实隐藏/字面事实。
        // 因而巨大的纯文本标题也不会分配与长度成正比的mask内存或创建空白输出目录。
        Self {
            file: None,
            source,
            pending: std::array::from_fn(|_| None),
            index: 0,
        }
    }

    /// 本次只保一个真实语义事实的至多六段；未写完之前不能接收后继事实覆盖它。
    fn begin(&mut self, fills: impl IntoIterator<Item = (Range<usize>, u8)>) -> Result<(), Error> {
        if self.busy() {
            return Err(Error::UnsupportedInline);
        }
        // 一条proof可能含opener、body、closer及两端trim；固定六槽覆盖这一个事实而非全部标题。
        // 上一事实未写完就不能覆盖槽位，后继语义读取必须等待真实文件写入完成。
        self.pending = std::array::from_fn(|_| None);
        self.index = 0;
        for (index, (range, flags)) in fills.into_iter().enumerate() {
            if index >= self.pending.len()
                || range.start < self.source.start
                || range.end > self.source.end
                || range.start > range.end
                || !(1..=2).contains(&flags)
            {
                return Err(Error::UnsupportedInline);
            }
            self.pending[index] = Some(Fill { range, flags });
        }
        Ok(())
    }

    fn busy(&self) -> bool {
        self.index < self.pending.len() && self.pending[self.index].is_some()
    }

    /// 一步只读改写一个1KiB页；稀疏零页是已证明普通源，无需按标题长度预分配RAM或写零。
    fn advance(&mut self) -> Result<MarkdownHeadingAnchorWork, Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        let fill = self.pending[self.index]
            .as_mut()
            .ok_or(Error::UnsupportedInline)?;
        // 空区间也是原事实的合法边界；只消耗该槽，不构造一次空pwrite或假装新内容已提交。
        if fill.range.is_empty() {
            self.index += 1;
            return Ok(work);
        }
        if self.file.is_none() {
            let file = tempfile::tempfile().map_err(storage)?;
            file.set_len(self.source.len().div_ceil(4) as u64)
                .map_err(storage)?;
            self.file = Some(file);
            return Ok(work);
        }
        // 页内按mask字节合并四个Source字节的2-bit标记；OR只增加已证权限，隐藏永远覆盖字面位。
        // 当前范围跨页时仅移动其已写起点，下一量子继续同一份原事实，不能提前接下一个token。
        let first = fill.range.start - self.source.start;
        let last = (first / 4 + PAGE) * 4;
        let through = (fill.range.end - self.source.start).min(last);
        let offset = first / 4;
        let count = through.div_ceil(4) - offset;
        let mut bytes = [0; PAGE];
        let file = self.file.as_ref().unwrap();
        file.read_exact_at(&mut bytes[..count], offset as u64)
            .map_err(storage)?;
        for (index, byte) in bytes[..count].iter_mut().enumerate() {
            let start = (offset + index) * 4;
            let low = first.saturating_sub(start).min(4);
            let high = through.saturating_sub(start).min(4);
            let mask = ((1u16 << (high * 2)) - 1) & !((1u16 << (low * 2)) - 1);
            *byte |= fill.flags * 0x55 & mask as u8;
        }
        file.write_all_at(&bytes[..count], offset as u64)
            .map_err(storage)?;
        fill.range.start = self.source.start + through;
        work.storage_bytes = count * 2;
        work.structural_transitions += count + 8;
        if fill.range.is_empty() {
            self.index += 1;
        }
        Ok(work)
    }

    /// 只读当前源小页的2-bit事实；没有文件就是真plain，不查询或创建默认文件。
    fn flags(&self, start: usize, count: usize, output: &mut [u8]) -> Result<usize, Error> {
        // 未写过的稀疏位置保普通文本语义；读取只物化这次至多256个Source字节对应的65字节。
        // 文件页与原正文页不共用所有权，取消时关闭mask不会触碰用户Source或原文件。
        output[..count].fill(0);
        let Some(file) = self.file.as_ref() else {
            return Ok(0);
        };
        let first = start - self.source.start;
        let offset = first / 4;
        let length = (first + count).div_ceil(4) - offset;
        let mut bytes = [0; 65];
        if length > bytes.len() {
            return Err(Error::UnsupportedInline);
        }
        file.read_exact_at(&mut bytes[..length], offset as u64)
            .map_err(storage)?;
        for (index, flag) in output[..count].iter_mut().enumerate() {
            let position = first + index;
            *flag = bytes[position / 4 - offset] >> ((position % 4) * 2) & 3;
        }
        Ok(length)
    }

    fn close(&mut self) -> Result<(), Error> {
        self.pending = std::array::from_fn(|_| None);
        if let Some(file) = self.file.take() {
            let descriptor = file.into_raw_fd();
            // SAFETY: 本匿名文件只有本owner；转移后恰好close一次，不重试可能已复用的descriptor。
            if unsafe { libc::close(descriptor) } != 0 {
                return Err(storage(io::Error::last_os_error()));
            }
        }
        Ok(())
    }
}

pub(super) struct HeadingProjection {
    initial: Option<HeadingSource>,
    cursor: Option<Box<InlineCodeCursor>>,
    source: Range<usize>,
    emphasis: EmphasisJob,
    stack: StackFile,
    quota: MarkdownTableWindowQuota,
    mask: Mask,
    storage: Option<InlineLinkStorageRequest>,
    link: Option<VerifiedInlineLink>,
    reference: Option<Box<ReferenceLabel>>,
    references: Option<Box<HeadingReferences>>,
    footnotes: Option<Box<HeadingFootnotes>>,
    decoded: bool,
    next: usize,
    decoder: InlineTextDecoder,
    flushing: bool,
    // 代码正文不解实体/反斜杠；这里只跨源页保最多三个不完整UTF8字节。
    literal: [u8; 4],
    literal_len: usize,
    // CRLF跨物理页仍只产生一个可见空格；两个字节都来自已认证正文，不能把片尾当换行终点。
    literal_after_cr: bool,
    normal_after_cr: bool,
    closed: bool,
}

fn semantic(work: SemanticWork) -> MarkdownHeadingAnchorWork {
    MarkdownHeadingAnchorWork {
        source_bytes: work.source_bytes,
        copied_bytes: work.copied_source_bytes,
        parser_input_bytes: work.parser_input_bytes,
        metadata_bytes: work.metadata_bytes,
        structural_transitions: work.structural_transitions + work.source_tree_steps,
        ..Default::default()
    }
}

impl HeadingProjection {
    pub(super) fn new(source: HeadingSource) -> Result<Self, Error> {
        // content是原物理Source域，scope是原inline匹配域；前缀只隐藏一次，不重建一份去前缀正文。
        // quota沿现有语义外存归属，固定栈尾页与实际新结构页按真实获取量准入。
        let range = source.content()?;
        let scope = source.scope()?;
        let options = source.options();
        let fuel = source
            .source_end()
            .checked_mul(4096)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(Error::ArithmeticOverflow)?;
        let mut mask = Mask::new(range.clone());
        if range.start < scope.start {
            mask.begin([(range.start..scope.start, 1)])?;
        }
        Ok(Self {
            initial: Some(source),
            cursor: None,
            source: range.clone(),
            emphasis: EmphasisJob::new(options, scope),
            stack: StackFile::new(),
            quota: MarkdownTableWindowQuota::new(1024, fuel),
            mask,
            storage: None,
            link: None,
            reference: None,
            references: None,
            footnotes: None,
            decoded: false,
            next: range.start,
            decoder: InlineTextDecoder::default(),
            flushing: false,
            literal: [0; 4],
            literal_len: 0,
            literal_after_cr: false,
            normal_after_cr: false,
            closed: false,
        })
    }

    /// 原 Source 上一个游标独占处理全部语法；mask写入和原匹配栈暂停时不会继续读取后继源。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        slugs: &mut Slugs,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        if self.closed {
            return Ok((work, true));
        }
        // 首个量子只把原Fact移交给原共享cursor；不能在构造函数同步扫描长标题或拿短Parser探路。
        // Box的真实固定容量单列，后续所有lookahead、storage和reference仍归这一个cursor。
        if let Some(source) = self.initial.take() {
            let fuel = document
                .byte_len()
                .checked_mul(4096)
                .and_then(|n| n.checked_add(1024 * 1024))
                .ok_or(Error::ArithmeticOverflow)?;
            let limits = InlinePlainLimits {
                max_byte_work: fuel,
                max_transitions: fuel,
            };
            let cursor = match source {
                HeadingSource::Atx(leaf) => leaf.heading_code_cursor(limits),
                HeadingSource::Setext(block) => block.into_heading_code_cursor(limits),
            }
            .map_err(Error::Global)?;
            let capacity = std::mem::size_of::<InlineCodeCursor>();
            self.quota
                .admit_metadata(capacity)
                .map_err(|error| Error::Semantic(MarkdownTableRequestError::Window(error)))?;
            work.metadata_bytes = capacity;
            self.cursor = Some(Box::new(cursor));
            return Ok((work, false));
        }
        // 文件事实先落完，再接新的源事件；匹配栈暂停同样先恢复，Source页不会跨这些I/O被持有。
        // 这个优先级防止后继proof覆盖当前待写range，也把每个量子的真实资源工作留在单一边界。
        if self.mask.busy() {
            return self.mask.advance().map(|work| (work, false));
        }
        if self.emphasis.busy() {
            let (measured, span) = self
                .emphasis
                .advance(&self.quota, budget, &mut self.stack)
                .map_err(Error::Semantic)?;
            // matched span由原EmphasisJob签发；这里只隐藏真实delimiter，正文仍等源序输出时再借页。
            // 没配对的run从未获得隐藏权限，下划线等字面内容会按原Source保留。
            if let Some(span) = span {
                self.mask.begin([
                    (span.whole.start..span.body.start, 1),
                    (span.body.end..span.whole.end, 1),
                ])?;
            }
            return Ok((semantic(measured), false));
        }
        // 原Begin先保存外层强调头再进入完整label域；入栈需要flush时保同一proof继续，不能重复Begin。
        // URL与title范围不进入label强调栈，也不复制成另一份可见文本。
        if let Some(proof) = self.link.take() {
            let (measured, entered) = self
                .emphasis
                .enter_link(&proof, &self.quota, budget, &mut self.stack)
                .map_err(Error::Semantic)?;
            if !entered {
                self.link = Some(proof);
            }
            return Ok((semantic(measured), false));
        }
        // 真实StackFile回复只恢复原暂停操作；Progress不是append/read已完成，不能提前发布链接。
        // metadata来自实际尾页分配，转换/flush费用已由回复携带，accept只转交而不双计工作。
        if let Some(request) = self.storage.as_ref() {
            let (step, metadata) = self
                .stack
                .storage(request, &self.quota, budget)
                .map_err(Error::Semantic)?;
            let ready = matches!(&step, InlineLinkStorageStep::Ready { .. });
            let measured = self
                .cursor
                .as_mut()
                .unwrap()
                .accept_storage(
                    request,
                    step,
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                    budget,
                )
                .map_err(Error::Global)?;
            if ready {
                self.storage = None;
            }
            work.structural_transitions += measured.transitions;
            work.metadata_bytes += metadata;
            return Ok((work, false));
        }
        if self.reference.is_some() {
            return self.advance_reference(document, budget);
        }
        if self.decoded {
            return self.render(document, slugs, budget);
        }
        let cursor = self.cursor.as_mut().ok_or(Error::UnsupportedInline)?;
        // Code/HTML/link前探会改变真正所需源位置，始终借cursor.next_byte，不沿假定的顺序offset推进。
        // 任何后继文件操作或错误展开之前都结束这个lease；Doc仍独占mmap健康和回收责任。
        let frame = document.table_source_frame(cursor.next_byte(), 256)?;
        let actual_read = frame.bytes.len();
        let step = cursor
            .advance(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                GlobalSourceAuthority {
                    current_revision: document.generation(),
                    cancelled: false,
                },
                budget - 128,
            )
            .map_err(Error::Global);
        frame.finish()?;
        let step = step?;
        let measured = match &step {
            InlineCodeStep::Progress { work }
            | InlineCodeStep::NeedInput { work }
            | InlineCodeStep::Span { work, .. }
            | InlineCodeStep::Prefix { work, .. }
            | InlineCodeStep::Emphasis { work, .. }
            | InlineCodeStep::Link { work, .. }
            | InlineCodeStep::Storage { work, .. }
            | InlineCodeStep::Reference { work, .. }
            | InlineCodeStep::LinkEnd { work, .. }
            | InlineCodeStep::Proven { work, .. }
            | InlineCodeStep::NeedsGeneralInline { work } => *work,
        };
        work.source_bytes = actual_read;
        work.structural_transitions += measured.transitions;
        // 三种额外事实每次至多一条：已解析脚注、原行末空白、已闭合HTML/autolink。
        // 它们都由同一源序cursor交付，不从样式、首个'>'或文本搜索补造可见范围。
        if let Some(footnote) = cursor.take_heading_footnote() {
            self.mask.begin([(footnote.source_range(), 1)])?;
        }
        if let Some(trim) = cursor.take_heading_line_trim() {
            self.mask.begin([(trim.source_range(), 1)])?;
        }
        if let Some(html) = cursor.take_heading_html() {
            let range = html.source_range();
            if html.is_autolink() {
                // autolink的显示label是原URL字面，不能再把其中'&amp;'当正文实体解码。
                // 两侧尖括号隐藏，内部仍由固定UTF8 carry跨真实源页读取。

                self.mask.begin([
                    (range.start + 1..range.end - 1, 2),
                    (range.start..range.start + 1, 1),
                    (range.end - 1..range.end, 1),
                ])?;
            } else {
                self.mask.begin([(range, 1)])?;
            }
        }
        match step {
            InlineCodeStep::Span { proof, .. } => {
                work.source_bytes += self.code(document, proof)?;
            }
            InlineCodeStep::Prefix { proof, .. } => self.mask.begin([(proof.source_range(), 1)])?,
            InlineCodeStep::Emphasis { run, .. } => {
                self.emphasis.start(run).map_err(Error::Semantic)?
            }
            InlineCodeStep::Link { proof, .. } => {
                let source = proof.source_range();
                let label = proof.label_range();
                self.mask
                    .begin([(source.start..label.start, 1), (label.end..source.end, 1)])?;
                self.link = Some(proof);
            }
            InlineCodeStep::LinkEnd { source, label, .. } => {
                let measured = self
                    .emphasis
                    .leave_link(&source, &label, budget, &mut self.stack)
                    .map_err(Error::Semantic)?;
                work.structural_transitions += measured.structural_transitions;
            }
            InlineCodeStep::Storage { request, .. } => self.storage = Some(request),
            InlineCodeStep::Reference { request, .. } => {
                self.reference = Some(Box::new(ReferenceLabel::new(request)))
            }
            InlineCodeStep::Proven { .. } => {
                // 真实inline域已闭合，后文不再能回改delimiter/link决定；此时才授权开始输出。
                // 外存匹配历史立即显式close，渲染阶段只保mask、slug文件和固定小页。

                self.decoded = true;
                self.cursor = None;
                self.emphasis.finish();
                self.stack.close().map_err(Error::Semantic)?; // 匹配完成即归还语法外存，不等最终导航才关闭。
            }
            InlineCodeStep::NeedsGeneralInline { .. } => return Err(Error::UnsupportedInline),
            _ => {}
        }
        Ok((work, false))
    }

    /// 代码 literal 与原 trim 证明分别落mask；只窥原body首尾两个字节判断真实CRLF宽度。
    fn code(
        &mut self,
        document: &MarkdownDocumentBuffer,
        proof: VerifiedInlineCodeSpan,
    ) -> Result<usize, Error> {
        let whole = proof.source_range();
        let body = proof.raw_body_range();
        let mut start = body.start;
        let mut end = body.end;
        if proof.trims_boundary_spaces() {
            // trim标志由原code匹配器给出；首尾最多各窥两byte，只区别真实CRLF与单个归一空格。
            // 全空格代码不会进入这个分支，不能擅自把它的全部可见空白删掉。

            start += self.eol_width(document, body.start, (body.start + 2).min(body.end))?;
            let tail = body.end.saturating_sub(2).max(body.start);
            let mut bytes = [0; 2];
            read_source(document, tail, &mut bytes[..body.end - tail])?;
            end -= if &bytes[..body.end - tail] == b"\r\n" {
                2
            } else {
                1
            };
        }
        self.mask.begin([
            (body.clone(), 2),
            (whole.start..body.start, 1),
            (body.end..whole.end, 1),
            (body.start..start, 1),
            (end..body.end, 1),
        ])?;
        Ok(if proof.trims_boundary_spaces() {
            body.len().min(2) * 2
        } else {
            0
        })
    }

    fn eol_width(
        &self,
        document: &MarkdownDocumentBuffer,
        start: usize,
        end: usize,
    ) -> Result<usize, Error> {
        let mut bytes = [0; 2];
        read_source(document, start, &mut bytes[..end - start])?;
        Ok(if &bytes[..end - start] == b"\r\n" {
            2
        } else {
            1
        })
    }

    /// 每次只补当前一个已认证identifier；查询仍沿原 Global 及原有限定义解析器，缺失须真实EOF。
    fn advance_reference(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let reference = self.reference.as_mut().unwrap();
        if reference.label().is_none() {
            return reference
                .advance(document, &self.quota, budget)
                .map(|work| (semantic(work), false))
                .map_err(Error::Semantic);
        }
        if reference.request.kind() == InlineReferenceKind::Footnote {
            // 脚注只查存在性；普通reference仍查真实URL/title，两个业务答案不借假URL混用。
            // 查询运行时保原cursor暂停身份，Found/Missing准备完才恢复同一个request。

            if self.footnotes.is_none() {
                let mut footnotes = HeadingFootnotes::new(self.quota.clone());
                footnotes.lookup(reference.label().unwrap());
                if let Some(error) = footnotes.take_failure() {
                    return Err(error);
                }
                self.footnotes = Some(Box::new(footnotes));
                return Ok((
                    MarkdownHeadingAnchorWork {
                        structural_transitions: 1,
                        ..Default::default()
                    },
                    false,
                ));
            }
            let (work, complete) = self.footnotes.as_mut().unwrap().advance(document, budget)?;
            if complete {
                let found = self
                    .footnotes
                    .as_mut()
                    .unwrap()
                    .lookup(reference.label().unwrap())
                    == Some(true);
                self.cursor
                    .as_mut()
                    .unwrap()
                    .accept_reference(
                        &reference.request,
                        if found {
                            InlineReferenceResolution::Resolved
                        } else {
                            InlineReferenceResolution::Missing
                        },
                        GlobalSourceAuthority {
                            current_revision: document.generation(),
                            cancelled: false,
                        },
                    )
                    .map_err(Error::Global)?;
                self.footnotes = None;
                self.reference = None;
            }
            return Ok((work, false));
        }
        if self.references.is_none() {
            let mut queries = MarkdownTableReferenceQueries::for_batch(Some(self.quota.clone()));
            queries
                .collect_verified_label(reference.label().unwrap())
                .map_err(|error| Error::Semantic(MarkdownTableRequestError::Window(error)))?;
            self.references = Some(Box::new(HeadingReferences::new(document, queries)?));
            return Ok((
                MarkdownHeadingAnchorWork {
                    structural_transitions: 1,
                    ..Default::default()
                },
                false,
            ));
        }
        let (work, complete) = self
            .references
            .as_mut()
            .unwrap()
            .advance(document, budget)?;
        if complete {
            let resolved = self
                .references
                .as_ref()
                .unwrap()
                .resolve(reference.label().unwrap())
                .is_some();
            self.cursor
                .as_mut()
                .unwrap()
                .accept_reference(
                    &reference.request,
                    if resolved {
                        InlineReferenceResolution::Resolved
                    } else {
                        InlineReferenceResolution::Missing
                    },
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                )
                .map_err(Error::Global)?;
            self.references = None;
            self.reference = None;
        }
        Ok((work, false))
    }

    /// 所有语义已闭合后才按原源序输出；更长的最终base永不命中有限fragment，可直接停止输出。
    fn render(
        &mut self,
        document: &MarkdownDocumentBuffer,
        slugs: &mut Slugs,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        // 语义证明已经完成。此时base越过目标长度，后续唯一化只会加长，不再可能命中fragment。
        // 可以结束剩余输出并关闭文件，不能把这个剪枝提前到尚未闭合的原字面扫描阶段。
        if slugs.irrelevant {
            self.close()?;
            return Ok((work, true));
        }
        // 归一化器的carry属于当前普通文本域；hidden/code边界先排出它，不能跨markup凑出新实体。
        // 真Source末端还要等最后完整UTF8输出排完，complete不能由一次空输出或短片推断。
        if self.flushing || self.next == self.source.end {
            let mut output = [0; 256];
            let step = self
                .decoder
                .feed(&[], true, &mut output, budget)
                .map_err(Error::Global)?;
            let text = std::str::from_utf8(&output[..step.written])
                .map_err(|_| Error::UnsupportedInline)?;
            let (writes, copied) = slugs.append_visible(text)?;
            work.storage_bytes = writes;
            work.copied_bytes = copied;
            work.structural_transitions += step.transitions;
            if step.complete {
                self.flushing = false;
                self.decoder = InlineTextDecoder::default();
                if self.next == self.source.end {
                    self.close()?;
                    return Ok((work, true));
                }
            }
            return Ok((work, false));
        }
        let count = (self.source.end - self.next).min(256);
        let mut flags = [0; 256];
        work.storage_bytes += self.mask.flags(self.next, count, &mut flags)?;
        let flag = flags[0];
        let mut group = flags[..count]
            .iter()
            .position(|&other| other != flag)
            .unwrap_or(count);
        if flag & 1 != 0 {
            // Hidden边界先结束前一个已证普通文本域，避免把跨markup的'&am'+'p;'误解成实体。
            self.next += group;
            self.flushing = true;
            self.literal_after_cr = false;
            self.normal_after_cr = false;
            if self.literal_len != 0 {
                return Err(Error::UnsupportedInline);
            }
            return Ok((work, false));
        }
        let mut bytes = [0; 260];
        read_source(document, self.next, &mut bytes[..group])?;
        work.source_bytes = group;
        work.copied_bytes = group;
        if flag & 2 != 0 {
            // 原code span只把真实CR/LF归一成空格，不解码实体；UTF8尾部由固定carry衔接。
            bytes.copy_within(..group, self.literal_len);
            bytes[..self.literal_len].copy_from_slice(&self.literal[..self.literal_len]);
            let length = group + self.literal_len;
            let valid = match std::str::from_utf8(&bytes[..length]) {
                Ok(_) => length,
                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                Err(_) => return Err(Error::UnsupportedInline),
            };
            self.literal_len = length - valid;
            self.literal[..self.literal_len].copy_from_slice(&bytes[valid..length]);
            let text =
                std::str::from_utf8(&bytes[..valid]).map_err(|_| Error::UnsupportedInline)?;
            // 与原code span相同：CR、LF及CRLF均为一个空格；保状态而不复制整段code。
            let mut text_bytes = String::with_capacity(valid);
            for scalar in text.chars() {
                if scalar == '\n' && self.literal_after_cr {
                    self.literal_after_cr = false;
                    continue;
                }
                self.literal_after_cr = scalar == '\r';
                text_bytes.push(if matches!(scalar, '\r' | '\n') {
                    ' '
                } else {
                    scalar
                });
            }
            let (writes, copied) = slugs.append_visible(&text_bytes)?;
            self.next += group;
            work.storage_bytes += writes;
            work.copied_bytes += copied;
            return Ok((work, false));
        }
        // 完整事实的域边界必须落在合法UTF8边界；代码carry不得泄到普通文本或另一条code span。
        // 有问题就沿原失败链清理，不能用替换字符掩盖Source坐标或解码错误。
        if self.literal_len != 0 {
            return Err(Error::UnsupportedInline);
        }
        self.literal_after_cr = false;
        if self.normal_after_cr && bytes[0] == b'\n' {
            // 前一步的真实CR已送出空格；这次只消费属于同一CRLF的LF，不建立第二个空格。
            self.next += 1;
            self.normal_after_cr = false;
            return Ok((work, false));
        }
        self.normal_after_cr = false;
        if let Some(end) = bytes[..group]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
        {
            // 只在真实EOL截小当前页，输出/输入长度仍一致，decoder消费数仍是canonical字节数。
            group = end + 1;
            bytes[end] = b' ';
        }
        let real_end = if group < count || self.next + group == self.source.end {
            true
        } else {
            let mut next = [0];
            self.mask.flags(self.next + group, 1, &mut next)?;
            next[0] != 0
        };
        let mut output = [0; 256];
        let step = self
            .decoder
            .feed(&bytes[..group], real_end, &mut output, budget)
            .map_err(Error::Global)?;
        // 消费可能因输出页满而早停；只有真消费到末尾CR时才持有下一LF的归一化责任。
        if step.consumed == group {
            let mut last = [0];
            read_source(document, self.next + group - 1, &mut last)?;
            work.source_bytes += 1;
            self.normal_after_cr = last[0] == b'\r';
        }
        self.next += step.consumed;
        self.flushing = real_end && !step.complete;
        if step.complete {
            self.decoder = InlineTextDecoder::default();
        }
        let text =
            std::str::from_utf8(&output[..step.written]).map_err(|_| Error::UnsupportedInline)?;
        let (writes, copied) = slugs.append_visible(text)?;
        work.storage_bytes += writes;
        work.copied_bytes += copied;
        work.structural_transitions += step.transitions;
        Ok((work, false))
    }

    /// 各真实资源都尝试归还；两个关闭错误同样保留，不能第一个Err提前跳过另一个FD。
    pub(super) fn close(&mut self) -> Result<(), Error> {
        self.initial = None;
        self.cursor = None;
        self.reference = None;
        self.references = None;
        self.footnotes = None;
        self.storage = None;
        self.link = None;
        self.closed = true;
        let stack = self.stack.close().map_err(Error::Semantic);
        let mask = self.mask.close();
        match (stack, mask) {
            (Err(primary), Err(cleanup)) => Err(Error::Cleanup {
                primary: Arc::new(primary),
                cleanup: Arc::new(cleanup),
            }),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }
}

/// 只合并当前至多260字节的真实逻辑片；全部lease逐个finish，UTF8可以跨piece而不造EOF。
fn read_source(
    document: &MarkdownDocumentBuffer,
    start: usize,
    output: &mut [u8],
) -> Result<(), Error> {
    let mut copied = 0;
    while copied < output.len() {
        let frame = document.table_source_frame(start + copied, output.len() - copied)?;
        let count = frame.bytes.len();
        output[copied..copied + count].copy_from_slice(&frame.bytes);
        copied += count;
        frame.finish()?;
        if count == 0 {
            return Err(Error::UnsupportedInline);
        }
    }
    Ok(())
}
