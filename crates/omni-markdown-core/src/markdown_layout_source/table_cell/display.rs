//! 同一真实表格 publication 的显示来源准备；原标量转换、原 pieces 与原匿名目录共用一个 owner。

use std::rc::Rc;

use super::*;
use crate::markdown_document_buffer::display_projection::{
    DisplayProjection, DisplayProjectionBuilder, DisplayProjectionVisibleRawRanges,
};
use crate::markdown_document_buffer::{
    MarkdownTableCellInlineStep, MarkdownViewportSourceAuthority, MarkdownViewportSourceRequest,
    SemanticWork, VerifiedMarkdownTableCellInline, VerifiedMarkdownTableRowSource,
    VerifiedMarkdownViewportSources,
};
use crate::markdown_layout_index::LayoutIndexObserver;
use crate::{
    MarkdownComposedLineAffinity, MarkdownPresentationQuery, MarkdownSourceLink,
    MarkdownTableCellDisplayAnswer, MarkdownTableCellDisplayCursor, MarkdownTableCellDisplayInput,
};

#[derive(Debug)]
/// Ready 只表示同一格完整转换和目录封存结束，Progress 必须继续支付真实原输入工作。
pub enum MarkdownTableCellDisplayPreparationStep {
    Progress { work: SemanticWork },
    Ready { work: SemanticWork },
}

/// 显示源只有这条真正完成的生产路径能构造，原 raw 结构票本身不能制造此类型。
pub struct MarkdownTableCellDisplaySource {
    source: MarkdownLayoutTextSource,
    projection: Arc<DisplayProjection>,
    cell: VerifiedMarkdownTableCellSource,
    canonical: Range<usize>,
    presentation: CellPresentation,
    authority: MarkdownViewportSourceAuthority,
}

/// 原显示目录的借用型可见raw片段；只交已输出标量的canonical域，不能保活或复制另一份Source。
pub struct MarkdownTableCellVisibleSourceRanges<'a> {
    raw: DisplayProjectionVisibleRawRanges<'a>,
    canonical_start: usize,
}

impl MarkdownTableCellVisibleSourceRanges<'_> {
    /// 仅表示本次共享额度确实夹断了可见源；不能据已返回片段声称检查完整视口。
    #[must_use]
    pub fn source_limited(&self) -> bool {
        self.raw.source_limited()
    }
}

impl Iterator for MarkdownTableCellVisibleSourceRanges<'_> {
    type Item = Result<Range<usize>, MarkdownDocumentBufferError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.raw.next().map(|range| {
            range.map(|range| self.canonical_start + range.start..self.canonical_start + range.end)
        })
    }
}

