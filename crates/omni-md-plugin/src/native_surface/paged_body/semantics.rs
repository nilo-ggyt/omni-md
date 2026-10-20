//! 同一Body输入的RootLeaf字体接入；只消费原有限publication，显隐不改canonical语法或坐标。

use std::{ops::Range, ptr::NonNull, rc::Rc};

use objc2::rc::Retained;
use objc2_app_kit::{NSFont, NSLayoutManager};
use objc2_core_foundation::CFRetained;
use objc2_core_text::CTFont;
use omni_markdown_core::{
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource, MarkdownLineComposeStyle,
    MarkdownLineComposer, MarkdownLineStyleRequest, MarkdownOrdinarySemanticState,
    MarkdownPresentationStyles, MarkdownTableCellDisplaySource, MarkdownTextSelection,
    MarkdownViewportSourceAuthority, MarkdownViewportSourceRequest, MarkdownViewportSourceStep,
    VerifiedMarkdownLayoutTableRow, VerifiedMarkdownRootLeafStyleBoundary,
    VerifiedMarkdownViewportSources,
};

use omni_markdown_core::markdown_document_buffer::{MarkdownTableRequestError, SemanticWork};
use omni_markdown_core::markdown_viewport_presentation::table_window::MarkdownTableWindowError;

use super::preparation::PreparationTicket;
use super::{NativeMarkdownPagedBody as Owner, NativeMarkdownPagedBodyError as Error, Result};
use crate::native_surface::{
    paged_fonts::NativeMarkdownPagedFont,
    paged_line_fonts::NativeMarkdownLineFont,
    text_attributes::{MarkdownTextAttributePalette, native_markdown_base_font},
};

const WINDOW_UTF16: usize = 16_384;
const QUANTUM: usize = 4096;

/// 基础角色沿原TextKit选字优先级；字重、斜体与下标另进入固定字体槽，不按正文长度增长。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Role {
    Body,
    Heading(usize),
    Code,
    Table,
    Hidden,
}
impl Role {
    fn from_styles(styles: MarkdownPresentationStyles) -> Self {
        // 显隐权限来自原选择绑定的真实语义；必须先于标题/代码，否则同一marker会借用大字体。
        if styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER) {
            return Self::Hidden;
        }
        for (index, heading) in [
            MarkdownPresentationStyles::HEADING_1,
            MarkdownPresentationStyles::HEADING_2,
            MarkdownPresentationStyles::HEADING_3,
            MarkdownPresentationStyles::HEADING_4,
            MarkdownPresentationStyles::HEADING_5,
            MarkdownPresentationStyles::HEADING_6,
        ]
        .into_iter()
        .enumerate()
        {
            if styles.contains(heading) {
                return Self::Heading(index);
            }
        }
        if styles.contains(MarkdownPresentationStyles::TABLE_HEADER)
            || (styles.contains(MarkdownPresentationStyles::TABLE)
                && styles.contains(MarkdownPresentationStyles::STRONG))
        {
            return Self::Table;
        }
        if styles.contains(MarkdownPresentationStyles::INLINE_CODE)
            || styles.contains(MarkdownPresentationStyles::FENCED_CODE)
        {
            Self::Code
        } else {
            Self::Body // 已认证的Rule等仍按原普通字体显示字面，不冒充已完成背景/隐藏预览。
        }
    }
    /// 只合并实际字形相同的组合；标题本来已粗体，代码沿原等宽face，不能借正文槽吞掉斜体。
    fn slot(self, styles: MarkdownPresentationStyles) -> Option<usize> {
        if self == Self::Hidden {
            return Some(40); // 隐藏字体不继承任何粗体、斜体或下标字号，保留原0.1pt身份。
        }
        let italic = usize::from(styles.contains(MarkdownPresentationStyles::EMPHASIS));
        let base = match self {
            Self::Body => {
                usize::from(styles.contains(MarkdownPresentationStyles::STRONG)) + italic * 2
            }
            Self::Heading(index) => 4 + index + italic * 6,
            Self::Code => 16 + italic,
            Self::Table => 18 + italic,
            Self::Hidden => unreachable!("隐藏角色已先返回独占槽"),
        };
        let subscript = styles.contains(MarkdownPresentationStyles::SUBSCRIPT)
            && !styles.contains(MarkdownPresentationStyles::MARKER);
        let slot = base + usize::from(subscript) * 20;
        (slot != 0).then_some(slot) // 普通正文继续复用原注册0号face，其余至多40个真实字体。
    }
}

/// 原属性策略直接选择真实NSFont；Native/Core捕获仍由既有精确face/axes入口完成。
pub(super) struct Typography {
    palette: MarkdownTextAttributePalette,
    slots: [Option<usize>; 41],
    /// 只借公开 default-line API；没有 storage、container、字符串或另一正文图。
    pub(super) metrics_owner: Rc<Retained<NSLayoutManager>>,
}
impl Typography {
    pub(super) fn new(font: &CTFont) -> Self {
        // SAFETY: 活CTFont在本调用有效，retain取得独立+1；Apple定义NSFont/CTFont toll-free bridge。
        let retained = unsafe { CFRetained::retain(NonNull::from(font)) };
        let retained: Retained<CTFont> = retained.into();
        // SAFETY: 上述真实CTFont与NSFont同一对象，只转换Rust视图并保留原+1所有权。
        let normal: Retained<NSFont> = unsafe { Retained::cast_unchecked(retained) };
        Self {
            palette: MarkdownTextAttributePalette::new(normal),
            slots: [None; 41],
            metrics_owner: Rc::new(NSLayoutManager::new()),
        }
    }
}

/// 在途试次固定原样式、coverage与publication；原selection变更不能给旧结果补票。
pub(super) struct Selection {
    pub(super) coverage: Range<usize>,
    pub(super) style: MarkdownLineComposeStyle,
    role: Role,
    styles: MarkdownPresentationStyles,
    /// closing-EOL 只持原物理 separator 票；不能把这份控制权限用于 inline 或隐藏语法。
    fence_separator: bool,
    scope: SelectionScope,
    source: MarkdownLayoutTextAuthority,
}

/// 三种真实来源互斥：源码字面、正文语义、格显示目录不能凭相同数值范围互相续权。
enum SelectionScope {
    Literal,
    Body(
        Rc<VerifiedMarkdownViewportSources>,
        MarkdownViewportSourceAuthority,
    ),
    Cell(Rc<MarkdownTableCellDisplaySource>, MarkdownTextSelection),
}

/// 独立表格行由同一次真实 publication 交接；它不是可套普通 Font 的根段落。
pub(super) enum StyleDecision {
    Font(Selection),
    Table(VerifiedMarkdownLayoutTableRow),
}
impl Selection {
    /// 只交已经绑定原 publication 的样式值；几何不能借它扩大来源或重新解释 Markdown。
    pub(super) fn styles(&self) -> MarkdownPresentationStyles {
        self.styles
    }

    /// 左字体响应只借当前Selection原出版物签语义交界；不重新查询最新窗口或另建Parser。
    /// 票只允许后继原Composer试右GCB，真实Font是否变化仍由同一个完整试次决定。
    pub(super) fn style_boundary(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
    ) -> Result<Option<VerifiedMarkdownRootLeafStyleBoundary>> {
        self.require(owner, source, request)?;
        let SelectionScope::Body(publication, authority) = &self.scope else {
            return Ok(None); // 字面或格显示不持RootLeaf权，不能用同值坐标签正文样式边界。
        };
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf样式沿原Body文档")
            .clone();
        Ok(source.verified_root_leaf_style_boundary(
            &document.borrow(),
            publication.clone(),
            *authority,
            request,
        )?)
    }

