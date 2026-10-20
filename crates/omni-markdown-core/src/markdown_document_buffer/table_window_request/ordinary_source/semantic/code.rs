//! 超长 Root 段落的原 code 证明与可见交集；正文只借 mmap 小帧，输出库存只随当前 viewport 增长。

use super::*;
use pulldown_cmark::global_frontier::{
    InlineCodeCursor, InlineCodeStep, VerifiedInlineEmphasisRun, VerifiedInlineLink,
};
mod emphasis;
mod link;
mod prefix;
use emphasis::EmphasisJob;
use link::LinkJob;
pub(in super::super) use prefix::PrefixDecoration;

#[derive(Eq, PartialEq)]
struct VisibleSpan {
    whole: Range<usize>,
    body: Range<usize>,
    active: bool,
    styles: MarkdownPresentationStyles,
}

/// 单个实际code证明或原强调kernel真实匹配的范围；从不收集整块历史range目录。
pub(in super::super) struct SpanProof {
    pub(in super::super) whole: Range<usize>,
    pub(in super::super) body: Range<usize>,
    pub(in super::super) styles: MarkdownPresentationStyles,
}

pub(super) struct CodeJob {
    cursor: Option<InlineCodeCursor>,
    pending: Option<SpanProof>,
    coordinates: Option<Coordinates>,
    spans: Vec<VisibleSpan>,
    coverage: Range<usize>,
    requested: Range<usize>,
    pub(super) output: Vec<Option<MarkdownPresentationRun>>,
    pub(super) general: bool,
    done: bool,
    emphasis: Option<Box<EmphasisJob>>,
    incoming: Option<VerifiedInlineEmphasisRun>,
    incoming_link: Option<VerifiedInlineLink>,
    link: Option<Box<LinkJob>>,
    pub(super) links: Vec<MarkdownSourceLink>,
    target_bytes: usize,
    options: pulldown_cmark::Options,
    // 强调的外层域来自原 seed；Link label 会临时成为独立内层域，不用视口边缘代替语法边界。
    source_range: Range<usize>,
    visible: Option<VisibleSpan>,
    visible_index: usize,
    query: Option<MarkdownPresentationQuery>,
    query_index: usize,
    query_cached: Option<MarkdownPresentationQuery>,
    containers: MarkdownPresentationStyles,
    prefix: Option<PrefixDecoration>,
    // 同一来源页按原源序只前进；每条 prefix 不能从零重扫所有已证明的 Origin。
    prefix_record_index: usize,
}

impl CodeJob {
    /// 只接原闭合 Root Paragraph；完整 Setext 标记/容器仍须其自身 origin 规则，不造普通段落权限。
    pub(super) fn new(
        block: &OrdinaryBlockRecord,
        origin: Option<&BlockOriginLine>,
        coverage: Range<usize>,
        requested: Range<usize>,
        limits: (usize, usize),
        quota: &MarkdownTableWindowQuota,
    ) -> Result<Option<Box<Self>>, MarkdownTableRequestError> {
        if !matches!(
            block.seed.kind(),
            InlineBlockKind::Paragraph | InlineBlockKind::TableCell
        ) {
            return Ok(None);
        }
        let limits = InlinePlainLimits {
            max_byte_work: limits.0,
            max_transitions: limits.1,
        };
        let cursor = match origin {
            Some(origin) => block
                .seed
                .emphasis_origin_cursor(origin, coverage.end, limits),
            None => block.seed.emphasis_cursor(coverage.end, limits),
        }
        .map_err(MarkdownTableRequestError::Global)?;
        let Some(cursor) = cursor else {
            return Ok(None);
        };
        quota
            .admit_metadata(std::mem::size_of::<Self>())
            .map_err(MarkdownTableRequestError::Window)?;
        Ok(Some(Box::new(Self {
            cursor: Some(cursor),
            pending: None,
            coordinates: None,
            spans: Vec::new(),
            coverage,
            requested,
            output: Vec::new(),
            general: false,
            done: false,
            emphasis: None,
            incoming: None,
            incoming_link: None,
            link: None,
            links: Vec::new(),
            target_bytes: 0,
            options: block.seed.options(),
            source_range: block.seed.source_range(),
            visible: None,
            visible_index: 0,
            query: None,
            query_index: 0,
            query_cached: None,
            containers: MarkdownPresentationStyles::default(),
            prefix: None,
            prefix_record_index: 0,
        })))
    }