impl MarkdownTableCellDisplaySource {
    /// 同publication真票限定显示窗口；迭代仍只借原sealed记录，MARKER和转义删符没有raw片段。
    /// 调用方须在无Native重入的消费后重核原窗口；退出或额度耗尽直接归还借用，不保新读取owner。
    pub fn visible_canonical_ranges_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        display: Range<usize>,
        maximum_units: usize,
    ) -> Result<MarkdownTableCellVisibleSourceRanges<'a>, MarkdownTableRequestError> {
        self.require_for(document)?;
        let raw = self
            .projection
            .visible_raw_ranges(display, maximum_units)
            .map_err(MarkdownTableRequestError::Source)?;
        self.require_for(document)?;
        Ok(MarkdownTableCellVisibleSourceRanges {
            raw,
            canonical_start: self.canonical.start,
        })
    }

    /// 原 Composer 读取实际显示字节，GCB、UBA 和字体请求都属于此 Source，不属于 raw 格。
    #[must_use]
    pub fn source(&self) -> &MarkdownLayoutTextSource {
        &self.source
    }

    /// 内层窗口和 paint receipt 必须保同一显示 Source 身份，不能只比较外层 canonical SceneKey。
    #[must_use]
    pub fn authority(&self) -> MarkdownLayoutTextAuthority {
        self.source.authority()
    }

    /// 原格票只授原始范围和真实行列；调用方核原 Doc 后才可用于 canonical 定位。
    #[must_use]
    pub fn canonical_raw_cell(&self) -> &VerifiedMarkdownTableCellSource {
        &self.cell
    }

    /// 每次原 Native 重入前后重核外层同一 publication；显示 Snapshot 冻结不表示当前编辑权限。
    pub fn require_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(), MarkdownTableRequestError> {
        self.projection
            .health()
            .map_err(MarkdownTableRequestError::Source)?;
        self.cell.canonical_range_for(document)?;
        self.presentation.require(document, self.authority)?;
        Ok(())
    }

    /// 激活只消费同一原 Parser 发布的真实目标；显示转换目录本身不会猜测 URL。
    pub fn link_at_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        canonical_index: usize,
    ) -> Result<Option<&'a MarkdownSourceLink>, MarkdownTableRequestError> {
        self.require_for(document)?;
        self.presentation
            .link(document, self.authority, canonical_index)
    }

    /// 只读目录保留显示边界的真实两侧；非空范围起点用下游、末端用上游，隐藏语法不占可见字符。
    pub fn display_to_canonical(
        &self,
        position: usize,
        affinity: MarkdownComposedLineAffinity,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        let raw = self.projection.display_to_raw(
            position,
            affinity == MarkdownComposedLineAffinity::Downstream,
        )?;
        Ok(self.canonical.start + raw)
    }

    /// 范围外不属于这个格，隐藏区明确沿给定亲和映射；半代理端点仍由同一 raw Snapshot 拒绝。
    pub fn canonical_to_display(
        &self,
        position: usize,
        affinity: MarkdownComposedLineAffinity,
    ) -> Result<Option<usize>, MarkdownDocumentBufferError> {
        self.projection.health()?;
        if position < self.canonical.start || position > self.canonical.end {
            return Ok(None);
        }
        self.projection
            .raw_to_display(
                position - self.canonical.start,
                affinity == MarkdownComposedLineAffinity::Downstream,
            )
            .map(Some)
    }

    /// 字符样式只来自原 Parser 已授权的同一输出目录；空显示和 EOF 没有可读字符范围。
    pub fn styles_at(
        &self,
        position: usize,
    ) -> Result<Option<MarkdownPresentationQuery>, MarkdownDocumentBufferError> {
        self.projection.styles_at(position)
    }

    /// 观察者只持实际资源原子事实，不能保活显示 Source、目录 FD 或原映射。
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownTableCellDisplayResourceObserver {
        MarkdownTableCellDisplayResourceObserver {
            source: self.source.resource_observer(),
            index: self.projection.resource_observer(),
        }
    }
}

#[derive(Clone)]
pub struct MarkdownTableCellDisplayResourceObserver {
    source: MarkdownLayoutTextSourceObserver,
    index: LayoutIndexObserver,
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownTableCellDisplayResources {
    pub source_payload_released: bool,
    pub origin_file_open: bool,
    pub origin_buffer_bytes: usize,
    pub origin_peak_buffer_bytes: usize,
    pub origin_cleanup_failed: bool,
    pub origin_reader_failed: bool,
}

impl MarkdownTableCellDisplayResourceObserver {
    /// 实际 FD/工作页与 Source payload 分列，不把 metadata 大小冒充整产品物理内存。
    #[must_use]
    pub fn snapshot(&self) -> MarkdownTableCellDisplayResources {
        let state = self.index.snapshot();
        MarkdownTableCellDisplayResources {
            source_payload_released: self.source.payload_released(),
            origin_file_open: state.file_open,
            origin_buffer_bytes: state.buffer_bytes,
            origin_peak_buffer_bytes: state.peak_buffer_bytes,
            origin_cleanup_failed: state.cleanup_failed,
            origin_reader_failed: state.reader_failed,
        }
    }
}

/// 固定 4 KiB 原字节与一个 UTF8 scalar 残余；跨片 Paused 不会让转换器把未见后继当作 EOF。
struct RawInput {
    source: MarkdownLayoutTextSource,
    buffer: Box<[u8; MARKDOWN_LAYOUT_SOURCE_BYTES]>,
    offset: usize,
    valid: usize,
    next_byte: usize,
    scalar: [u8; 4],
    used: usize,
    needed: usize,
}

enum RawInputScalar {
    Scalar(char),
    Paused,
    End,
}

impl RawInput {
    /// 首次分配只属当前真实准备作业，不因格长度扩大，也不为每个离屏格提前创建。
    fn new(source: MarkdownLayoutTextSource) -> Self {
        Self {
            source,
            buffer: Box::new([0; MARKDOWN_LAYOUT_SOURCE_BYTES]),
            offset: 0,
            valid: 0,
            next_byte: 0,
            scalar: [0; 4],
            used: 0,
            needed: 0,
        }
    }