    /// 右邻字体试次只消费Core原slot借出的票；元数据回到原ScopeBody，保持现有逐Native调用复核。
    /// 本入口不推进SourceRequest、不改变左coverage，也不把右字素送进正文输出。
    pub(super) fn root_leaf_boundary(
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
        boundary: &VerifiedMarkdownRootLeafStyleBoundary,
        ticket: &PreparationTicket,
    ) -> Result<Self> {
        ticket.require_pure(owner, source)?;
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf右字素沿原Body文档")
            .clone();
        let (query, publication, authority) =
            boundary.right_query_for(&document.borrow(), source, request)?;
        let styles = query.styles;
        Ok(Self {
            coverage: request.source_range(),
            style: MarkdownLineComposeStyle {
                presentation: styles,
                points: 0.0, // 原base_index会读取真实语义Font字号，不继承左侧Emoji或隐藏字体的尺寸。
                language: None,
                features: Vec::new(),
            },
            role: Role::from_styles(styles),
            styles,
            fence_separator: false,
            scope: SelectionScope::Body(publication, authority),
            source: source.authority(),
        })
    }
    pub(super) fn literal(
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
        points: f32,
    ) -> Self {
        Self::literal_range(source, request.source_range(), points)
    }
    /// 真实空白控制只有布局，没有可隐藏语法；沿同一冻结 Source 保留普通字体。
    fn literal_range(
        source: &MarkdownLayoutTextSource,
        coverage: Range<usize>,
        points: f32,
    ) -> Self {
        Self {
            coverage,
            style: MarkdownLineComposeStyle {
                presentation: MarkdownPresentationStyles::default(),
                points,
                language: None,
                features: Vec::new(),
            },
            role: Role::Body,
            styles: MarkdownPresentationStyles::default(),
            fence_separator: false,
            scope: SelectionScope::Literal, // Source模式没有语义权限，不构造占位authority。
            source: source.authority(),
        }
    }
    pub(super) fn context_changed(&self, owner: &Owner) -> bool {
        let selection = match &self.scope {
            SelectionScope::Literal => return false,
            SelectionScope::Body(_, authority) => authority.selection,
            SelectionScope::Cell(_, selection) => *selection,
        };
        owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf只绑定原Body文档")
            .borrow()
            .selection()
            != selection
    }

    /// 只消费当前格真实显示目录与原样式请求的交集，不重读 raw Markdown 或借用正文范围。
    pub(super) fn cell(
        owner: &Owner,
        display: Rc<MarkdownTableCellDisplaySource>,
        request: &MarkdownLineStyleRequest,
    ) -> Result<Self> {
        Self::cell_range(owner, display, request.source(), request.source_range())
    }

    /// 格内原换行/空尾行保同一显示 Source；控制没有权限读取邻格或假定普通正文样式。
    pub(super) fn cell_control(
        owner: &Owner,
        display: Rc<MarkdownTableCellDisplaySource>,
        request: &omni_markdown_core::MarkdownLineInlineControl,
    ) -> Result<Self> {
        Self::cell_range(
            owner,
            display,
            request.source(),
            request.source_utf16.clone(),
        )
    }

    /// 样式和控制共用实际显示目录，空 EOF 只授原空范围，不能扩展成默认样式覆盖未知正文。
    fn cell_range(
        owner: &Owner,
        display: Rc<MarkdownTableCellDisplaySource>,
        authority: &MarkdownLayoutTextAuthority,
        range: Range<usize>,
    ) -> Result<Self> {
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("格显示属于原Body")
            .clone();
        let document = document.borrow();
        display.require_for(&document)?;
        if authority != &display.authority() {
            // 相同偏移不能续另一显示源的权，旧格回调必须在这里退场。
            return Err(super::ViewError::Revoked.into());
        }
        let (end, styles) = if range.is_empty() && range.start == display.source().utf16_len() {
            // 真正空尾控制不消费文字，默认样式权限只到这个空范围。
            (range.end, MarkdownPresentationStyles::default())
        } else {
            // 样式只授真实原目录覆盖交集，后续样式变化继续发下一次原 NeedStyle。
            let query = display
                .styles_at(range.start)
                .map_err(super::ViewError::Document)?;
            let Some(query) = query else {
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_cell_unsupported phase=style_missing range_start={} range_end={} display_utf16={}",
                    range.start,
                    range.end,
                    display.source().utf16_len(),
                );
                return Err(Error::Unsupported);
            };
            let end = range.end.min(query.utf16_range.end);
            if end <= range.start {
                // 未覆盖非空文字意味着语义尚不可用，禁止用默认 Body 字体静默继续。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_cell_unsupported phase=style_empty range_start={} range_end={} covered_end={}",
                    range.start, range.end, query.utf16_range.end,
                );
                return Err(Error::Unsupported);
            }
            (end, query.styles)
        };
        Ok(Self {
            coverage: range.start..end,
            style: MarkdownLineComposeStyle {
                presentation: styles,
                points: 0.0,
                language: None,
                features: Vec::new(),
            },
            role: Role::from_styles(styles),
            styles,
            fence_separator: false,
            source: display.authority(),
            scope: SelectionScope::Cell(display, document.selection()),
        })
    }
    pub(super) fn require(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
    ) -> Result<()> {
        self.require_source(owner, source, request.source())
    }
    /// 原控制和原字体请求共享来源核验；控制没有权限凭相邻 part 继承新的样式。
    pub(super) fn require_control(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &omni_markdown_core::MarkdownLineInlineControl,
    ) -> Result<()> {
        self.require_source(owner, source, request.source())
    }
    fn require_source(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<()> {
        source
            .byte_offset_for_utf16(0)
            .map_err(super::ViewError::Document)?;
        if source.authority() != self.source || authority != &self.source {
            return Err(super::ViewError::Revoked.into());
        }
        if let SelectionScope::Cell(display, _) = &self.scope {
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("格语义持原Body文档")
                .clone();
            display.require_for(&document.borrow())?;
            if !self.coverage.is_empty() {
                // Native 字体准备可能跨事件循环，响应前再次核原目录覆盖与样式未撤销。
                let query = display
                    .styles_at(self.coverage.start)
                    .map_err(super::ViewError::Document)?
                    .ok_or(Error::Unsupported)?;
                if query.utf16_range.end < self.coverage.end || query.styles != self.styles {
                    return Err(super::ViewError::Revoked.into());
                }
            } else if self.coverage.start != source.utf16_len() {
                // 空控制只能来自同源真实 EOF，不能替未知区间填一个空样式票。
                return Err(super::ViewError::Revoked.into());
            }
        }
        if let SelectionScope::Body(publication, authority) = &self.scope {
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("RootLeaf只绑定原Body文档")
                .clone();
            let document = document.borrow();
            if self.fence_separator {
                if publication.root_fence_separator_for(&document, *authority, &self.coverage)?
                    != Some(self.coverage.clone())
                {
                    return Err(super::ViewError::Revoked.into());
                }
                return Ok(()); // 原字体回调前后都重新借同一 publication；不改普通叶的覆盖或语义状态。
            }
            let MarkdownOrdinarySemanticState::RootLeafReady(leaf) =
                publication.ordinary_semantics_for(&document, *authority)?
            else {
                return Err(Error::Unsupported);
            };
            let query = leaf
                .query_for(&document, *authority, self.coverage.start)?
                .ok_or(Error::Unsupported)?;
            if query.utf16_range.end < self.coverage.end
                || query.styles != self.styles
                || Role::from_styles(query.styles) != self.role
            {
                return Err(super::ViewError::Revoked.into());
            }
        }
        Ok(()) // 原输入/width/选择票由PreparationTicket在Native前后另核，不从pub数值重建Source。
    }
    pub(super) fn base_index(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        ticket: &PreparationTicket,
    ) -> Result<usize> {
        let Some(slot) = self.role.slot(self.styles) else {
            return Ok(0);
        };
        let cached = owner
            .typography
            .borrow()
            .as_ref()
            .expect("InlinePreview持活动字体角色策略")
            .slots[slot];
        if let Some(index) = cached {
            self.style.points = owner.fonts.borrow()[index].native().point_size() as f32;
            return Ok(index);
        }
        // 取出策略再进入Native，不能让RefCell借用跨NSFont/current回调；关闭由原preparing门串行化。
        let mut typography = owner
            .typography
            .borrow_mut()
            .take()
            .expect("活动策略只有本次装配能借出");
        let result = (|| {
            ticket.require(owner, source)?;
            // 同一语义run完整交给既有字体策略；隐藏标记使用原palette的真实小字号，
            // 不删源字符、不把marker混入标题槽，也不以默认字号覆盖Source授权的显隐。
            // 同一原样式位也随Composer封存part交给CPU画墨迹/装饰；字体只负责真实形状与宽度。
            let native = native_markdown_base_font(self.styles, &typography.palette);
            ticket.require(owner, source)?;
            let current = || ticket.current(owner, source);
            let captured = NativeMarkdownPagedFont::capture(
                native.as_ref(),
                owner.configuration.maximum_font_bytes,
                owner.mtm,
                &current,
            )?;
            let registered = Rc::new(
                NativeMarkdownLineFont::register(captured, &owner.catalog, &current)?
                    .capture_preview_metrics(&typography.metrics_owner, &current)?,
            );
            ticket.require(owner, source)?;
            // 原策略可在系统无法构造斜体时交回基础face；分页不能把该失败缓存成已实现的强调。
            let mut required = objc2_core_text::CTFontSymbolicTraits::empty();
            if self.role != Role::Hidden {
                if self.styles.contains(MarkdownPresentationStyles::EMPHASIS) {
                    required |= objc2_core_text::CTFontSymbolicTraits::TraitItalic;
                }
                if matches!(self.role, Role::Heading(_))
                    || (self.role == Role::Body
                        && self.styles.contains(MarkdownPresentationStyles::STRONG))
                {
                    required |= objc2_core_text::CTFontSymbolicTraits::TraitBold;
                }
                if self.role == Role::Code {
                    required |= objc2_core_text::CTFontSymbolicTraits::TraitMonoSpace;
                }
            }
            if !registered.traits().contains(required) {
                return Err(crate::native_surface::paged_line_fonts::NativeMarkdownLineFontError::StyleUnavailable.into());
            } // 真face不能满足样式时归还候选，不发布普通字体冒充的strong/emphasis/code。
            self.style.points = registered.native().point_size() as f32;
            let index = owner.fonts.borrow().len();
            owner.fonts.borrow_mut().push(registered);
            typography.slots[slot] = Some(index);
            Ok(index)
        })();
        if owner.active.get() {
            *owner.typography.borrow_mut() = Some(typography);
        } // 真关闭不把取出的Native字体策略重新放回业务槽。
        result
    }
}

