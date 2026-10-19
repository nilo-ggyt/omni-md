//! 同一Body输入的RootLeaf字体接入；只消费原有限publication，不隐藏语法或另建正文。

use std::{ops::Range, ptr::NonNull, rc::Rc};

use objc2::rc::Retained;
use objc2_app_kit::NSFont;
use objc2_core_foundation::CFRetained;
use objc2_core_text::CTFont;
use omni_markdown_core::{
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource, MarkdownLineComposeStyle,
    MarkdownLineStyleRequest, MarkdownOrdinarySemanticState, MarkdownPresentationStyles,
    MarkdownTextSelection, MarkdownViewportSourceAuthority, MarkdownViewportSourceRequest,
    MarkdownViewportSourceStep, VerifiedMarkdownViewportSources,
};

use omni_markdown_core::markdown_document_buffer::SemanticWork;

use super::preparation::PreparationTicket;
use super::{NativeMarkdownPagedBody as Owner, NativeMarkdownPagedBodyError as Error, Result};
use crate::native_surface::{
    paged_fonts::NativeMarkdownPagedFont,
    paged_line_fonts::NativeMarkdownLineFont,
    text_attributes::{MarkdownTextAttributePalette, native_markdown_base_font},
};

const WINDOW_UTF16: usize = 16_384;
const QUANTUM: usize = 4096;

/// 角色只为当前活动Body固定七槽；不按Source长度、样式请求数或PSName增加缓存。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Role {
    Body,
    Heading(usize),
    Code,
}
impl Role {
    fn from_styles(styles: MarkdownPresentationStyles) -> Self {
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
        if styles.contains(MarkdownPresentationStyles::FENCED_CODE) {
            Self::Code
        } else {
            Self::Body // 已认证的Rule等仍按原普通字体显示字面，不冒充已完成背景/隐藏预览。
        }
    }
    fn slot(self) -> Option<usize> {
        match self {
            Self::Body => None,
            Self::Heading(index) => Some(index),
            Self::Code => Some(6),
        }
    }
}

/// 原属性策略直接选择真实NSFont；Native/Core捕获仍由既有精确face/axes入口完成。
pub(super) struct Typography {
    palette: MarkdownTextAttributePalette,
    slots: [Option<usize>; 7],
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
            slots: [None; 7],
        }
    }
}