    /// 每次最多真实借一段 4 KiB 原源；已有未消费字节继续沿同一 buffer，不重复读取前缀。
    fn refill(&mut self) -> Result<usize, MarkdownDocumentBufferError> {
        self.source.health()?;
        if self.offset != self.valid || self.next_byte == self.source.byte_len() {
            return Ok(0);
        }
        let end = self
            .source
            .byte_len()
            .min(self.next_byte + MARKDOWN_LAYOUT_SOURCE_BYTES);
        let mut count = 0;
        self.source.visit_byte_range(self.next_byte..end, |bytes| {
            self.buffer[count..count + bytes.len()].copy_from_slice(bytes);
            count += bytes.len();
            Ok(())
        })?;
        self.offset = 0;
        self.valid = count;
        self.next_byte = end;
        Ok(count)
    }

    /// 来源已经完成 UTF8 验证，仍对真实截断/非法 scalar 保留错误，不能交付半个可见字形。
    fn next(&mut self) -> Result<RawInputScalar, MarkdownDocumentBufferError> {
        while self.offset != self.valid {
            let byte = self.buffer[self.offset];
            self.offset += 1;
            if self.used == 0 {
                self.needed = match byte {
                    0..=0x7f => 1,
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => return Err(MarkdownDocumentBufferError::InvalidUtf8Document),
                };
            }
            self.scalar[self.used] = byte;
            self.used += 1;
            if self.used == self.needed {
                let scalar = std::str::from_utf8(&self.scalar[..self.used])
                    .ok()
                    .and_then(|text| text.chars().next())
                    .ok_or(MarkdownDocumentBufferError::InvalidUtf8Document)?;
                self.used = 0;
                return Ok(RawInputScalar::Scalar(scalar));
            }
        }
        if self.next_byte == self.source.byte_len() {
            if self.used != 0 {
                return Err(MarkdownDocumentBufferError::InvalidUtf8Document);
            }
            Ok(RawInputScalar::End)
        } else {
            Ok(RawInputScalar::Paused)
        }
    }
}

/// 两种实际发布来源各自持原授权；结构行本身不会进入此枚举。
#[derive(Clone)]
enum CellPresentation {
    Window {
        publication: Rc<VerifiedMarkdownViewportSources>,
        window: usize,
    },
    Inline(Rc<VerifiedMarkdownTableCellInline>),
}

impl CellPresentation {
    /// 外带显示 Source 仍必须核对当前原 publication，不能用冻结字节恢复编辑权限。
    fn require(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        match self {
            Self::Window {
                publication,
                window,
            } => {
                publication
                    .table_sources_for(document, authority)?
                    .window_at_for(document, *window)?
                    .ok_or(MarkdownTableRequestError::PublicationSuperseded)?;
                Ok(())
            }
            Self::Inline(publication) => publication.require_for(document, authority),
        }
    }

    /// 有限窗口沿原 runs 查询；流式格沿同一个原语义发布查询，未知区始终拒绝转换。
    fn query(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        canonical: &Range<usize>,
        index: usize,
    ) -> Result<Option<MarkdownPresentationQuery>, MarkdownTableRequestError> {
        match self {
            Self::Window {
                publication,
                window,
            } => {
                let sources = publication.table_sources_for(document, authority)?;
                let window = sources
                    .window_at_for(document, *window)?
                    .ok_or(MarkdownTableRequestError::PublicationSuperseded)?;
                let presentation = window.presentation_for(document)?;
                Ok(crate::markdown_viewport_presentation::query_published_runs(
                    &presentation.runs,
                    canonical,
                    index,
                ))
            }
            Self::Inline(publication) => publication.query_for(document, authority, index),
        }
    }