/// 单一来源续体只沿原Global向前；选区实际换代先撤旧，再建同Doc唯一后继。
enum CellNavigation {
    Required {
        column: usize,
    },
    Inline {
        row: omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableRowSource,
        column: usize,
        started: bool,
    },
}

/// 目标只含这次真实 Row/CellInline 的来源，未完成的视觉位置仍须由父 Body 原目标流程取得。
pub(super) struct CellDestination {
    pub row: VerifiedMarkdownLayoutTableRow,
    pub source: omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource,
}

pub(super) enum CellNavigationStep {
    /// 原字形/CellInline 尚有归属任务，调用方先继续它，不能并发借同一个请求。
    Occupied,
    Pending,
    Ready(CellDestination),
    End {
        terminal: omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableRowSource,
        authority: MarkdownViewportSourceAuthority,
    },
    Boundary,
}

pub(super) struct RootLeaf {
    source: MarkdownLayoutTextAuthority,
    context: u64,
    selection: Option<MarkdownTextSelection>,
    request: Option<MarkdownViewportSourceRequest>,
    ready: Option<Rc<VerifiedMarkdownViewportSources>>,
    window: Range<usize>,
    // 正常来源准备时 ready=None；选择换代后 pending+ready 只暂存原最后发布，
    // 等首次真实样式范围到来再交给 Core 重投影，绝不把它当作新选择的 Ready。
    pending: bool,
    /// 只记本次click真正回退的起点；同一次完整重放后仍未知必须保原错误，不能自动无限重试。
    link_repositioned: Option<usize>,
    /// 当前格来源恢复只允许一次真实根重放；范围只是查询目标，不能替代原结构行票。
    row_repositioned: Option<usize>,
    /// 活动格换行与只读格共用原 Request；此槽只保本次真正查询的结构票，不另建 Global。
    cell_navigation: Option<CellNavigation>,
    pub(super) work: SemanticWork,
}
impl RootLeaf {
    /// 当前原请求已把 CellInline 工作记入同一总账；不能读上次普通叶快照后再重复叠加。
    pub(super) fn current_work(&self) -> SemanticWork {
        self.request
            .as_ref()
            .map_or(self.work, MarkdownViewportSourceRequest::total_work)
    }

    /// 结构格只借原请求内唯一 Global；不能从 raw 格票创建第二份 Parser 或默认语义。
    pub(super) fn advance_cell(
        &mut self,
        preparation: &mut omni_markdown_core::MarkdownTableCellDisplayPreparation,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
    ) -> std::result::Result<
        omni_markdown_core::MarkdownTableCellDisplayPreparationStep,
        MarkdownTableRequestError,
    > {
        preparation.advance_with_source_request(
            document,
            self.request.as_mut().expect("活表格延续原唯一语义请求"),
            false,
        ).inspect_err(|_cause| {
            #[cfg(feature = "native-markdown-runtime-probe")]
            eprintln!(
                "native_root_source_failure stage=cell_display window={:?} selection={:?} total={:?} cause={_cause:?}",
                self.window,
                document.selection(),
                self.current_work(),
            );
        })
    }

    /// 取消由原准备阶段判断归属；只推进已经借出的Inline，Display目录在Core实际关闭。
    pub(super) fn cancel_cell(
        &mut self,
        preparation: &mut omni_markdown_core::MarkdownTableCellDisplayPreparation,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
    ) -> std::result::Result<
        omni_markdown_core::MarkdownTableCellDisplayPreparationStep,
        MarkdownTableRequestError,
    > {
        preparation.cancel_with_source_request(
            document,
            self.request.as_mut().expect("取消格仍由原唯一来源请求拥有"),
        )
    }

    pub(super) fn new(source: &MarkdownLayoutTextSource, context: u64) -> Self {
        Self {
            source: source.authority(),
            context,
            selection: None,
            request: None,
            ready: None,
            window: 0..0,
            pending: false,
            link_repositioned: None,
            row_repositioned: None,
            cell_navigation: None,
            work: SemanticWork::default(),
        }
    }

    /// 已发布字体依赖完整canonical选择；相同正文generation不能给旧隐藏标记续权。
    pub(super) fn selection_changed(&self, selection: MarkdownTextSelection) -> bool {
        self.selection.is_some_and(|old| old != selection)
    }