/// 在途试次固定原样式、coverage与publication；原selection变更不能给旧结果补票。
pub(super) struct Selection {
    pub(super) coverage: Range<usize>,
    pub(super) style: MarkdownLineComposeStyle,
    role: Role,
    styles: MarkdownPresentationStyles,
    publication: Option<(
        Rc<VerifiedMarkdownViewportSources>,
        MarkdownViewportSourceAuthority,
    )>,
    source: MarkdownLayoutTextAuthority,
}
impl Selection {
    pub(super) fn literal(
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
        points: f32,
    ) -> Self {
        Self {
            coverage: request.source_range(),
            style: MarkdownLineComposeStyle {
                points,
                language: None,
                features: Vec::new(),
            },
            role: Role::Body,
            styles: MarkdownPresentationStyles::default(),
            publication: None, // Source模式没有语义权限，不构造占位authority。
            source: source.authority(),
        }
    }
    pub(super) fn context_changed(&self, owner: &Owner) -> bool {
        self.publication.as_ref().is_some_and(|(_, authority)| {
            owner
                .document
                .borrow()
                .as_ref()
                .expect("RootLeaf只绑定原Body文档")
                .borrow()
                .selection()
                != authority.selection
        })
    }
    pub(super) fn require(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
    ) -> Result<()> {
        source
            .byte_offset_for_utf16(0)
            .map_err(super::ViewError::Document)?;
        if source.authority() != self.source || request.source() != &self.source {
            return Err(super::ViewError::Revoked.into());
        }
        if let Some((publication, authority)) = &self.publication {
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("RootLeaf只绑定原Body文档")
                .clone();
            let document = document.borrow();
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
        let Some(slot) = self.role.slot() else {
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
            // 本片始终显示原marker；去掉隐藏位后沿原base_font优先级/字号，不使用0.1pt隐藏字体。
            let native = native_markdown_base_font(
                self.styles
                    .without(MarkdownPresentationStyles::HIDDEN_MARKER),
                &typography.palette,
            );
            ticket.require(owner, source)?;
            let current = || ticket.current(owner, source);
            let captured = NativeMarkdownPagedFont::capture(
                native.as_ref(),
                owner.configuration.maximum_font_bytes,
                owner.mtm,
                &current,
            )?;
            let registered = Rc::new(NativeMarkdownLineFont::register(
                captured,
                &owner.catalog,
                &current,
            )?);
            ticket.require(owner, source)?;
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
pub(super) struct RootLeaf {
    source: MarkdownLayoutTextAuthority,
    context: u64,
    selection: Option<MarkdownTextSelection>,
    request: Option<MarkdownViewportSourceRequest>,
    ready: Option<Rc<VerifiedMarkdownViewportSources>>,
    window: Range<usize>,
    pending: bool,
    pub(super) work: SemanticWork,
}
impl RootLeaf {
    pub(super) fn new(source: &MarkdownLayoutTextSource, context: u64) -> Self {
        Self {
            source: source.authority(),
            context,
            selection: None,
            request: None,
            ready: None,
            window: 0..0,
            pending: false,
            work: SemanticWork::default(),
        }
    }
    pub(super) fn select(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
        ticket: &PreparationTicket,
    ) -> Result<Option<Selection>> {
        ticket.require_pure(owner, source)?;
        if source.authority() != self.source || request.source() != &self.source {
            return Err(super::ViewError::Revoked.into());
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("InlinePreview装配只接受Body原Doc")
            .clone();
        let document = document.borrow();
        let selection = document.selection();
        if self.selection.is_some_and(|old| old != selection) {
            self.clear(); // 旧选择的publication无法续权；不捕第二Source或并存两个解析前沿。
        }
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection,
            cancelled: false,
        };
        let range = request.source_range();
        let desired = range.start..range.end.min(range.start.saturating_add(WINDOW_UTF16));
        if self.request.is_none() {
            self.request = Some(document.viewport_leaf_source_request(
                desired.clone(),
                self.context,
                selection,
            )?);
            self.selection = Some(selection);
            self.window = desired;
            self.pending = true;
            return Ok(None); // 工厂只装配原请求，真实推进留给后继独立4096量子。
        }
        if self.pending {
            let parser = self.request.as_mut().expect("pending持原来源请求");
            let step = parser.advance(&document, authority, QUANTUM)?;
            self.work = parser.total_work(); // 含源读取/复制/Parser输入的原真实累计账，不混成Composer字节0。
            if matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                self.ready = Some(Rc::new(
                    parser
                        .take_sources_for(&document, authority)?
                        .expect("SourcesAvailable持唯一Ready"),
                ));
                self.pending = false;
            }
            return Ok(None); // 一量子只推进同一个语义任务，不同时再做Font或Composer工作。
        }
        let ready = self.ready.as_ref().expect("完成来源持同一publication");
        if range.start >= self.window.end {
            self.request
                .as_mut()
                .expect("forward复用唯一来源续体")
                .continue_forward(&document, ready, desired.clone(), authority)?;
            self.window = desired;
            self.pending = true;
            return Ok(None);
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) =
            ready.ordinary_semantics_for(&document, authority)?
        else {
            return Err(Error::Unsupported); // SourcesOnly/未知inline不签默认Body样式。
        };
        let query = leaf
            .query_for(&document, authority, range.start)?
            .ok_or(Error::Unsupported)?;
        let end = range.end.min(query.utf16_range.end);
        if end <= range.start {
            return Err(Error::Unsupported);
        }
        Ok(Some(Selection {
            coverage: range.start..end,
            style: MarkdownLineComposeStyle {
                points: 0.0,
                language: None,
                features: Vec::new(),
            },
            role: Role::from_styles(query.styles),
            styles: query.styles,
            publication: Some((ready.clone(), authority)),
            source: self.source.clone(),
        }))
    }
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn publication(&self) -> Option<std::rc::Weak<VerifiedMarkdownViewportSources>> {
        self.ready.as_ref().map(Rc::downgrade)
    }

    fn clear(&mut self) {
        if let Some(request) = self.request.as_mut() {
            request.cancel();
        }
        self.request = None;
        self.ready = None;
        self.selection = None;
        self.pending = false;
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
    pub points: f32,
    pub native_points: f64,
    pub role: &'static str,
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl Typography {
    pub(super) fn role_at(&self, index: usize) -> &'static str {
        if self.slots[6] == Some(index) {
            "code"
        } else if self.slots[..6].contains(&Some(index)) {
            "heading"
        } else {
            "body-or-fallback"
        }
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl Owner {
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