    /// 正式结构行入口只使用真实 CellInline 链接索引；旧完整窗口从未签发链接目标权限。
    fn link<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<&'a MarkdownSourceLink>, MarkdownTableRequestError> {
        match self {
            Self::Inline(publication) => publication.link_at_for(document, authority, index),
            Self::Window { .. } => Err(MarkdownTableRequestError::InvalidRequest),
        }
    }
}

/// 单格准备串行借原 SourceRequest；发布票不拥有第二个 Global、请求或可重入 RefCell。
pub struct MarkdownTableCellDisplayPreparation {
    state: Option<Preparation>,
    failed: Option<MarkdownTableRequestError>,
}

enum Preparation {
    Inline {
        row: VerifiedMarkdownTableRowSource,
        authority: MarkdownViewportSourceAuthority,
        column: usize,
        started: bool,
    },
    Display(Box<DisplayPreparation>),
}

impl MarkdownTableCellDisplayPreparation {
    /// 原完整窗口已有真实 inline 发布，可直接进入同一显示转换器。
    pub fn new(
        document: &MarkdownDocumentBuffer,
        publication: Rc<VerifiedMarkdownViewportSources>,
        authority: MarkdownViewportSourceAuthority,
        window: usize,
        row: usize,
        column: usize,
    ) -> Result<Self, MarkdownTableRequestError> {
        Ok(Self {
            state: Some(Preparation::Display(Box::new(DisplayPreparation::new(
                document,
                publication,
                authority,
                window,
                row,
                column,
            )?))),
            failed: None,
        })
    }