    /// 旧视口与暂停中的当前行共用此入口；只在原请求最新 publication 上取得真正行票。
    /// 同一 Global 的根重放仍逐量子推进，不是就近 checkpoint 恢复，也不重新发放扫描预算。
    fn row_source(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        position: usize,
    ) -> Result<Option<omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableRowSource>>
    {
        source
            .byte_offset_for_utf16(0)
            .map_err(super::ViewError::Document)?;
        if source.authority() != self.source || self.selection != Some(document.selection()) {
            return Err(super::ViewError::Revoked.into());
        }
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection: document.selection(),
            cancelled: false,
        };
        if self.pending && self.ready.is_some() {
            return Ok(None); // 原样式范围尚未到来，不能用新选择推进旧请求或借旧行票。
        }
        let parser = self.request.as_mut().expect("真实表格沿原唯一来源请求恢复");
        if self.pending {
            // 原在途来源量子先结束，不能为另一个可见行并存第二份 Global 或 publication。
            let step = parser.advance(document, authority, QUANTUM).inspect_err(|_cause| {
                // 验收日志只保真实请求阶段、窗口和总账，不输出正文、路径或链接目标。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=row_source window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
            self.work = parser.total_work();
            if matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                self.ready = Some(Rc::new(
                    parser
                        .take_sources_for(document, authority)?
                        .expect("真实来源发布只移交一次"),
                ));
                self.pending = false;
            }
            return Ok(None);
        }
        let ready = self.ready.as_ref().expect("表格恢复持原最新来源发布");
        if self.window.contains(&position)
            && let Some(row) = ready.table_row_at_for(document, authority, position)?
        {
            self.row_repositioned = None;
            return Ok(Some(row));
        }
        if self.row_repositioned == Some(position) {
            // 同一原查询已经完整重放仍没有实际行，不把裸坐标升级成来源权限或反复重试。
            return Err(Error::Unsupported);
        }
        // 恢复一行只需要行首完整scalar的查询窗；原Scanner仍负责认证整条物理行及H/D来源。
        // 额外带上后方16K会把无关普通尾段塞进256条来源页，并让每格反复重扫同一大表前缀。
        let Some(desired) =
            parser.table_row_source_viewport(document, authority, position, QUANTUM)?
        else {
            self.work = parser.total_work();
            return Ok(None); // 原PositionLocator仍在同一个请求/总账内，不借Doc的8KiB UTF16缓存。
        };
        if desired.start >= self.window.start && desired.end >= self.window.end {
            // 前向行沿最新publication续原Global与真实checkpoint；Core仍核其完整前向覆盖契约。
            parser.continue_forward(document, ready, desired.clone(), authority).inspect_err(|_cause| {
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=row_forward previous={:?} desired={desired:?} total={:?} cause={_cause:?}",
                    self.window,
                    parser.total_work(),
                );
            })?;
        } else {
            parser.reposition(document, ready, desired.clone(), authority).inspect_err(|_cause| {
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=row_reposition previous={:?} desired={desired:?} total={:?} cause={_cause:?}",
                    self.window,
                    parser.total_work(),
                );
            })?;
        } // 只有实际逆向/收窄需求才根重放，不把后方行也送回表头重新申请扫描fuel。
        self.ready = None;
        self.window = desired;
        self.pending = true;
        self.row_repositioned = Some(position);
        Ok(None)
    }

    /// 布局与活动格都先经过上方同一最新出版物，只有真正结构行能转成原 flow 行。
    pub(super) fn table_row(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        position: usize,
    ) -> Result<Option<VerifiedMarkdownLayoutTableRow>> {
        let Some(row) = self.row_source(document, source, position)? else {
            return Ok(None);
        };
        Ok(Some(source.verified_table_layout_row_from_source(
            document,
            row,
            self.authority(document),
        )?))
    }

    fn authority(
        &self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
    ) -> MarkdownViewportSourceAuthority {
        MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection: document.selection(),
            cancelled: false,
        }
    }

    pub(super) fn navigating_cell(&self) -> bool {
        self.cell_navigation.is_some()
    }

    pub(super) fn request_started(&self) -> bool {
        self.request.is_some() && !(self.pending && self.ready.is_some())
    }

    /// 每回合只推进原请求一个实际阶段。同行换列也要取得最新结构来源，不能把旧 Layout 票
    /// 的数值坐标当作 publication 权限；跨行继续原 RequiredRow，真正 End 才返回追加资格。
    pub(super) fn navigate_cell(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        original: &VerifiedMarkdownLayoutTableRow,
        column: usize,
        action: crate::native_surface::paged_text_view::NativeMarkdownPagedCellAction,
    ) -> Result<CellNavigationStep> {
        use crate::native_surface::paged_text_view::NativeMarkdownPagedCellAction as Action;
        use omni_markdown_core::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigation;
        original.require_for(document, source)?;
        let authority = self.authority(document);
        if self.cell_navigation.is_none() {
            let Some(row) = self.row_source(document, source, original.source_range().start)?
            else {
                return Ok(CellNavigationStep::Pending);
            };
            let (header, ordinal, columns) = row.coordinate_for(document, authority)?;
            if (header, ordinal) != original.coordinate() || columns != original.columns() {
                return Err(super::ViewError::Revoked.into());
            }
            // 同一真实列空间内才移动；缺格仍由原 Scanner 生成 implicit-empty 来源。
            let local = match action {
                Action::Next if column + 1 < columns => Some(column + 1),
                Action::Previous if column > 0 => Some(column - 1),
                Action::Cancel => return Err(Error::Unsupported),
                _ => None,
            };
            if let Some(column) = local {
                self.cell_navigation = Some(CellNavigation::Inline {
                    row,
                    column,
                    started: false,
                });
                return Ok(CellNavigationStep::Pending);
            }
            let direction = match action {
                Action::Next => MarkdownTableWindowNavigation::Next,
                Action::Previous => MarkdownTableWindowNavigation::Previous,
                Action::NextRow => MarkdownTableWindowNavigation::NextRow,
                Action::Cancel => unreachable!("取消不发来源导航"),
            };
            let ready = self.ready.as_ref().expect("最新行来自当前出版物");
            let Some(query) = ready.required_row_query_for(document, authority, &row, direction)?
            else {
                return Ok(CellNavigationStep::Boundary); // 真正表头前界，原草稿与选择均不动。
            };
            self.request
                .as_mut()
                .expect("活动格复用原来源请求")
                .continue_forward_with_required_row(
                    document,
                    ready,
                    self.window.clone(),
                    authority,
                    query,
                )?;
            self.ready = None;
            self.pending = true;
            self.cell_navigation = Some(CellNavigation::Required {
                column: match action {
                    Action::Next => 0,
                    Action::Previous => columns - 1,
                    Action::NextRow => column,
                    Action::Cancel => unreachable!("取消不发来源导航"),
                },
            });
            return Ok(CellNavigationStep::Pending);
        }
        self.advance_cell_navigation(document, source)
    }

    /// 提交后的旧位置只作查询；实际新行仍由当前请求重新认证，不能沿旧 raw 票开新草稿。
    pub(super) fn activate_cell(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        position: usize,
        column: usize,
        next_row: bool,
    ) -> Result<CellNavigationStep> {
        if self.cell_navigation.is_none() {
            let Some(row) = self.row_source(document, source, position)? else {
                return Ok(CellNavigationStep::Pending);
            };
            if next_row {
                use omni_markdown_core::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigation;
                let authority = self.authority(document);
                let ready = self.ready.as_ref().expect("新canonical的真实行来源");
                let query = ready
                    .required_row_query_for(
                        document,
                        authority,
                        &row,
                        MarkdownTableWindowNavigation::Next,
                    )?
                    .expect("真实向后查询不是表头前界");
                self.request
                    .as_mut()
                    .expect("新canonical唯一来源请求")
                    .continue_forward_with_required_row(
                        document,
                        ready,
                        self.window.clone(),
                        authority,
                        query,
                    )?;
                self.ready = None;
                self.pending = true;
                self.cell_navigation = Some(CellNavigation::Required { column });
                return Ok(CellNavigationStep::Pending);
            } // 仅真实append receipt触发；新行仍由原RequiredRow重新认证，不按新增字数猜行首。
            self.cell_navigation = Some(CellNavigation::Inline {
                row,
                column,
                started: false,
            });
            return Ok(CellNavigationStep::Pending);
        }
        self.advance_cell_navigation(document, source)
    }

    fn advance_cell_navigation(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
    ) -> Result<CellNavigationStep> {
        use omni_markdown_core::markdown_document_buffer::{
            MarkdownTableCellInlineStep, MarkdownViewportSourceRequiredRowResult,
        };
        let authority = self.authority(document);
        let navigation = self.cell_navigation.take().expect("原导航仍在同一请求");
        match navigation {
            CellNavigation::Required { column } => {
                let parser = self.request.as_mut().expect("原 RequiredRow 仍有 owner");
                let step = parser.advance(document, authority, QUANTUM).inspect_err(|_cause| {
                // 验收日志只保真实请求阶段、窗口和总账，不输出正文、路径或链接目标。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=cell_required window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
                self.work = parser.total_work();
                if !matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                    self.cell_navigation = Some(CellNavigation::Required { column });
                    return Ok(CellNavigationStep::Pending);
                }
                let ready = Rc::new(
                    parser
                        .take_sources_for(document, authority)?
                        .expect("真实来源仅交付一次"),
                );
                let result = ready
                    .required_row_result_for(document, authority)?
                    .expect("原 RequiredRow Ready 必须交真实行或 End");
                self.ready = Some(ready);
                self.pending = false;
                match result {
                    MarkdownViewportSourceRequiredRowResult::Row { row } => {
                        self.cell_navigation = Some(CellNavigation::Inline {
                            row,
                            column,
                            started: false,
                        });
                        Ok(CellNavigationStep::Pending)
                    }
                    MarkdownViewportSourceRequiredRowResult::TableEnd { terminal } => {
                        Ok(CellNavigationStep::End {
                            terminal,
                            authority,
                        })
                    }
                }
            }
            CellNavigation::Inline {
                row,
                column,
                started,
            } => {
                let parser = self.request.as_mut().expect("格语义短借原唯一请求");
                if !started {
                    parser.prepare_cell_inline(document, row.clone(), authority, column)?;
                    self.cell_navigation = Some(CellNavigation::Inline {
                        row,
                        column,
                        started: true,
                    });
                    return Ok(CellNavigationStep::Pending);
                }
                let step = parser.advance_cell_inline(document, authority, QUANTUM).inspect_err(|_cause| {
                // 验收日志只保真实请求阶段、窗口和总账，不输出正文、路径或链接目标。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=cell_inline window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
                self.work = parser.total_work();
                if !matches!(step, MarkdownTableCellInlineStep::Ready { .. }) {
                    self.cell_navigation = Some(CellNavigation::Inline {
                        row,
                        column,
                        started,
                    });
                    return Ok(CellNavigationStep::Pending);
                }
                let inline = parser
                    .take_cell_inline_ready_for(document, authority)?
                    .expect("真实 CellInline Ready 只移交一次");
                let raw = inline.cell_source_for(document, authority)?.clone();
                let row = source.verified_table_layout_row_from_source(document, row, authority)?;
                Ok(CellNavigationStep::Ready(CellDestination {
                    row,
                    source: raw,
                }))
            }
        }
    }

    /// 取消只停止接纳后继格，原唯一请求已经启动的工作仍须真正交回。
    /// RequiredRow 取得真实 Row/End 后立即停止；不为了清理再启动一次 CellInline。
    pub(super) fn drain_cell_navigation(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
    ) -> Result<bool> {
        use omni_markdown_core::markdown_document_buffer::MarkdownTableCellInlineStep;

        source
            .byte_offset_for_utf16(0)
            .map_err(super::ViewError::Document)?;
        if self.cell_navigation.is_none() && !self.pending {
            return Ok(true); // 尚未借来源的业务意图没有外部任务可撤，不创建请求来证明空闲。
        }
        if source.authority() != self.source || self.selection != Some(document.selection()) {
            return Err(super::ViewError::Revoked.into());
        }
        if self.pending && self.ready.is_some() && self.cell_navigation.is_none() {
            // 同源健康和新完整选择均已核；pending+ready 只等首次新样式范围，没有格作业可排空。
            // 取消导航不应把新选择交给仍待重投影的旧请求，更不能撤掉原 Composer 的准备。
            return Ok(true);
        }
        let authority = self.authority(document);
        let navigation = self.cell_navigation.take();
        match navigation {
            Some(CellNavigation::Inline { started: false, .. }) => Ok(true), // 这里只持已认证行票，原 parser 尚未启动格任务，直接归还票即可。
            Some(CellNavigation::Inline {
                row,
                column,
                started: true,
            }) => {
                let parser = self.request.as_mut().expect("原 CellInline 仍有唯一 owner");
                let step = parser.advance_cell_inline(document, authority, QUANTUM).inspect_err(|_cause| {
                // 验收日志只保真实请求阶段、窗口和总账，不输出正文、路径或链接目标。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=cell_drain_inline window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
                self.work = parser.total_work();
                if !matches!(step, MarkdownTableCellInlineStep::Ready { .. }) {
                    self.cell_navigation = Some(CellNavigation::Inline {
                        row,
                        column,
                        started: true,
                    });
                    return Ok(false);
                }
                // take 才真正释放请求的单格占用；丢弃结果，不转换 Display 或开启可写草稿。
                drop(
                    parser
                        .take_cell_inline_ready_for(document, authority)?
                        .expect("原 CellInline Ready 只归还一次"),
                );
                Ok(true)
            }
            navigation => {
                // 这里包括原 RequiredRow，以及导航取得原行前尚在途的普通来源根重放。
                // 两者都消费同一批真实出版物；仅 RequiredRow 额外消费它实际的 Row/End 结果。
                let parser = self.request.as_mut().expect("原来源量子仍有唯一 owner");
                let step = parser.advance(document, authority, QUANTUM).inspect_err(|_cause| {
                // 验收日志只保真实请求阶段、窗口和总账，不输出正文、路径或链接目标。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=cell_drain_source window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
                self.work = parser.total_work();
                if !matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                    self.cell_navigation = navigation;
                    return Ok(false);
                }
                let ready = Rc::new(
                    parser
                        .take_sources_for(document, authority)?
                        .expect("真实来源出版物只归还一次"),
                );
                if matches!(navigation, Some(CellNavigation::Required { .. })) {
                    ready
                        .required_row_result_for(document, authority)?
                        .expect("原 RequiredRow 必须交回真实 Row 或 End");
                }
                self.ready = Some(ready);
                self.pending = false;
                Ok(true)
            }
        }
    }

    /// 原行票核身份，最新发布再授格准备；旧行恢复改变游标后，暂停行不能借旧发布启动 CellInline。
    pub(super) fn prepare_cell(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        expected: &VerifiedMarkdownLayoutTableRow,
        column: usize,
    ) -> Result<Option<omni_markdown_core::MarkdownTableCellDisplayPreparation>> {
        expected.require_for(document, source)?;
        let Some(row) = self.table_row(document, source, expected.source_range().start)? else {
            return Ok(None);
        };
        if row.source_range() != expected.source_range()
            || row.coordinate() != expected.coordinate()
            || row.columns() != expected.columns()
        {
            // 只有同源同物理行可续当前格，查询命中另一结构不能作为旧行的替代来源。
            return Err(super::ViewError::Revoked.into());
        }
        Ok(Some(row.prepare_cell(document, source, column)?))
    }

    /// 原 Plain 前缀或真实 Root Code 能力覆盖整个 Composer 库存时，才保留同源字形。
    /// 能力借用、Composer 只读核验与旧发布退休都在同一纯 Core 栈中完成，没有 Native 回调。
    pub(super) fn reproject_selection_preserving_styles(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        selection: MarkdownTextSelection,
        composer: &MarkdownLineComposer,
        ticket: &PreparationTicket,
    ) -> Result<bool> {
        ticket.require_pure(owner, source)?;
        if source.authority() != self.source {
            return Err(super::ViewError::Revoked.into());
        }
        if self.pending || self.cell_navigation.is_some() {
            return Ok(false); // 原请求/格作业尚在途，仍由原重排路径取消并等待真实归还。
        }
        let (Some(parser), Some(previous)) = (self.request.as_mut(), self.ready.as_ref()) else {
            return Ok(false);
        };
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf只借原Body文档")
            .clone();
        let document = document.borrow();
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection,
            cancelled: false,
        };
        {
            // cap 不是裸 Range，也不保旧 publication 的强副本；它借原请求/Doc/最新发布，
            // 认证 Plain 已读前缀或同一 Root Code 正文内的选择移动。后者不重用旧
            // marked/选区像素：旧 Scene/Reader 仍退休，后继帧须匹配完整新 Input。
            // Composer 自己核 Stage/Store/活跃支持范围，
            // 不能由 Native 的 SceneReady 或已显示行数猜测其尚未捕获的整形输入。
            let Some(capability) =
                parser.selection_style_invariant(&document, previous, authority)?
            else {
                return Ok(false);
            };
            if !composer.can_retain_selection_styles(&capability)? {
                return Ok(false);
            }
        } // 借用先结束，才允许原唯一请求改变选择、退休 publication；没有跨 Native 或挂起保存 cap。
        ticket.require_pure(owner, source)?;
        if !parser.reproject_selection(&document, previous, self.window.clone(), authority)? {
            return Ok(false); // Core 资格不满足保证原 owner/发布不变，交原 reset_styles 冷路径。
        }
        self.work = parser.total_work();
        self.ready = None; // 旧发布此刻已永久退休，不等下一 NeedStyle，目标可能已经落在保留前缀内。
        self.pending = true;
        self.selection = Some(selection);
        self.link_repositioned = None;
        self.row_repositioned = None;
        // 后继样式必须由新选择正常 advance 生成的新发布提供，不能借旧 Boundary 给新 Input 补票。
        ticket.require_pure(owner, source)?;
        Ok(true)
    }

    /// 旧字形图已真实结束后接纳新选择；只有已移交输出的原请求可尝试保同源结构。
    /// 实际新窗口由后继首次 StyleRequest 决定，不能猜一个可能截断 GCB 的范围。
    pub(super) fn reset_selection(&mut self, selection: MarkdownTextSelection) {
        if !self.pending
            && self.request.is_some()
            && self.ready.is_some()
            && self.cell_navigation.is_none()
        {
            // pending+ready 是同一原槽的待重投影意图；旧发布仍按旧完整选择核权，
            // 所有查询先受 pending 阻断。Core 接受或拒绝后才退还这枚最后 publication。
            self.pending = true;
            self.link_repositioned = None;
            self.row_repositioned = None;
            self.cell_navigation = None;
        } else {
            self.clear(); // 活动来源/格作业或再次换选择仍走原取消、释放与冷准备。
        }
        self.selection = Some(selection);
    }

    /// 目标只借当前原Source/publication的解析事实；窗口外明确未准备，不能据LINK位猜URL。
    pub(super) fn link_at(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        index: usize,
    ) -> std::result::Result<Option<omni_markdown_core::MarkdownSourceLink>, super::ViewError> {
        source
            .byte_offset_for_utf16(0)
            .map_err(super::ViewError::Document)?;
        if source.authority() != self.source {
            return Err(super::ViewError::Revoked);
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf链接只绑定原Body文档")
            .clone();
        let document = document.borrow();
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection: document.selection(),
            cancelled: false,
        };
        if self.pending && self.ready.is_some() {
            if self.selection != Some(authority.selection) {
                return Err(super::ViewError::Revoked);
            }
            return Err(super::ViewError::Pending); // 原最后发布不能因 Native 已接纳新选择而获得新权限。
        }
        let ready = self.ready.as_ref().ok_or(super::ViewError::Pending)?;
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = ready
            .ordinary_semantics_for(&document, authority)
            .map_err(super::ViewError::LinkSource)?
        else {
            return Err(super::ViewError::Pending);
        };
        // 原有限闭合块的 Parser 已经逐项准入完整 label 与解码目标；正链接事实
        // 可以覆盖当前样式交集以外的同块文字，但不能据此扩张字体、绘图或“无链接”权限。
        match leaf.link_at_for(&document, authority, index) {
            Ok(link) => Ok(link.cloned()),
            Err(MarkdownTableRequestError::Window(MarkdownTableWindowError::ContextPending))
                if !self.window.contains(&index) =>
            {
                Err(super::ViewError::Pending)
            } // 没有正证据的窗外查询仍明确未准备，后继真实 click 必须推进唯一原来源。
            Err(cause) => Err(super::ViewError::LinkSource(cause)),
        }
    }

    /// 旧sealed Scene回查只重定位原唯一请求；推进与安装分别占后继回合，不复制Source或保全源URL表。
    pub(super) fn advance_link(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        index: usize,
        ticket: &PreparationTicket,
    ) -> Result<Option<Option<omni_markdown_core::MarkdownSourceLink>>> {
        self.row_repositioned = None; // 链接接管唯一来源游标，之前取消的旧行意图到此结束。
        ticket.require_pure(owner, source)?;
        if source.authority() != self.source || index >= source.utf16_len() {
            return Err(super::ViewError::Revoked.into());
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf链接恢复只绑定原Body文档")
            .clone();
        let document = document.borrow();
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection: document.selection(),
            cancelled: false,
        };
        if self.selection != Some(authority.selection) {
            return Err(super::ViewError::Revoked.into()); // 不能借click对选择变化后的旧语义图重新授权。
        }
        if self.pending && self.ready.is_some() {
            return Ok(None); // 等待原 Composer 给出新样式范围，不把旧完整选择的请求推进成失败。
        }
        if self.pending {
            let parser = self.request.as_mut().expect("pending持唯一来源请求");
            let step = parser.advance(&document, authority, QUANTUM).inspect_err(|_cause| {
                // 验收日志只保真实请求阶段、窗口和总账，不输出正文、路径或链接目标。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=link_source window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
            self.work = parser.total_work();
            if matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                self.ready = Some(Rc::new(
                    parser
                        .take_sources_for(&document, authority)?
                        .expect("SourcesAvailable持唯一Ready"),
                ));
                self.pending = false;
            }
            return Ok(None); // 一个量子只推进原Parser；不同时创建下一份publication或进行Native副作用。
        }
        // 先消费同一真实 publication 已持有的正目标；窗口边界只约束未知/负证明，
        // 不能丢弃原 Parser 已验证且已付费的另一个 label，也不为它重放整个来源。
        match self.link_at(owner, source, index) {
            Ok(link) => {
                self.link_repositioned = None;
                return Ok(Some(link));
            }
            Err(super::ViewError::Pending) if !self.window.contains(&index) => {} // 未知窗外仍沿原一单位恢复；窗内缺事实保原失败，不能无限重放。
            Err(
                cause @ super::ViewError::LinkSource(MarkdownTableRequestError::Window(
                    MarkdownTableWindowError::ContextPending,
                )),
            ) => {
                if self.link_repositioned == Some(index) {
                    return Err(cause.into()); // 同一真实起点已完成重放仍无覆盖；保typed Pending，不造空target或重复重扫。
                }
            }
            Err(cause) => return Err(cause.into()), // I/O、撤权和原解析首因均不具备回退重试权限。
        }
        let ready = self.ready.as_ref().ok_or(super::ViewError::Pending)?;
        // 链接是原命中点的查询，不需要把后方16KiB表格和其它块一起请求成语义窗口。
        // 这里的一单位只限定查询覆盖；原Core仍从同Global的真实闭合block取得完整Parser、
        // references和HTML上下文，返回完整label/destination，不截断Markdown链接或另建来源。
        // 命中票的index为真实GCB起点；若是emoji，原端点定位器仍按既有规则认证完整scalar。
        let desired = index..source.utf16_len().min(index + 1);
        self.request
            .as_mut()
            .expect("已发布RootLeaf仍持同一来源续体")
            .reposition(&document, ready, desired.clone(), authority)?;
        self.ready = None; // 请求已经接管重定位，先退旧Ready再让后继量子装配新有限语义工作区。
        self.window = desired;
        self.pending = true;
        self.link_repositioned = Some(index);
        Ok(None) // 原数值window内的coverage空洞也必须沿同一请求恢复真实起点，不能签成无链接。
    }
    pub(super) fn select(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
        ticket: &PreparationTicket,
    ) -> Result<Option<StyleDecision>> {
        self.select_range(
            owner,
            source,
            request.source(),
            request.source_range(),
            None,
            ticket,
        )
    }

    /// 原 LF/CRLF/Tab 也从唯一 publication 查询；空逻辑行只在真实行树证明后保留字面默认值。
    pub(super) fn select_control(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &omni_markdown_core::MarkdownLineInlineControl,
        ticket: &PreparationTicket,
    ) -> Result<Option<Selection>> {
        match self.select_range(
            owner,
            source,
            request.source(),
            request.source_utf16.clone(),
            Some(request.kind),
            ticket,
        )? {
            None => Ok(None),
            Some(StyleDecision::Font(selection)) => Ok(Some(selection)),
            Some(StyleDecision::Table(_)) => Err(Error::Unsupported),
        }
    }

    fn select_range(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        range: Range<usize>,
        control: Option<omni_markdown_core::MarkdownLineInlineControlKind>,
        ticket: &PreparationTicket,
    ) -> Result<Option<StyleDecision>> {
        ticket.require_pure(owner, source)?;
        self.link_repositioned = None; // 原Composer重新接管语义，后继click属于另一次实际查询，不继承旧手势的恢复记录。
        self.row_repositioned = None; // 同样结束先前旧行恢复的单次重放记录，后继滚回仍是新的真实需求。
        if source.authority() != self.source || authority != &self.source {
            return Err(super::ViewError::Revoked.into());
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("InlinePreview装配只接受Body原Doc")
            .clone();
        let document = document.borrow();
        // 只有下方真正缺少语义覆盖的控制才需要证明整条逻辑行为空。
        // 普通已覆盖控制、首工厂和在途量子不消费这个事实，不能每轮重复解码行树的 Unicode 计量。
        // 延迟查询仍沿原 CRLF/尾空行定义，不另写行解析器或保存跨请求缓存。
        let is_blank_control = || -> Result<bool> {
            Ok(control.is_some()
                && super::producer::source_line_range_at_head(&document, range.start)
                    .map_err(super::ViewError::Document)?
                    == range)
        };
        let line_break = matches!(
            control,
            Some(
                omni_markdown_core::MarkdownLineInlineControlKind::CarriageReturn
                    | omni_markdown_core::MarkdownLineInlineControlKind::LineFeed
            )
        ); // 控制种类来自原 Composer nonce；只对真实 CR/LF 查询同源 fence separator。
        let selection = document.selection();
        if self.selection.is_some_and(|old| old != selection) {
            self.clear(); // 旧选择的publication无法续权；不捕第二Source或并存两个解析前沿。
        }
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection,
            cancelled: false,
        };
        let desired = range.start..range.end.min(range.start.saturating_add(WINDOW_UTF16));
        if self.pending && self.ready.is_some() {
            // 这里只移交原已消费输出的请求和它最后一枚发布；新范围来自真实 Style/Control，
            // 新完整选择来自当前 Doc。Core 自己核同源、闭合记录和 prefix 能力，不据默认样式猜资格。
            let parser = self.request.as_mut().expect("待重投影仍持原唯一来源请求");
            let previous = self.ready.as_ref().expect("待重投影只借原最后发布");
            let reprojected =
                parser.reproject_selection(&document, previous, desired.clone(), authority)?;
            self.work = parser.total_work(); // 固定控制准备与真正析构费用仍计入原请求累计账。
            if reprojected {
                self.ready = None; // Core 已撤旧 publication；不保旧 Ready/Reader 或给旧 Font 响应补票。
                self.window = desired;
                return Ok(None); // 重投影控制与后继来源 advance 分回合，不重新领取或隐藏原步骤额度。
            }
            // false 只表示原有限记录/已证前缀不足；Core 保证未改 owner。
            // I/O、取消、旧发布或完整选择失权均由上面的 Err 原样退出，不能降为冷准备成功。
            self.clear();
            self.selection = Some(selection);
        }
        if self.request.is_none() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                eprintln!(
                    "native_root_source_window stage=style_create requested={range:?} desired={desired:?} selection={selection:?} source_units={}",
                    source.utf16_len(),
                );
            } // 只记录真正开窗，不在每个Font/GCB请求中重复输出。
            self.request = Some(document.viewport_leaf_source_request(
                desired.clone(),
                self.context,
                selection,
            )?);
            self.selection = Some(selection);
            self.window = desired;
            self.pending = true;
            // 工厂只绑定原 root、准入固定库存，不读取正文；下面仍只执行一次原来源步。
            // 固定控制准备另有真实费用，不把本回合全部 metadata 宣称为至多4096。
        } else if let Some(parser) = self.request.as_mut()
            && !self.pending
            && !self.window.contains(&range.start)
        {
            let ready = self.ready.as_ref().expect("完成来源持同一publication");
            #[cfg(feature = "native-markdown-runtime-probe")]
            if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                eprintln!(
                    "native_root_source_window stage=style_retarget previous={:?} requested={range:?} desired={desired:?} selection={selection:?}",
                    self.window,
                );
            }
            let backward = range.start < self.window.start;
            if backward {
                parser
                    .reposition(&document, ready, desired.clone(), authority)
                    .inspect_err(|_cause| {
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        eprintln!(
                            "native_root_source_failure stage=style_reposition previous={:?} desired={desired:?} total={:?} cause={_cause:?}",
                            self.window,
                            parser.total_work(),
                        );
                    })?;
            } else {
                parser
                    .continue_forward(&document, ready, desired.clone(), authority)
                    .inspect_err(|_cause| {
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        eprintln!(
                            "native_root_source_failure stage=style_forward previous={:?} desired={desired:?} total={:?} cause={_cause:?}",
                            self.window,
                            parser.total_work(),
                        );
                    })?;
            }
            self.ready = None; // click可把唯一语义游标移向旧视口；后继Composer仍按真实来源重新准备其需要的窗口。
            self.window = desired;
            self.pending = true;
            if backward {
                // 逆向先真实重置 Global 与旧 owner；保原独立控制回合，不能再挤入新 Leaf 构造。
                return Ok(None);
            }
            // 前向仅迁移原 publication 的有限目录与最新 quota；累计 fuel 从未重置，
            // 接着推进同一请求一次，避免每个短样式窗口为纯准备多走一次 Native 调度。
        }
        if self.pending {
            let parser = self.request.as_mut().expect("pending持原来源请求");
            let step = parser.advance(&document, authority, QUANTUM).inspect_err(|_cause| {
                // 验收日志只保真实请求阶段、窗口和总账，不输出正文、路径或链接目标。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=style_source window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
            self.work = parser.total_work(); // 含源读取/复制/Parser输入的原真实累计账，不混成Composer字节0。
            if matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                self.ready = Some(Rc::new(
                    parser
                        .take_sources_for(&document, authority)?
                        .expect("SourcesAvailable持唯一Ready"),
                ));
                self.pending = false;
            }
            return Ok(None); // 每次只 advance 一次；即使已 Ready，也不合并样式查询、Font 或 Composer。
        }
        let ready = self.ready.as_ref().expect("完成来源持同一publication");
        if line_break
            && let Some(separator) = ready.root_fence_separator_for(&document, authority, &range)?
        {
            let mut selection = Selection::literal_range(source, separator, 0.0);
            selection.fence_separator = true;
            selection.scope = SelectionScope::Body(ready.clone(), authority);
            return Ok(Some(StyleDecision::Font(selection))); // 完整 Parser 没给 closing separator 代码样式；原 Root 票单独证明它。
        }
        if control.is_none()
            && let Some(row) = ready.table_row_at_for(&document, authority, range.start)?
        {
            let row = source.verified_table_layout_row_from_source(&document, row, authority)?;
            if row.source_range().start != range.start {
                return Err(Error::Unsupported);
            } // 原 NeedStyle 必须正处于真实块起点，不能在已消费的半行里事后跳过正文。
            return Ok(Some(StyleDecision::Table(row)));
        } // TABLE 样式位、同值范围及离屏 header context 均不能补造可绘制行。
        let state = ready.ordinary_semantics_for(&document, authority)?;
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = state else {
            let blank = is_blank_control()?;
            if blank {
                return Ok(Some(StyleDecision::Font(Selection::literal_range(
                    source, range, 0.0,
                ))));
            } // 只为真实空逻辑行保留控制高度；普通字形的未知区间仍明确拒绝。
            #[cfg(feature = "native-markdown-runtime-probe")]
            eprintln!(
                "native_root_leaf_missing_semantics range={range:?} window={:?} blank={blank} line_break={line_break} state={}",
                self.window,
                match state {
                    MarkdownOrdinarySemanticState::SourcesOnly => "sources-only".into(),
                    MarkdownOrdinarySemanticState::Pending(reason) =>
                        format!("plain-pending:{reason:?}"),
                    MarkdownOrdinarySemanticState::LeafPending(reason) =>
                        format!("leaf-pending:{reason:?}"),
                    MarkdownOrdinarySemanticState::PlainReady(_) => "plain-ready".into(),
                    MarkdownOrdinarySemanticState::RootLeafReady(_) => unreachable!(),
                }
            );
            return Err(Error::Unsupported); // SourcesOnly/未知inline不签默认Body样式。
        };
        let query = match leaf.query_for(&document, authority, range.start)? {
            Some(query) => query,
            None => {
                // 未覆盖才核原完整控制行；真正可见的字形仍不得借默认样式绕过原 Parser。
                let blank = is_blank_control()?;
                if blank {
                    return Ok(Some(StyleDecision::Font(Selection::literal_range(
                        source, range, 0.0,
                    ))));
                }
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_leaf_uncovered_query range={range:?} window={:?} blank={blank} line_break={line_break} coverage={:?}",
                    self.window,
                    leaf.covered_ranges_for(&document, authority)?
                        .collect::<Vec<_>>()
                );
                return Err(Error::Unsupported);
            }
        };
        let end = range.end.min(query.utf16_range.end);
        if end <= range.start {
            return Err(Error::Unsupported);
        }
        Ok(Some(StyleDecision::Font(Selection {
            coverage: range.start..end,
            style: MarkdownLineComposeStyle {
                presentation: query.styles,
                points: 0.0,
                language: None,
                features: Vec::new(),
            },
            role: Role::from_styles(query.styles),
            styles: query.styles,
            fence_separator: false,
            scope: SelectionScope::Body(ready.clone(), authority),
            source: self.source.clone(),
        })))
    }
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn publication(&self) -> Option<std::rc::Weak<VerifiedMarkdownViewportSources>> {
        if self.pending {
            None // 待重投影的旧 publication 仍属旧选择，不发布为当前图的观察票。
        } else {
            self.ready.as_ref().map(Rc::downgrade)
        }
    }

    fn clear(&mut self) {
        if let Some(request) = self.request.as_mut() {
            request.cancel();
        }
        self.request = None;
        self.ready = None;
        self.selection = None;
        self.pending = false;
        self.link_repositioned = None;
        self.row_repositioned = None;
        self.cell_navigation = None;
    }
}
impl Drop for RootLeaf {
    fn drop(&mut self) {
        self.clear();
    } // 关闭真正释放语义续体/Ready；不等迟到Native或worker回调清槽。
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// 实际sealed part的有限诊断事实；不返回Source/Font/Reader强边或另造行身份。
pub(in crate::native_surface) struct RootLeafPart {
    pub source: Range<usize>,
    /// 原封存glyph的总推进；与CTFont自身的原生advance不同，实际Core整形也必须独立核对。
    pub advance_26_6: i64,
    pub script: [u8; 4],
    pub direction: omni_markdown_core::MarkdownShapingDirection,
    /// 仅完整采集至多256字形的真实part；超出时明确未采，不截前缀冒充完整字形域。
    /// 这些Copy值不保活Reader/Source/Font，也不改变原观察入口可支持的part范围。
    pub glyphs: Option<Vec<omni_markdown_core::MarkdownShapedGlyph>>,
    pub presentation: MarkdownPresentationStyles,
    pub points: f32,
    pub native_points: f64,
    pub role: &'static str,
    pub traits: objc2_core_text::CTFontSymbolicTraits,
    pub metrics: crate::native_surface::paged_font_geometry::NativeMarkdownPagedFontMetricValues,
    pub font_owner: std::rc::Weak<NativeMarkdownLineFont>,
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl Typography {
    pub(super) fn role_at(&self, index: usize) -> &'static str {
        if self.slots[40] == Some(index) {
            "hidden"
        } else {
            // 槽是有限字体组合事实；下标只改变字号，仍归原基础角色，不从PSName猜字体用途。
            match self.slots.iter().position(|slot| *slot == Some(index)) {
                Some(slot) if (4..16).contains(&(slot % 20)) => "heading",
                Some(slot) if (16..18).contains(&(slot % 20)) => "code",
                Some(slot) if (18..20).contains(&(slot % 20)) => "table",
                _ => "body-or-fallback",
            }
        }
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl Owner {
    /// 实际可见窗口的行观测只复制有限标量；不借 Producer 样式期望反造几何 oracle。
    pub(in crate::native_surface) fn probe_root_leaf_lines(
        &self,
    ) -> Result<Vec<crate::native_surface::preview_paragraph::PreviewLineObservation>> {
        let scene = self.scene.borrow().clone().ok_or(Error::Busy)?;
        scene.probe_line_geometry().map_err(Error::from)
    }
    /// 固定最多32行/256part，读取原Reader和实际注册Font；不从语义期望反造诊断结果。
    /// 只保metadata弱票，实测Body关闭确实释放Ready；不为测试延长任何Source或Native生命。
    pub(in crate::native_surface) fn probe_root_leaf_publication(
        &self,
    ) -> Result<std::rc::Weak<VerifiedMarkdownViewportSources>> {
        self.producer
            .borrow()
            .as_ref()
            .and_then(|producer| producer.root_leaf_publication())
            .ok_or(Error::Busy)
    }

    pub(in crate::native_surface) fn probe_root_leaf_parts(&self) -> Result<Vec<RootLeafPart>> {
        self.producer
            .borrow()
            .as_ref()
            .ok_or(Error::Busy)?
            .root_leaf_parts(self)
    }
}