    /// code probe 真实越过视口寻找 closer；只有已闭合 proof 的可见端点才进行有限 UTF16 转换。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        selection: &Range<usize>,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<(SemanticWork, bool), MarkdownTableRequestError> {
        if self.done {
            return Ok((SemanticWork::default(), true));
        }
        if let Some(span) = self.visible.as_ref() {
            let mut work = SemanticWork {
                structural_transitions: PLAIN_CONTROL,
                ..Default::default()
            };
            while self.visible_index < self.spans.len()
                && work.structural_transitions + std::mem::size_of::<VisibleSpan>() <= budget
            {
                work.structural_transitions += std::mem::size_of::<VisibleSpan>();
                if &self.spans[self.visible_index] == span {
                    self.visible = None;
                    return Ok((work, false));
                }
                self.visible_index += 1;
            }
            if self.visible_index == self.spans.len() {
                quota
                    .admit_metadata(std::mem::size_of::<VisibleSpan>())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.spans.reserve_exact(1);
                self.spans.push(self.visible.take().unwrap());
                work.metadata_bytes += std::mem::size_of::<VisibleSpan>();
            }
            return Ok((work, false));
        }
        if let Some(proof) = self.pending.as_ref() {
            if let Some(coordinates) = self.coordinates.as_mut() {
                let (work, result) = coordinates.advance(document, budget)?;
                if let Some(values) = result {
                    let span = VisibleSpan {
                        whole: values[0]..values[3],
                        body: values[1]..values[2],
                        active: markdown_selection_intersects(selection, &proof.whole),
                        styles: proof.styles,
                    };
                    self.visible = Some(span); // 去重必须在后继有限量子逐项进行，不能隐藏一次无界Vec遍历。
                    self.visible_index = 0;
                    self.coordinates = None;
                    self.pending = None;
                }
                return Ok((work, false));
            }
            let whole = &proof.whole;
            let body = &proof.body;
            let clamp = |at: usize| at.clamp(self.coverage.start, self.coverage.end);
            // 远处 opener/closer 只取 AVL 计量后的可见交集，绝不解码两端之间的整个 giant code body。
            let (coordinates, work) = Coordinates::new(
                document,
                [
                    clamp(whole.start),
                    clamp(body.start),
                    clamp(body.end),
                    clamp(whole.end),
                ],
                budget,
            )?;
            self.coordinates = Some(coordinates);
            return Ok((work, false));
        }
        if let Some(prefix) = self.prefix.as_mut() {
            // 此流式 kernel 仍只接已消费的 Quote/List/Item；Footnote 先走有真实 opening 的短块入口。
            let (work, marker, done) = prefix.advance(document, budget, &[])?;
            self.pending = marker;
            if done {
                self.prefix = None;
            }
            return Ok((work, false));
        }
        if let Some(proof) = self.incoming_link.take() {
            if self.emphasis.is_none() {
                quota
                    .admit_metadata(std::mem::size_of::<EmphasisJob>())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.emphasis = Some(Box::new(EmphasisJob::new(
                    self.options,
                    self.source_range.clone(),
                )));
                self.incoming_link = Some(proof);
                return Ok((
                    SemanticWork {
                        metadata_bytes: std::mem::size_of::<EmphasisJob>(),
                        structural_transitions: PLAIN_CONTROL,
                        ..Default::default()
                    },
                    false,
                ));
            }
            self.emphasis.as_mut().unwrap().enter_link(&proof)?;
            let source = proof.source_range();
            let metadata = if source.start < self.coverage.end && self.coverage.start < source.end {
                quota
                    .admit_metadata(std::mem::size_of::<LinkJob>())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.link = Some(Box::new(LinkJob::new(
                    proof,
                    crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_REFERENCE_BYTES
                        .saturating_sub(self.target_bytes),
                )));
                std::mem::size_of::<LinkJob>()
            } else {
                0 // 离屏 label 仍隔离强调，但不为它复制 URL 或创建点击目标库存。
            };
            return Ok((
                SemanticWork {
                    metadata_bytes: metadata,
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                },
                false,
            ));
        }
        if let Some(link) = self.link.as_mut() {
            let (mut work, projection) = link.advance(document, selection, quota, budget)?;
            if let Some(projection) = projection {
                self.link = None;
                if let Some(link) = projection.link {
                    quota
                        .admit_metadata(std::mem::size_of::<MarkdownSourceLink>())
                        .map_err(MarkdownTableRequestError::Window)?;
                    self.target_bytes += link.destination.byte_len();
                    self.links.reserve_exact(1);
                    self.links.push(link);
                    work.metadata_bytes += std::mem::size_of::<MarkdownSourceLink>();
                }
                self.visible = Some(projection.span);
                self.visible_index = 0;
            }
            return Ok((work, false));
        }
        if let Some(run) = self.incoming.take() {
            if self.emphasis.is_none() {
                quota
                    .admit_metadata(std::mem::size_of::<EmphasisJob>())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.emphasis = Some(Box::new(EmphasisJob::new(
                    self.options,
                    self.source_range.clone(),
                )));
                self.incoming = Some(run);
                return Ok((
                    SemanticWork {
                        metadata_bytes: std::mem::size_of::<EmphasisJob>(),
                        structural_transitions: PLAIN_CONTROL,
                        ..Default::default()
                    },
                    false,
                ));
            }
            self.emphasis.as_mut().unwrap().start(run)?;
            return Ok((
                SemanticWork {
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                },
                false,
            ));
        }
        if let Some(emphasis) = self.emphasis.as_mut()
            && emphasis.busy()
        {
            let (work, span) = emphasis.advance(quota)?;
            if let Some(span) = span
                && span.whole.start < self.coverage.end
                && self.coverage.start < span.whole.end
            {
                self.pending = Some(span);
            }
            return Ok((work, false));
        }
        if self.cursor.is_none() {
            if let Some(mut emphasis) = self.emphasis.take() {
                emphasis.finish()?;
            } // 真正close原FD之后才结束；源失败/取消则由原请求drop释放同一owner。
            self.done = true;
            return Ok((
                SemanticWork {
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                },
                true,
            ));
        }
        let tree = tree_height(document);
        let child = budget.saturating_sub(tree + PLAIN_CONTROL) * 4 / 5;
        if child < 2048 {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let cursor = self.cursor.as_mut().unwrap();
        let frame = document
            .table_source_frame(cursor.next_byte(), child)
            .map_err(MarkdownTableRequestError::Source)?;
        let workspace_before = cursor.allocated_workspace_bytes();
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
                child,
            )
            .map_err(MarkdownTableRequestError::Global);
        frame.finish().map_err(MarkdownTableRequestError::Source)?;
        let step = step?;
        let allocated = cursor.allocated_workspace_bytes() - workspace_before;
        quota
            .admit_metadata(allocated)
            .map_err(MarkdownTableRequestError::Window)?;
        let measured = match &step {
            InlineCodeStep::Progress { work }
            | InlineCodeStep::NeedInput { work }
            | InlineCodeStep::Span { work, .. }
            | InlineCodeStep::Prefix { work, .. }
            | InlineCodeStep::Emphasis { work, .. }
            | InlineCodeStep::Link { work, .. }
            | InlineCodeStep::LinkEnd { work, .. }
            | InlineCodeStep::Proven { work, .. }
            | InlineCodeStep::NeedsGeneralInline { work } => *work,
        };
        match step {
            InlineCodeStep::Span { proof, .. } => {
                let range = proof.source_range();
                if range.start < self.coverage.end && self.coverage.start < range.end {
                    self.pending = Some(SpanProof {
                        whole: range,
                        body: proof.raw_body_range(),
                        styles: MarkdownPresentationStyles::INLINE_CODE,
                    });
                }
            }
            InlineCodeStep::Prefix { proof, .. } => {
                let range = proof.source_range();
                if range.start < self.coverage.end && self.coverage.start < range.end {
                    self.prefix = Some(PrefixDecoration::new(proof));
                } // 真物理行 owner 由原 SourceRequest 交回；不按裁剪范围决定显隐。
            }
            InlineCodeStep::Emphasis { run, .. } => self.incoming = Some(run),
            InlineCodeStep::Link { proof, .. } => self.incoming_link = Some(proof),
            InlineCodeStep::LinkEnd { source, label, .. } => {
                self.emphasis
                    .as_mut()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?
                    .leave_link(&source, &label)?;
            }
            InlineCodeStep::Proven {
                requires_general_styles,
                ..
            } => {
                self.general = requires_general_styles;
                let containers = self.cursor.as_ref().unwrap().containers();
                for (present, style) in [
                    (containers.quote, MarkdownPresentationStyles::BLOCK_QUOTE),
                    (
                        containers.unordered_list,
                        MarkdownPresentationStyles::UNORDERED_LIST,
                    ),
                    (
                        containers.ordered_list,
                        MarkdownPresentationStyles::ORDERED_LIST,
                    ),
                ] {
                    if present {
                        self.containers = self.containers.union(style);
                    }
                } // 只有原 scope 与 inline 都完成后才发布继承样式，不从 marker 拼写猜祖先。
                self.cursor = None;
            }
            InlineCodeStep::NeedsGeneralInline { .. } => {
                self.general = true;
                self.cursor = None;
            }
            _ => {}
        }
        Ok((
            SemanticWork {
                source_bytes: measured.byte_reads,
                metadata_bytes: allocated,
                structural_transitions: measured.transitions + tree + PLAIN_CONTROL,
                source_tree_steps: tree,
                ..Default::default()
            },
            self.done,
        ))
    }

    /// 查询只遍历当前 viewport 的已证交集；无交集的区间只有最终 plain/code prefix 成功后才可默认。
    pub(super) fn advance_query(
        &mut self,
        index: usize,
        budget: usize,
    ) -> (SemanticWork, Option<MarkdownPresentationQuery>) {
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        if let Some(cached) = self.query_cached.as_ref()
            && cached.utf16_range.contains(&index)
        {
            return (
                work,
                Some(MarkdownPresentationQuery {
                    utf16_range: index..cached.utf16_range.end,
                    styles: cached.styles,
                }),
            );
        }
        if self.query.is_none() {
            self.query = Some(MarkdownPresentationQuery {
                utf16_range: index..self.requested.end,
                styles: self.containers,
            });
            self.query_index = 0;
        }
        let query = self.query.as_mut().unwrap();
        while self.query_index < self.spans.len()
            && work.structural_transitions + std::mem::size_of::<VisibleSpan>() <= budget
        {
            let span = &self.spans[self.query_index];
            self.query_index += 1;
            work.structural_transitions += std::mem::size_of::<VisibleSpan>();
            let end = &mut query.utf16_range.end;
            let styles = &mut query.styles;
            if index < span.whole.start {
                *end = (*end).min(span.whole.start);
                continue;
            }
            if span.whole.contains(&index) {
                *styles = styles.union(span.styles);
                *end = (*end).min(span.whole.end);
                if index < span.body.start {
                    *end = (*end).min(span.body.start);
                } else if index < span.body.end {
                    *end = (*end).min(span.body.end);
                }
                if !span.body.contains(&index) {
                    *styles = styles.union(MarkdownPresentationStyles::MARKER);
                    if !span.active {
                        *styles = styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                    }
                }
            }
        }
        if self.query_index != self.spans.len() {
            return (work, None);
        }
        let query = self.query.take().unwrap();
        self.query_cached = Some(query.clone());
        (work, Some(query))
    }
    pub(super) fn push_output(
        &mut self,
        run: MarkdownPresentationRun,
        quota: &MarkdownTableWindowQuota,
    ) -> Result<(), MarkdownTableRequestError> {
        quota
            .admit_metadata(std::mem::size_of::<Option<MarkdownPresentationRun>>())
            .map_err(MarkdownTableRequestError::Window)?;
        self.output.reserve_exact(1);
        self.output.push(Some(run));
        Ok(())
    }

    /// 与父 Leaf 的既有链接移交口相同；只移动已完成、当前覆盖内的真实目标，不另复制 String。
    pub(super) fn take_link(&mut self) -> Option<MarkdownSourceLink> {
        self.links.pop()
    }
    /// 发布前真实归还可见 code 证明和坐标 scratch，仅保已经封存的输出 run。
    pub(super) fn release_scratch(&mut self) {
        self.spans = Vec::new();
        self.pending = None;
        self.coordinates = None;
        self.cursor = None;
        self.emphasis = None;
        self.incoming = None;
        self.incoming_link = None;
        self.link = None;
        self.visible = None;
        self.query = None;
        self.query_cached = None;
        self.prefix = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只查询本进程descriptor当前实际inode；并发测试可复用fd数值，不能把新文件误报为旧占用。
    pub(super) fn descriptor_identity(
        descriptor: std::os::fd::RawFd,
    ) -> Option<(libc::dev_t, libc::ino_t)> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat只写完整stat到有效栈槽，不创建File或取得descriptor的close权限。
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } == 0 {
            // SAFETY: 只有fstat成功后才读取它已完整初始化的系统结构。
            let status = unsafe { status.assume_init() };
            Some((status.st_dev, status.st_ino))
        } else {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            None
        }
    }

    #[test]
    /// 原文件栈已换页后分别取消、源EIO、索引EBADF；首错永久保留并真实归还唯一FD。
    fn active_emphasis_file_is_closed_on_cancel_and_real_source_or_storage_errors() {
        for scenario in ["cancel", "source", "storage"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("giant-emphasis.md");
            let source = format!(
                "{}{}{}\n",
                "*a ".repeat(200),
                // 超过原64KiB驻留槽后才有真实msync驱逐；短文件只武装注入不能证明源首错处理。
                "中".repeat(40_000),
                " a*".repeat(200)
            );
            std::fs::write(&path, &source).unwrap();
            let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
            let observer = document.original_backing_observer().unwrap();
            let authority = MarkdownViewportSourceAuthority {
                context_revision: 57,
                selection: document.selection(),
                cancelled: false,
            };
            let mut request = document
                .viewport_leaf_source_request(1000..1040, 57, document.selection())
                .unwrap();
            let descriptor = loop {
                request.advance(&document, authority, 4096).unwrap();
                if let Some(descriptor) = request
                    .semantic
                    .as_ref()
                    .and_then(|job| job.code.as_ref())
                    .and_then(|job| job.emphasis.as_ref())
                    .and_then(|job| job.descriptor_for_test())
                {
                    break descriptor;
                }
                assert!(request.sources_for(&document, authority).unwrap().is_none());
            };
            let descriptor = if scenario == "storage" {
                let read_only = directory.path().join("read-only-index");
                std::fs::write(&read_only, b"unchanged").unwrap();
                let emphasis = request
                    .semantic
                    .as_mut()
                    .unwrap()
                    .code
                    .as_mut()
                    .unwrap()
                    .emphasis
                    .as_mut()
                    .unwrap();
                emphasis.replace_file_for_test(std::fs::File::open(read_only).unwrap());
                emphasis.descriptor_for_test().unwrap()
            } else {
                descriptor
            };
            let original_inode = descriptor_identity(descriptor).unwrap();
            if scenario == "cancel" {
                request.cancel();
            } else if scenario == "source" {
                observer.fail_next_reclaim_for_test();
            }
            let error = loop {
                match request.advance(&document, authority, 4096) {
                    Ok(_) => assert!(request.sources_for(&document, authority).unwrap().is_none()),
                    Err(error) => break error,
                }
            };
            match scenario {
                "cancel" => assert_eq!(error, MarkdownTableRequestError::Cancelled),
                "source" => assert_eq!(
                    error,
                    MarkdownTableRequestError::Source(
                        MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                    )
                ),
                _ => assert_eq!(
                    error,
                    MarkdownTableRequestError::InlineStorageUnavailable(libc::EBADF)
                ),
            }
            assert_eq!(
                request.advance(&document, authority, 4096).err(),
                Some(error)
            );
            // 原fd必须已经关闭或被内核复用为其他inode；仍指向原inode就是实际泄漏，不能放行。
            assert_ne!(descriptor_identity(descriptor), Some(original_inode));
            assert!(request.semantic.is_none());
            drop(request);
            drop(document);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }

    #[test]
    /// 真实 giant probe 在远处 closer 之前被取消或换页失败时，不能发布半份样式或保住原映射。
    fn active_giant_code_probe_releases_source_and_owner_on_cancel_and_reclaim_error() {
        for cancel in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("code-resource.md");
            let source = format!("prefix ` {} ` tail\n", "中".repeat(40_000));
            std::fs::write(&path, &source).unwrap();
            let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
            let observer = document.original_backing_observer().unwrap();
            let authority = MarkdownViewportSourceAuthority {
                context_revision: 57,
                selection: document.selection(),
                cancelled: false,
            };
            let mut request = document
                .viewport_leaf_source_request(100..120, 57, document.selection())
                .unwrap();
            for _ in 0..source.len() * 100 + 10000 {
                request.advance(&document, authority, 4096).unwrap();
                if request
                    .semantic
                    .as_ref()
                    .and_then(|job| job.code.as_ref())
                    .and_then(|job| job.cursor.as_ref())
                    .is_some_and(|cursor| cursor.next_byte() > 20_000)
                {
                    break;
                }
            }
            assert!(
                request
                    .semantic
                    .as_ref()
                    .and_then(|job| job.code.as_ref())
                    .and_then(|job| job.cursor.as_ref())
                    .is_some_and(|cursor| cursor.next_byte() > 20_000),
                "必须先进入真实 code lookahead 再注入失败"
            );
            if cancel {
                request.cancel();
                assert_eq!(
                    request.advance(&document, authority, 4096).err(),
                    Some(MarkdownTableRequestError::Cancelled)
                );
                assert_eq!(observer.first_errno(), None);
                assert!(document.original_backing_health().is_ok());
            } else {
                observer.fail_next_reclaim_for_test();
                let error = loop {
                    match request.advance(&document, authority, 4096) {
                        Ok(MarkdownViewportSourceStep::Progress { .. }) => {}
                        Ok(_) => panic!("未完成真实 code 回收不能取得 Ready"),
                        Err(error) => break error,
                    }
                };
                assert_eq!(
                    error,
                    MarkdownTableRequestError::Source(
                        crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                    )
                );
                assert_eq!(
                    request.advance(&document, authority, 4096).err(),
                    Some(error)
                );
                assert_eq!(observer.first_errno(), Some(libc::EIO));
            }
            assert!(request.semantic.is_none() && request.leaf_semantic.is_none());
            assert!(request.batch.source_global_mut().is_err());
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            drop(request);
            drop(document);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }
}