    /// 结构行只建立 Pending；原请求后续完成 RequiredRow、原 math 与原 Inline 才可分配显示源。
    pub fn from_row_source(
        document: &MarkdownDocumentBuffer,
        row: VerifiedMarkdownTableRowSource,
        authority: MarkdownViewportSourceAuthority,
        column: usize,
    ) -> Result<Self, MarkdownTableRequestError> {
        row.require_for(document, authority)?;
        let (_, _, columns) = row.coordinate_for(document, authority)?;
        if column >= columns {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(Self {
            state: Some(Preparation::Inline {
                row,
                authority,
                column,
                started: false,
            }),
            failed: None,
        })
    }

    /// 完整窗口的原入口保留真实用途；结构行必须显式借唯一请求，不能静默另建 Global。
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
    ) -> Result<MarkdownTableCellDisplayPreparationStep, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        match self
            .state
            .as_mut()
            .ok_or(MarkdownTableRequestError::Cancelled)?
        {
            Preparation::Display(preparation) => preparation.advance(document, cancelled),
            Preparation::Inline { row, authority, .. } => {
                row.require_for(document, *authority)?;
                Err(if cancelled {
                    MarkdownTableRequestError::Cancelled
                } else {
                    MarkdownTableRequestError::InvalidRequest
                })
            }
        }
    }

    /// 每次只推进一个原语义量子或一个转换量子；唯一请求保留 Global 与跨格引用缓存的所有权。
    pub fn advance_with_source_request(
        &mut self,
        document: &MarkdownDocumentBuffer,
        request: &mut MarkdownViewportSourceRequest,
        cancelled: bool,
    ) -> Result<MarkdownTableCellDisplayPreparationStep, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = (|| match self
            .state
            .as_mut()
            .ok_or(MarkdownTableRequestError::Cancelled)?
        {
            Preparation::Display(preparation) => preparation.advance(document, cancelled),
            Preparation::Inline {
                row,
                authority,
                column,
                started,
            } => {
                row.require_for(document, *authority)?;
                if cancelled {
                    request.cancel();
                    return Err(MarkdownTableRequestError::Cancelled);
                }
                if !*started {
                    request.prepare_cell_inline(document, row.clone(), *authority, *column)?;
                    *started = true;
                    return Ok(MarkdownTableCellDisplayPreparationStep::Progress {
                        work: SemanticWork::default(),
                    });
                }
                let step = request.advance_cell_inline(
                    document,
                    *authority,
                    MARKDOWN_LAYOUT_SOURCE_BYTES,
                )?;
                match step {
                    MarkdownTableCellInlineStep::Progress { work } => {
                        Ok(MarkdownTableCellDisplayPreparationStep::Progress { work })
                    }
                    MarkdownTableCellInlineStep::Ready { work } => {
                        let publication = request
                            .take_cell_inline_ready_for(document, *authority)?
                            .expect("原 Ready 单格只移交一次");
                        let preparation = DisplayPreparation::from_inline(
                            document,
                            Rc::new(publication),
                            *authority,
                        )?;
                        self.state = Some(Preparation::Display(Box::new(preparation)));
                        Ok(MarkdownTableCellDisplayPreparationStep::Progress { work })
                    }
                }
            }
        })();
        if let Err(error) = result {
            let error = document
                .original_backing_health()
                .err()
                .map(MarkdownTableRequestError::Source)
                .unwrap_or(error);
            self.failed = Some(error);
            return Err(error);
        }
        result
    }

    /// 真正显示目录完成前没有可移交的源；Pending 行票不能成为半成品 Ready。
    pub fn take_ready_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<MarkdownTableCellDisplaySource>, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        match self
            .state
            .as_mut()
            .ok_or(MarkdownTableRequestError::Cancelled)?
        {
            Preparation::Display(preparation) => preparation.take_ready_for(document),
            Preparation::Inline { row, authority, .. } => {
                row.require_for(document, *authority)?;
                Ok(None)
            }
        }
    }

    /// 只观察实际已创建的显示目录；Inline尚未分配writer时为None，不为收据补建任何资源。
    #[must_use]
    pub fn resource_observer(&self) -> Option<MarkdownTableCellDisplayResourceObserver> {
        let Preparation::Display(preparation) = self.state.as_ref()? else {
            return None;
        };
        if let Some(display) = &preparation.ready {
            return Some(display.resource_observer());
        }
        Some(MarkdownTableCellDisplayResourceObserver {
            source: preparation.input.source.resource_observer(),
            index: preparation.builder.as_ref()?.resource_observer(),
        })
    }

    /// 取消只归还本格原任务。尚未借Request或已经进入Display时立即释放；已借Inline则
    /// 按原4KiB量子推进至真正take归槽，丢弃语义结果，不创建或继续显示转换。
    /// Ready表示本准备的真实资源已经释放，原SourceRequest仍可准备其他格；后续advance返回Cancelled。
    pub fn cancel_with_source_request(
        &mut self,
        document: &MarkdownDocumentBuffer,
        request: &mut MarkdownViewportSourceRequest,
    ) -> Result<MarkdownTableCellDisplayPreparationStep, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = (|| match self
            .state
            .as_mut()
            .ok_or(MarkdownTableRequestError::Cancelled)?
        {
            Preparation::Display(preparation) => {
                preparation.require(document)?;
                Ok(MarkdownTableCellDisplayPreparationStep::Ready {
                    work: SemanticWork::default(),
                })
            }
            Preparation::Inline {
                row,
                authority,
                started,
                ..
            } => {
                row.require_for(document, *authority)?;
                if !*started {
                    return Ok(MarkdownTableCellDisplayPreparationStep::Ready {
                        work: SemanticWork::default(),
                    });
                }
                match request.advance_cell_inline(
                    document,
                    *authority,
                    MARKDOWN_LAYOUT_SOURCE_BYTES,
                )? {
                    MarkdownTableCellInlineStep::Progress { work } => {
                        Ok(MarkdownTableCellDisplayPreparationStep::Progress { work })
                    }
                    MarkdownTableCellInlineStep::Ready { work } => {
                        drop(
                            request
                                .take_cell_inline_ready_for(document, *authority)?
                                .expect("取消仍须真正消费原格Ready，归还唯一来源槽"),
                        );
                        Ok(MarkdownTableCellDisplayPreparationStep::Ready { work })
                    }
                }
            }
        })();
        match result {
            Ok(MarkdownTableCellDisplayPreparationStep::Ready { work }) => {
                drop(self.state.take()); // 先真正关闭writer/Source/追加页，再发布取消完成。
                self.failed = Some(MarkdownTableRequestError::Cancelled);
                Ok(MarkdownTableCellDisplayPreparationStep::Ready { work })
            }
            Ok(step) => Ok(step),
            Err(error) => {
                let error = document
                    .original_backing_health()
                    .err()
                    .map(MarkdownTableRequestError::Source)
                    .unwrap_or(error);
                self.failed = Some(error);
                drop(self.state.take());
                Err(error) // 原Source首errno不能被正常取消或publication失权覆盖。
            }
        }
    }
}

/// 一个格的真实有限准备；outer publication、raw reader、cursor 与目录 writer 由原 caller 串行推进。
struct DisplayPreparation {
    presentation: CellPresentation,
    authority: MarkdownViewportSourceAuthority,
    cell: VerifiedMarkdownTableCellSource,
    canonical: Range<usize>,
    input: RawInput,
    cursor: MarkdownTableCellDisplayCursor,
    builder: Option<DisplayProjectionBuilder>,
    ready: Option<MarkdownTableCellDisplaySource>,
    completed: bool,
    failed: Option<MarkdownTableRequestError>,
}

impl DisplayPreparation {
    /// 只接受同一真实 viewport publication 的已发布格；raw 长格结构票不能补出未知 inline 样式。
    pub fn new(
        document: &MarkdownDocumentBuffer,
        publication: Rc<VerifiedMarkdownViewportSources>,
        authority: MarkdownViewportSourceAuthority,
        window: usize,
        row: usize,
        column: usize,
    ) -> Result<Self, MarkdownTableRequestError> {
        document
            .original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        let sources = publication.table_sources_for(document, authority)?;
        let verified = sources
            .window_at_for(document, window)?
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let cell = document.verified_table_cell_source(verified, row, column)?;
        let canonical = cell.canonical_range_for(document)?.clone();
        let source = MarkdownLayoutTextSource::capture_table_cell(document, &cell)?;
        let builder = DisplayProjectionBuilder::new(source.0.snapshot.clone())
            .map_err(MarkdownTableRequestError::Source)?;
        Ok(Self {
            presentation: CellPresentation::Window {
                publication,
                window,
            },
            authority,
            cell,
            cursor: MarkdownTableCellDisplayCursor::new(canonical.start),
            canonical,
            input: RawInput::new(source),
            builder: Some(builder),
            ready: None,
            completed: false,
            failed: None,
        })
    }

    /// 原 CellInline Ready 已包含完整语义；此处只冻结同一 raw pieces，绝不重新解析或读取整格。
    fn from_inline(
        document: &MarkdownDocumentBuffer,
        publication: Rc<VerifiedMarkdownTableCellInline>,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Self, MarkdownTableRequestError> {
        let cell = publication.cell_source_for(document, authority)?.clone();
        let canonical = cell.canonical_range_for(document)?.clone();
        let source = MarkdownLayoutTextSource::capture_table_cell(document, &cell)?;
        let builder = DisplayProjectionBuilder::new(source.0.snapshot.clone())
            .map_err(MarkdownTableRequestError::Source)?;
        Ok(Self {
            presentation: CellPresentation::Inline(publication),
            authority,
            cell,
            cursor: MarkdownTableCellDisplayCursor::new(canonical.start),
            canonical,
            input: RawInput::new(source),
            builder: Some(builder),
            ready: None,
            completed: false,
            failed: None,
        })
    }

    /// 每量子至多读取 4 KiB、转换 256 个输入或输出标量；隐藏语法只记真实工作，不伪造完成。
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
    ) -> Result<MarkdownTableCellDisplayPreparationStep, MarkdownTableRequestError> {
        let result = self.advance_inner(document, cancelled);
        if let Err(error) = result {
            let error = self
                .input
                .source
                .health()
                .err()
                .map(MarkdownTableRequestError::Source)
                .unwrap_or(error);
            self.failed = Some(error);
            drop(self.builder.take());
            drop(self.ready.take());
            return Err(error);
        }
        result
    }

    /// 所有读取都在同一 Doc 借用内；没有 Native 调用或可重入回调持有这些引用。
    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
    ) -> Result<MarkdownTableCellDisplayPreparationStep, MarkdownTableRequestError> {
        self.require(document)?;
        if let Some(error) = self.failed {
            return Err(error);
        }
        if cancelled {
            return Err(MarkdownTableRequestError::Cancelled);
        }
        if self.completed {
            return if self.ready.is_some() {
                Ok(MarkdownTableCellDisplayPreparationStep::Ready {
                    work: SemanticWork::default(),
                })
            } else {
                Err(MarkdownTableRequestError::InvalidRequest)
            };
        }
        let copied = self
            .input
            .refill()
            .map_err(MarkdownTableRequestError::Source)?;
        let metadata_before = self
            .builder
            .as_ref()
            .expect("活动准备拥有原 writer")
            .record_bytes();
        let mut work = SemanticWork {
            source_bytes: copied,
            copied_source_bytes: copied,
            ..SemanticWork::default()
        };
        let mut inputs = 0;
        let mut outputs = 0;
        loop {
            let step = self
                .cursor
                .step_with_input_limit(256 - inputs, |canonical| {
                    Ok(
                        match self
                            .input
                            .next()
                            .map_err(MarkdownTableRequestError::Source)?
                        {
                            RawInputScalar::Scalar(scalar) => {
                                MarkdownTableCellDisplayInput::Scalar {
                                    scalar,
                                    styles: self
                                        .presentation
                                        .query(
                                            document,
                                            self.authority,
                                            &self.canonical,
                                            canonical,
                                        )?
                                        .ok_or(MarkdownTableRequestError::PublicationSuperseded)?
                                        .styles,
                                }
                            }
                            RawInputScalar::Paused => MarkdownTableCellDisplayInput::Paused,
                            RawInputScalar::End => MarkdownTableCellDisplayInput::End,
                        },
                    )
                })?;
            inputs += step.input_scalars;
            work.structural_transitions += step.input_scalars;
            match step.answer {
                MarkdownTableCellDisplayAnswer::Scalar(scalar) => {
                    self.builder
                        .as_mut()
                        .expect("活动显示准备拥有唯一目录 writer")
                        .append(scalar)
                        .map_err(MarkdownTableRequestError::Source)?;
                    outputs += 1;
                    work.structural_transitions += 1;
                }
                MarkdownTableCellDisplayAnswer::NeedInput => break,
                MarkdownTableCellDisplayAnswer::Progress => break,
                MarkdownTableCellDisplayAnswer::Complete => {
                    self.require(document)?;
                    let builder = self.builder.take().expect("首次完成仍拥有原目录 writer");
                    let projection = builder
                        .finish()
                        .map_err(MarkdownTableRequestError::Source)?;
                    work.metadata_bytes = projection.record_bytes() - metadata_before;
                    let snapshot = MarkdownTextSnapshot::from_display_projection(
                        projection.clone(),
                        self.input.source.generation(),
                    );
                    let source = MarkdownLayoutTextSource(Arc::new(Payload {
                        bytes: snapshot.byte_len(),
                        utf16: snapshot.utf16_len(),
                        generation: self.input.source.generation(),
                        snapshot,
                        domain: SourceDomain::ReadOnlyCell(Box::new(self.cell.clone())),
                        dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
                    }));
                    self.require(document)?;
                    self.ready = Some(MarkdownTableCellDisplaySource {
                        source,
                        projection,
                        cell: self.cell.clone(),
                        canonical: self.canonical.clone(),
                        presentation: self.presentation.clone(),
                        authority: self.authority,
                    });
                    self.completed = true;
                    return Ok(MarkdownTableCellDisplayPreparationStep::Ready { work });
                }
            }
            if inputs >= 256 || outputs == 256 {
                break;
            }
        }
        work.metadata_bytes = self
            .builder
            .as_ref()
            .expect("未完成准备仍拥有原 writer")
            .record_bytes()
            - metadata_before;
        self.require(document)?;
        Ok(MarkdownTableCellDisplayPreparationStep::Progress { work })
    }

    /// 移交一次完成后的显示源；旧窗口、选择、Source 错误或消费后的请求均不能另签一份 Ready。
    pub fn take_ready_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<MarkdownTableCellDisplaySource>, MarkdownTableRequestError> {
        self.require(document)?;
        if let Some(error) = self.failed {
            return Err(error);
        }
        Ok(self.ready.take())
    }

    /// 健康和原 publication 必须同时有效，不能用相同 canonical 范围给后继输入补票。
    fn require(&self, document: &MarkdownDocumentBuffer) -> Result<(), MarkdownTableRequestError> {
        self.input
            .source
            .health()
            .map_err(MarkdownTableRequestError::Source)?;
        self.cell.canonical_range_for(document)?;
        self.presentation.require(document, self.authority)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
