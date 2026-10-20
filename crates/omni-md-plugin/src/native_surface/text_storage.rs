//! Adapts the sole piece-tree document to virtual NSString and bounded NSTextStorage primitives.
//!
//! This module owns derived viewport/font dictionaries and balanced native edit notifications.
//! Every document backlink is weak; native temporary retention cannot prolong canonical text.

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use super::cache_owner::{CacheAdmission, CacheOwner, CachePayload};
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use super::font_run_scratch::{FontRunScratch, copy_query_scalar};
use super::*;
use omni_markdown_core::MarkdownDocumentBufferError;

mod pasteboard_output;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) use super::text_input_policy::copy_native_markdown_input_text;
// 正式 Paged 输入沿共享策略复制本次候选；这三个入口不依赖旧 storage 的构造与缓存。
pub(super) use super::text_input_policy::{
    NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES, copy_native_markdown_input_text_with_limit,
};
pub(super) use pasteboard_output::MarkdownPasteboardOutput;
#[cfg(feature = "native-paged-production")]
pub(super) use pasteboard_output::MarkdownPasteboardWriteError;

#[path = "input_transaction.rs"]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod input_transaction;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) use input_transaction::*;
#[path = "pending_selection.rs"]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod pending_selection;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) use pending_selection::*;
#[path = "source_semantics.rs"]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod source_semantics;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use source_semantics::{NativeMarkdownNoTablesProof, NativeMarkdownSourceOwner};

/// 字符发布同时移交编辑结果、原选择收据和通知尾声；调用方须先声明尾槽，
/// 将 guard 保持到实际 after 安装及全部局部 Native claims 释放结束。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
type NativeMarkdownCanonicalPublication<Edit> = (
    Edit,
    Rc<NativeMarkdownCommittedSelection>,
    Option<NativeMarkdownPendingSelectionGuard>,
);

/// 原生计算的原始正文与模式；相同代次数值不等于同一笔记。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
struct AttributeRequest {
    identity: WeakMarkdownDocument,
    generation: u64,
    mode: MarkdownPresentationMode,
    semantic_epoch: u64,
    // 行高失效也使在途字典请求失效；字典只保存这个标量，不延长投影寿命。
    table_geometry_epoch: u64,
    table_row_height: Option<f64>,
    list_role: MarkdownListGlyphRole,
}

/// A native string adapter borrows document lifetime weakly and retains no text bytes.
pub(super) struct PieceBackedStringIvars {
    // AppKit may temporarily retain this string after surface teardown. A weak document
    // link prevents delayed callbacks from extending the canonical text owner's lifetime.
    pub(super) document: RefCell<WeakMarkdownDocument>,
    // 显式双feature试验才有此槽；默认适配器不创建家族、快照或额外派生值。
    #[cfg(all(
        feature = "native-block-fragment-diagnostic",
        feature = "native-markdown-runtime-probe"
    ))]
    pub(super) frozen_values: Option<super::frozen_live_values::FrozenLiveValues>,
}

define_class!(
    // SAFETY: NSString requires length and characterAtIndex: primitives; getCharacters:range:
    // provides bounded bulk access. The adapter and document remain main-thread-only.
    #[unsafe(super = NSString)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PieceBackedStringIvars]
    pub(super) struct PieceBackedMarkdownString;

    impl PieceBackedMarkdownString {
        #[unsafe(method(length))]
        /// Reports canonical UTF-16 length without allocating a Foundation text mirror.
        fn length(&self) -> NSUInteger {
            // After document teardown a delayed native query observes an empty adapter.
            self.ivars()
                .document
                .borrow()
                .upgrade()
                .map_or(0, |document| document.borrow().utf16_len())
        }

        #[unsafe(method(characterAtIndex:))]
        /// 只读取原canonical中的一个UTF-16单元，保留代理对内部位置的NSString合同。
        fn character_at_index(&self, index: NSUInteger) -> unichar {
            // 缺源或非法请求沿既有live适配器返回替换单元；诊断仍保留原非法范围事实。
            let document = self.ivars().document.borrow().upgrade();
            if let Some(document) = document {
                let document = document.borrow();
                #[cfg(all(feature = "native-block-fragment-diagnostic", feature = "native-markdown-runtime-probe"))]
                super::string_read_diagnostic::observe_primitive(
                    document.utf16_len(), Some(index), 1, index < document.utf16_len(),
                    super::string_read_diagnostic::ReadPrimitive::Character,
                ); // 同一原借用只交标量；单字符访问也不能绕过全进程非法范围诊断。
                document.utf16_code_unit_at(index).unwrap_or(0xfffd)
            } else {
                #[cfg(all(feature = "native-block-fragment-diagnostic", feature = "native-markdown-runtime-probe"))]
                super::string_read_diagnostic::observe_primitive(
                    0, Some(index), 1, false,
                    super::string_read_diagnostic::ReadPrimitive::Character,
                ); // 已失去原源时仍记录真实非法请求，不升级另一文档或把它补成合法空读。
                0xfffd
            }
        }

        #[unsafe(method(getCharacters:range:))]
        /// Copies a caller-provided UTF-16 range without retaining a duplicate source buffer.
        ///
        /// # Safety
        /// Foundation must supply a destination writable for range.length code units.
        unsafe fn get_characters_range(&self, destination: NonNull<unichar>, range: NSRange) {
            // SAFETY: Foundation provides a destination writable for at least range.length
            // UTF-16 code units for the duration of this primitive call.
            let destination =
                unsafe { std::slice::from_raw_parts_mut(destination.as_ptr(), range.length) };
            if let Some(document) = self.ivars().document.borrow().upgrade() {
                // A live canonical owner supplies the requested bounded UTF-16 range.
                let document = document.borrow();
                #[cfg(all(
                    feature = "native-block-fragment-diagnostic",
                    feature = "native-markdown-runtime-probe"
                ))]
                super::string_read_diagnostic::observe(
                    document.utf16_len(),
                    range.location,
                    range.length,
                );
                copy_piece_backed_utf16_range(&document, range.location, destination);
            } else {
                #[cfg(all(
                    feature = "native-block-fragment-diagnostic",
                    feature = "native-markdown-runtime-probe"
                ))]
                // 原弱正文已失效，此次canonical长度就是0；不能为诊断升级或重建另一正文owner。
                super::string_read_diagnostic::observe(0, range.location, range.length);
                // A delayed query after teardown must still initialize every destination code unit.
                destination.fill(0xfffd);
            }
        }

        #[cfg(all(feature = "native-block-fragment-diagnostic", feature = "native-markdown-runtime-probe"))]
        #[unsafe(method_id(copyWithZone:))]
        #[unsafe(method_family = copy)]
        fn frozen_copy(&self, zone: *mut objc2::runtime::NSZone) -> Retained<NSString> {
            if let Some(values) = &self.ivars().frozen_values {
                // 本次copy捕获原当前代次；helper在首次Native构造前已放下Doc强租约与全部借用。
                values.copy_value(&self.ivars().document)
            } else {
                // 未显式启用的既有会话按原公开copy语义转发，不能受运行中环境变量改变影响。
                // SAFETY: 同一NSString superclass接收原zone，copy家族由method_id只移交这一个+1。
                unsafe { msg_send![super(self), copyWithZone: zone] }
            }
        }

        #[cfg(all(feature = "native-block-fragment-diagnostic", feature = "native-markdown-runtime-probe"))]
        #[unsafe(method_id(substringWithRange:))]
        #[unsafe(method_family = none)]
        fn frozen_substring(&self, range: NSRange) -> Retained<NSString> {
            if let Some(values) = &self.ivars().frozen_values {
                // 精确保留UTF-16范围，包含代理对内部位置；不裁短请求或改变原文字。
                values.substring_value(&self.ivars().document, range)
            } else {
                // SAFETY: 原公开noncopy selector单次传给super，默认返回与原路径一致。
                unsafe { msg_send![super(self), substringWithRange: range] }
            }
        }
    }
);

/// Fills a native buffer from scalar-safe canonical ranges, deterministically replacing invalid requests.
pub(super) fn copy_piece_backed_utf16_range(
    document: &MarkdownDocumentBuffer,
    utf16_offset: usize,
    destination: &mut [unichar],
) {
    let valid_range = utf16_offset
        .checked_add(destination.len())
        .is_some_and(|range_end| range_end <= document.utf16_len());
    if !valid_range
        || document
            .copy_utf16_code_units(utf16_offset, destination)
            .is_err()
    {
        // Objective-C primitives cannot return Rust errors. Invalid requests still initialize
        // the entire destination deterministically rather than exposing unwritten memory.
        destination.fill(0xfffd);
    }
}

impl PieceBackedMarkdownString {
    /// Creates a virtual string with a weak link to the session's unique document owner.
    pub(super) fn new(document: &SharedMarkdownDocument, mtm: MainThreadMarker) -> Retained<Self> {
        // The weak link lets delayed Foundation string queries expire without retaining an old mmap.
        let string = Self::alloc(mtm).set_ivars(PieceBackedStringIvars {
            document: RefCell::new(Rc::downgrade(document)),
            #[cfg(all(
                feature = "native-block-fragment-diagnostic",
                feature = "native-markdown-runtime-probe"
            ))]
            frozen_values: super::frozen_live_values::FrozenLiveValues::new_if_requested(),
        });
        // SAFETY: the custom class implements the required NSString primitives and initializes
        // all Rust ivars before invoking the superclass initializer.
        unsafe { msg_send![super(string), init] }
    }
}

#[derive(Clone)]
/// 几何、单格和 delimiter 共享同一不可变目录；原源与选择票不能由后继缓存重新签发。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct NativeMarkdownTablePresentationSnapshot {
    pub(super) document_generation: u64,
    pub(super) utf16_window: std::ops::Range<usize>,
    pub(super) identity: WeakMarkdownDocument,
    pub(super) document_length: usize,
    pub(super) selection: MarkdownTextSelection,
    pub(super) semantic_epoch: u64,
    // 无下方真实无表证明时，None仅表示旧bounded fallback；不能据此授权普通或表格语义。
    pub(super) semantics: Option<MarkdownViewportPresentationSnapshot>,
    // 仅同一Core来源的真实Gap可签无表证明；它不授予普通inline样式或表格完成权限。
    pub(super) verified_no_tables: Option<NativeMarkdownNoTablesProof>,
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl NativeMarkdownTablePresentationSnapshot {
    pub(super) fn tables(&self) -> &[MarkdownTablePrototype] {
        self.semantics
            .as_ref()
            .map_or(&[], |semantics| semantics.tables())
    }

    /// 纯状态检查结束所有借用；正常属性窗口换页不撤销原不可变快照，编辑和选择变化仍撤权。
    pub(super) fn is_current(&self, storage: &PieceBackedMarkdownTextStorage) -> bool {
        if !storage.ivars().accepts_edits.get()
            || storage.ivars().composition_active.get()
            || storage.presentation_mode() != MarkdownPresentationMode::InlinePreview
            || storage.ivars().presentation_semantic_epoch.get() != Some(self.semantic_epoch)
            || !std::rc::Weak::ptr_eq(&self.identity, &storage.ivars().document.borrow())
        {
            // 旧正文、模式、输入或关闭票不能借相同 generation 读取后继会话。
            return false;
        }
        self.identity.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document.generation() == self.document_generation
                && document.utf16_len() == self.document_length
                && document.selection() == self.selection
                && self
                    .verified_no_tables
                    .as_ref()
                    .is_none_or(|proof| proof.is_current(storage, &document))
        })
    }

    /// 单格样式只从原成功解析查询，覆盖缺失绝不能被转换成默认样式。
    pub(super) fn query(
        &self,
        index: usize,
    ) -> Option<omni_markdown_core::MarkdownPresentationQuery> {
        self.semantics.as_ref()?.query(index)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A visible-cell UTF-16 interval carries only the semantic style required for native drawing.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct NativeMarkdownTableCellStyleSpan {
    pub(super) utf16_range: std::ops::Range<usize>,
    pub(super) styles: MarkdownPresentationStyles,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 每个显示run都线性对应原文相同数量的UTF16单元；隐藏语法不占显示坐标。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct NativeMarkdownTableCellSourceSpan {
    pub(super) display_utf16_range: std::ops::Range<usize>,
    pub(super) source_utf16_range: std::ops::Range<usize>,
}

#[derive(Debug, Eq, PartialEq)]
/// 有限单格只生成一次显示文字；样式与源run共享同一次标量追加结果，供实际绘制和命中使用。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct NativeMarkdownTableCellDisplayText {
    pub(super) text: String,
    pub(super) style_spans: Vec<NativeMarkdownTableCellStyleSpan>,
    pub(super) source_spans: Vec<NativeMarkdownTableCellSourceSpan>,
}

/// 隐藏标记和折叠转义时同时记录原绝对坐标，不为指针命中重读单格或重建另一份显示串。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_table_cell_display_text_from_source(
    source: &str,
    absolute_utf16_start: usize,
    mut styles_at: impl FnMut(usize) -> MarkdownPresentationStyles,
) -> NativeMarkdownTableCellDisplayText {
    use omni_markdown_core::{
        MarkdownTableCellDisplayAnswer, MarkdownTableCellDisplayCursor,
        MarkdownTableCellDisplayInput,
    };
    let mut text = String::with_capacity(source.len());
    let mut style_spans: Vec<NativeMarkdownTableCellStyleSpan> = Vec::new();
    let mut source_spans: Vec<NativeMarkdownTableCellSourceSpan> = Vec::new();
    let mut input = source.chars();
    let mut cursor = MarkdownTableCellDisplayCursor::new(absolute_utf16_start);
    loop {
        // 现行 CTFrame 只保留原已准入小格的输出；实际转换规则与新分页显示源共用原 Core 游标。
        let step = cursor
            .step(|canonical| {
                Ok::<_, std::convert::Infallible>(match input.next() {
                    Some(scalar) => MarkdownTableCellDisplayInput::Scalar {
                        scalar,
                        styles: styles_at(canonical),
                    },
                    None => MarkdownTableCellDisplayInput::End,
                })
            })
            .expect("固定字符串读取没有失败分支");
        let scalar = match step.answer {
            MarkdownTableCellDisplayAnswer::Scalar(scalar) => scalar,
            MarkdownTableCellDisplayAnswer::Progress => continue,
            MarkdownTableCellDisplayAnswer::Complete => break,
            MarkdownTableCellDisplayAnswer::NeedInput => unreachable!("完整有限字符串没有暂停来源"),
        };
        text.push(scalar.scalar);
        if let Some(previous) = source_spans.last_mut()
            && previous.display_utf16_range.end == scalar.display_utf16.start
            && previous.source_utf16_range.end == scalar.canonical_utf16.start
        {
            // 只有显示和 canonical 两边都连续才合并；隐藏标记与转义的源 gap 必须保留。
            previous.display_utf16_range.end = scalar.display_utf16.end;
            previous.source_utf16_range.end = scalar.canonical_utf16.end;
        } else {
            source_spans.push(NativeMarkdownTableCellSourceSpan {
                display_utf16_range: scalar.display_utf16.clone(),
                source_utf16_range: scalar.canonical_utf16,
            });
        }
        if let Some(previous) = style_spans.last_mut()
            && previous.utf16_range.end == scalar.display_utf16.start
            && previous.styles == scalar.styles
        {
            // 显示相邻且实际样式一致便复用原属性 run；它不改变上方来源断点。
            previous.utf16_range.end = scalar.display_utf16.end;
        } else {
            style_spans.push(NativeMarkdownTableCellStyleSpan {
                utf16_range: scalar.display_utf16,
                styles: scalar.styles,
            });
        }
    }
    NativeMarkdownTableCellDisplayText {
        text,
        style_spans,
        source_spans,
    }
}

/// 仅存活前台编辑器拥有的有限字体、颜色和基础属性；关闭取走最后一个项目 Rc。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct MarkdownStorageRenderPayload {
    pub(super) base_attributes: Retained<MarkdownTextAttributes>,
    pub(super) palette: MarkdownTextAttributePalette,
}

/// composition root只绑定一次原view；借出时没有RefCell借用跨Weak升级，关闭后不能恢复。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
struct NativeMarkdownStorageViewLink {
    view: Option<Weak<PieceBackedMarkdownTextView>>,
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl CachePayload for NativeMarkdownStorageViewLink {
    fn empty() -> Self {
        Self { view: None }
    }
}

/// Weak document access plus editor-bounded derived attributes and synchronous input state.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct PieceBackedTextStorageIvars {
    view_link: CacheOwner<NativeMarkdownStorageViewLink>,
    view_link_bound: Cell<bool>,
    // AppKit may retain storage beyond the surface callback. The model owns text, and
    // this weak adapter link prevents delayed TextKit work from keeping that text alive.
    pub(super) document: RefCell<WeakMarkdownDocument>,
    // 仅Weak访问view自有的有限fold metadata，不建立Native对象或文档正文强边。
    pub(super) fold_state: RefCell<std::rc::Weak<NativeMarkdownFoldState>>,
    pub(super) string: Retained<PieceBackedMarkdownString>,
    // 字体与属性资源可独立取走；延迟 TextKit 请求只得到空字典。
    pub(super) render_payload: RefCell<Option<Rc<MarkdownStorageRenderPayload>>>,
    pub(super) font_selection_cache: CacheOwner<MarkdownFontSelectionCache>,
    pub(super) attribute_cache: CacheOwner<MarkdownTextAttributeCache>,
    pub(super) font_run_scratch: CacheOwner<FontRunScratch>,
    #[cfg(feature = "native-block-fragment-diagnostic")]
    pub(super) mode_attribute_diagnostic:
        super::preview_blocks::diagnostic::ModeAttributeDiagnostic,
    // A single bounded semantic cache parses source once for both visible source syntax and inline preview.
    pub(super) presentation: RefCell<MarkdownViewportPresentation>,
    // 同一活动storage只保一份Core来源请求/原Global；getter不得按query重新分配结构owner。
    pub(super) source_semantics: RefCell<Option<Rc<NativeMarkdownSourceOwner>>>,
    // 源语义与字体/行高 scratch 分开；单纯几何准备不能撤销原 metadata 发布。
    // None 表示单调身份耗尽，永远不复用旧数值给快照续权。
    pub(super) presentation_semantic_epoch: Cell<Option<u64>>,
    pub(super) presentation_mode: Cell<MarkdownPresentationMode>,
    // 只保存当前有限行高；不持有正文或原生布局对象，宽度变化立即撤销旧投影权限。
    pub(super) table_row_heights: RefCell<Option<Rc<NativeMarkdownTableRowHeightProjection>>>,
    pub(super) table_geometry_epoch: Cell<u64>,
    pub(super) table_projection_width_key: Cell<i64>,
    pub(super) pending_selection: Rc<NativeMarkdownPendingSelectionSlot>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) pending_selection_reads: Rc<Cell<u64>>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) pending_selection_fault:
        RefCell<Option<Rc<super::probe::pending_fault::NativeMarkdownPendingSelectionFault>>>,
    pub(super) composition_active: Cell<bool>,
    // 仅同步输入回调发布一个精确计划；输入结果不进入闲置正文缓存。
    pub(super) input_transaction: RefCell<Option<Rc<NativeMarkdownInputTransaction>>>,
    pub(super) input_transaction_epoch: Cell<u64>,
    // 关闭断开 Weak 前从实际 Core reservation 查询保留排空收据，不能只看 Native 槽已 take。
    pub(super) input_reservation_empty: Cell<bool>,
    // 仅合成virtual-paragraph观察真实substring receiver；固定计数不持正文或Native对象。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) content_probe_substring_calls: Cell<u8>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) character_process_count: Cell<u64>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) input_commit_count: Cell<u64>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) input_callback_success_count: Cell<u64>,
    // 直接记录真正的 autosave 排队入口；同步回调内观察空计时器不能证明没有排队。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) autosave_enqueue_count: Cell<u64>,
    // Close preflight seals canonical mutation before native window callbacks can deliver stale input.
    pub(super) accepts_edits: Cell<bool>,
    // Derived attribute edits must wait until the outer NSTextView input transaction ends.
    pub(super) native_input_depth: Cell<usize>,
    pub(super) payload_use_depth: Cell<usize>,
    pub(super) deferred_presentation_invalidation: RefCell<Option<std::ops::Range<usize>>>,
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
define_class!(
    // SAFETY: this NSTextStorage subclass implements all four required mutable attributed
    // string primitives and reports edits through edited:range:changeInLength:.
    // All state and callbacks are confined to the AppKit main thread.
    #[unsafe(super = NSTextStorage)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PieceBackedTextStorageIvars]
    pub(super) struct PieceBackedMarkdownTextStorage;

    impl PieceBackedMarkdownTextStorage {
        #[unsafe(method(fixesAttributesLazily))]
        /// Declares derived attributes lazy so Foundation consumes bounded queries instead of an eager mirror.
        fn fixes_attributes_lazily(&self) -> bool {
            // TextKit must query the bounded viewport provider on demand. If the
            // superclass eagerly normalizes attributes after every edit, it can
            // re-enter `attributesAtIndex:effectiveRange:` while our piece tree
            // transaction is still unwinding and repeatedly fix the same range.
            // Declaring the storage lazy is the documented NSTextStorage contract
            // for virtual/derived attributes and keeps input and undo finite.
            true
        }

        #[unsafe(method_id(string))]
        /// Returns the same virtual NSString adapter, never a flattened document copy.
        fn string(&self) -> Retained<NSString> {
            // Retaining the adapter does not extend the weak canonical document lifetime.
            self.ivars().string.clone().into_super()
        }

        #[cfg(feature = "native-markdown-runtime-probe")]
        #[unsafe(method_id(attributedSubstringFromRange:))]
        #[unsafe(method_family = none)]
        fn probe_attributed_substring(&self, range: NSRange) -> Retained<NSAttributedString> {
            let observed = if super::content_storage_probe::receiver_observation_requested() {
                let count = self.ivars().content_probe_substring_calls.get();
                if count < 4 {
                    self.ivars().content_probe_substring_calls.set(count + 1);
                    Some(count + 1)
                } else {
                    None // 固定观察预算耗尽后不再递增，避免长枚举把诊断计数变成溢出故障。
                }
            } else {
                None // 默认和其它probe值不输出，不添加文字/长度getter或改变原请求。
            };
            if let Some(call) = observed {
                eprintln!(
                    "content_storage_probe stage=canonical_substring_enter call={} receiver=canonical_storage location={} length={} synthetic_only=true diagnostic_formal=false",
                    call, range.location, range.length,
                );
            }
            // SAFETY: 此公开非copy selector按原NSRange单次调用父类；不截断、不重建或替换返回对象。
            let value: Retained<NSAttributedString> = unsafe {
                msg_send![super(self), attributedSubstringFromRange: range]
            };
            if let Some(call) = observed {
                let receiver: &NSAttributedString = self;
                eprintln!(
                    "content_storage_probe stage=canonical_substring_return call={} result_class={} result_is_self={} result_is_virtual={} synthetic_only=true diagnostic_formal=false",
                    call,
                    value.class().name().to_string_lossy(),
                    std::ptr::eq::<NSAttributedString>(&*value, receiver),
                    super::content_storage_probe::virtual_attributed_receiver_is_ours(&value),
                ); // 只观察公开runtime身份，不调用返回值string/length或读取正文。
            }
            value // 唯一尾表达式由method_id按noncopy约定移交，未额外retain或转换所有权。
        }

        #[unsafe(method_id(attributesAtIndex:effectiveRange:))]
        /// Supplies one bounded semantic run to TextKit without persisting mutable attributes.
        ///
        /// # Safety
        /// A non-null effective_range must be writable for one NSRange during this call.
        unsafe fn attributes_at_index_effective_range(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
        ) -> Retained<MarkdownTextAttributes> {
            let (attributes, range) = if self.ivars().composition_active.get() {
                // Marked text uses stable base attributes until its synchronous IME transaction commits.
                let range = self.composition_base_attribute_range(index);
                (self.base_attributes(), range)
            } else {
                // Outside composition, derive attributes from the bounded current-generation viewport.
                self.attributes_at_index(index)
            };
            if !effective_range.is_null() {
                // Foundation requested the effective range; a null optional output requires no write.
                // SAFETY: Foundation supplies either null or a writable NSRange pointer for this call.
                // The computed effective range is bounded by the current canonical document.
                unsafe { effective_range.write(range) };
            }
            attributes
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        /// Mutates the piece tree before publishing one synchronous TextKit character transaction.
        fn replace_characters_in_range(&self, range: NSRange, replacement: &NSString) {
        // 原生通知/测量可能重入关闭；栈内载荷租约结束前预检必须拒绝。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

            // 原输入计划只借出局部 Rc；额外或已撤权的 primitive 不重新领取当前选择。
            let input = self.ivars().input_transaction.borrow().as_ref().map(Rc::clone);
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_input_probe_stage("primitive_enter", self);
            if let Some(input) = input.as_ref()
                && !self.input_transaction_is_current(input)
            {
                input.reject("原输入事务已失效");
                return;
            }
            if let Some(input) = input.as_ref()
                && self.consume_unmark_bookkeeping(input, range, replacement)
            {
                return; // 已知等值子调用已经消费，未知子调用已拒绝；两者都不能回落为正文编辑。
            }

            if !self.ivars().accepts_edits.get() {
                // After successful save/close preflight, delayed native input may not create an unsaved revision.
                return;
            }
            // Reject arithmetic overflow before changing the canonical text or semantic selection.
            let Some(range_end) = range.location.checked_add(range.length) else {
                // Foundation cannot receive a Rust Result here; invalid ranges preserve the current document.
                return;
            };
            let Some(document) = self.document() else {
                // Delayed native edits after teardown cannot recreate a canonical document.
                return;
            };
            // 本次计划的 observer 已在 super 前完成，primitive 不能再准备另一份候选。
            let observer = if input.is_none() {
                match self.prepare_fold_observer(&document) {
                    Ok(observer) => Some(observer),
                    Err(_) => return, // 非输入协议的既有 primitive 路径保持原事务边界。
                }
            } else { None };
            // Native selection coordinates count UTF-16 units, independently of the incoming UTF-8 byte length.
            let replacement_utf16_length = replacement.len_utf16();
            if let Some(input) = input.as_ref()
                && !self.input_transaction_is_current(input)
            {
                input.reject("读取替换长度时原输入已失效");
                return;
            }
            let Some(selection_offset) = range.location.checked_add(replacement_utf16_length) else {
                // An unrepresentable caret would violate canonical selection bounds; reject the whole edit.
                return;
            };
            let selection_after = input.as_ref().map_or(
                MarkdownTextSelection::caret(selection_offset),
                |input| input.after,
            );
            let pending_sources = NativeMarkdownPendingSelectionSources {
                source: "primitive", replacement: Some(range), caret: Some(NSRange::new(selection_offset, 0)),
                marked_before: input.as_ref().and_then(|input| input.marked_cell.get()),
                marked_after: input.as_ref().and_then(|input| input.marked_after),
                is_input_replacement: input.is_some(),
            };
            // TextKit reports signed UTF-16 deltas, independently of the native unsigned range width.
            let Ok(delta) = isize::try_from(replacement_utf16_length)
                .and_then(|inserted| isize::try_from(range.length).map(|removed| inserted - removed))
            else {
                // A delta outside isize cannot be reported faithfully; leave text and history unchanged.
                return;
            };

            // Native分组可重入，进入Core前仍须检查原fold与正文代次。
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_input_probe_stage("primitive_before_begin_editing", self);
            self.beginEditing();
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_input_probe_stage("primitive_after_begin_editing", self);
            let edit_result = autoreleasepool(|pool| {
                if let Some(input) = input.as_ref()
                    && !self.input_transaction_is_current(input)
                {
                    return Err("原输入在编辑组开始后已失效".to_owned());
                }
                // The temporary replacement view is borrowed only for this complete canonical transaction.
                // SAFETY: the borrowed UTF-8 view is fully consumed before this inner autorelease pool drains.
                let replacement = unsafe { replacement.to_str(pool) };
                if let Some(input) = input.as_ref()
                    && (!self.input_transaction_is_current(input) || !input.claim(range, replacement))
                {
                    return Err("基础文本替换与原输入声明不一致".to_owned());
                }
                let result = if let Some(input) = input.as_ref() {
                    let prepared = input.prepared.borrow_mut().take().ok_or("原输入没有可发布的 Core 准备票")?;
                    let NativeMarkdownPreparedInput { token, folds } = prepared;
                    let published = {
                        let mut current = document.borrow_mut();
                        if !self.ivars().accepts_edits.get() || !folds.is_current(&current)
                            || !input.captured.current_in(&current)
                        {
                            Err("原输入在 Core 发布前已撤权".to_owned())
                        } else {
                            #[cfg(feature = "native-markdown-runtime-probe")]
                            native_markdown_input_probe_stage("primitive_before_core_publish", self);
                            let result = current.publish_text_input(&token).map_err(|error| error.to_string());
                            self.ivars().input_reservation_empty.set(!current.has_prepared_text_input());
                            result
                        }
                    };
                    match published {
                        Ok(_) => {
                            drop(token); // 已消费同 nonce；不再能取消任何后继事务。
                            Ok(folds)
                        }
                        Err(error) => {
                            // 仍保存原 token 供正常出口 exact cancel，不靠 token Drop 后的无票 drain 清理。
                            input.prepared.replace(Some(NativeMarkdownPreparedInput { token, folds }));
                            Err(error)
                        }
                    }
                } else {
                    let Some(observer) = observer else { return Err("普通字符修改缺少原观察票".into()); };
                    let mut current = document.borrow_mut();
                    if !self.ivars().accepts_edits.get() || !observer.is_current(&current) {
                        return Err("折叠对应的笔记状态已变更".to_owned());
                    }
                    current.replace_utf16_range_with_selection_observed(
                        range.location..range_end, replacement,
                        MarkdownTextSelection { anchor_utf16: range.location, head_utf16: range_end },
                        selection_after, observer,
                    ).map_err(|error| error.to_string())
                };
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_input_probe_stage(if result.is_ok() { "primitive_core_ok" } else { "primitive_core_err" }, self);
                // 先结束Doc借用并发布同代fold，再允许本地池drain产生Native析构重入。
                // 不能把PreparedFolds带出池后才安装，否则旧权限可在root提交后被撤销。
                result.map(|prepared| {
                    let (committed, guard) = self.publish_prepared_folds(&document, prepared);
                    let pending_owner = self.begin_pending_selection(&committed, pending_sources);
                    if let Some(input) = input.as_ref() {
                        // 完整选择已由同一 Core 事务发布；marked 与组合标量也先于任何 Native 通知。
                        self.set_input_composition_state(input.marked_after.is_some());
                        input.publish(Rc::clone(&committed), pending_owner);
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        self.ivars().input_commit_count.set(self.ivars().input_commit_count.get() + 1);
                    }
                    (committed, guard)
                })
            });
            if let (Some(input), Err(error)) = (input.as_ref(), edit_result.as_ref()) {
                input.reject(error); // 无成功票就不改 marked；原候选和原 Core 状态保持。
            }
            let pending_trace = edit_result.as_ref().ok().and_then(|(committed, _)| self.pending_selection_for(committed));
            if edit_result.is_ok() {
                // 原 owner 在首池 drain 前已经发布；字符通知不再依赖另一份无身份标量。
                if let Some(owner) = &pending_trace {
                    self.pending_selection_phase(owner, NativeMarkdownPendingSelectionPhase::EditedNotification);
                }
                self.edited_range_changeInLength(
                    NSTextStorageEditActions::EditedCharacters,
                    range,
                    delta,
                );
                if let Some(owner) = &pending_trace {
                    self.pending_selection_phase(owner, NativeMarkdownPendingSelectionPhase::Attributes);
                }
                self.invalidate_presentation_attributes();
            }
            // Failed edits publish no change; both success and rejection close the native editing group.
            if let Some(owner) = &pending_trace {
                self.pending_selection_phase(owner, NativeMarkdownPendingSelectionPhase::EndEditing);
            }
            self.endEditing();
            if let Some(owner) = &pending_trace {
                self.pending_selection_phase(owner, NativeMarkdownPendingSelectionPhase::SuperclassTail);
                // 删空后可能没有glyph surface会要求重绘；同一次原提交仍须标脏自有视口图像。
                self.invalidate_committed_view(&owner.committed);
                if input.is_none() { self.finish_pending_selection(owner); }
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_input_probe_stage("primitive_after_end_editing", self);
            if edit_result.is_ok() {
                // Observe dirtiness only after native observers finish this primitive's edit group.
                #[cfg(feature = "native-markdown-runtime-probe")]
                queue_native_markdown_autosave_for_storage(self);
            }
        }

        #[cfg(feature = "native-markdown-runtime-probe")]
        #[unsafe(method(processEditing))]
        /// 仅回归统计实际 processEditing 的字符标志，不改变默认产品的通知行为。
        fn probe_process_editing(&self) {
            if self.editedMask().contains(NSTextStorageEditActions::EditedCharacters) {
                self.ivars().character_process_count.set(self.ivars().character_process_count.get() + 1);
            }
            // SAFETY: 同一主线程 NSTextStorage 的公开方法仍只向 superclass 转发一次。
            unsafe { let _: () = msg_send![super(self), processEditing]; }
        }

        #[unsafe(method(setAttributes:range:))]
        /// Ignores mutable native attribute writes because viewport semantics own presentation.
        ///
        /// # Safety
        /// The optional dictionary must contain native attribute keys/values valid for this call.
        unsafe fn set_attributes_range(
            &self,
            _attributes: Option<&MarkdownTextAttributes>,
            _range: NSRange,
        ) {
            // Presentation attributes come from bounded viewport semantics. Typing attributes
            // must neither become a second mutable model nor change canonical Markdown bytes.
        }

        #[unsafe(method(fixAttributesInRange:))]
        /// Completes lazy Foundation bookkeeping without scanning/parsing or storing mutable attributes.
        fn fix_attributes_in_range(&self, _range: NSRange) {
            // The presentation layer never stores invalid attributes in the
            // text storage. Attribute runs are computed from the current
            // piece-tree generation when queried, so Foundation's generic
            // paragraph/attachment fixer has no mutable backing attributes to
            // repair. Keeping this override finite is what makes the documented
            // lazy NSTextStorage contract suitable for a virtual string.
        }
    }
);

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl PieceBackedMarkdownTextStorage {
    #[cfg(any(
        test,
        all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        )
    ))]
    pub(super) fn frozen_value_family(
        &self,
    ) -> Option<std::sync::Arc<super::frozen_value_family::FrozenValueFamily>> {
        // 关闭cohort观察同一个原家族，不取源快照或Native派生值，也不新建关闭权限。
        #[cfg(all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        ))]
        {
            self.ivars()
                .string
                .ivars()
                .frozen_values
                .as_ref()
                .map(super::frozen_live_values::FrozenLiveValues::family)
        }
        #[cfg(not(all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        )))]
        {
            // 未构造该诊断家族的普通构建返回真实缺席；生命周期仍保留其余逐对象释放门。
            None
        }
    }

    /// Creates session-local derived caches and virtual native primitives over one weak canonical owner.
    pub(super) fn new(document: SharedMarkdownDocument, mtm: MainThreadMarker) -> Retained<Self> {
        // Fixed palette/base attributes are native resources acquired only for an explicitly active editor.
        let string = PieceBackedMarkdownString::new(&document, mtm);
        let font = NSFont::systemFontOfSize(15.0);
        // TextKit 在原装配边界取得完整列表字形；分页只构造共同字体，不承担这组额外强边。
        // 此时尚未建立 render_payload 的 RefCell，也未借用正文，Native 初始化不会跨业务借用。
        let palette = MarkdownTextAttributePalette::new_for_textkit(font);
        let font_object: &AnyObject = &palette.normal_font;
        let foreground_color_object: &AnyObject = &palette.foreground;
        // SAFETY: AppKit exports these immutable keys for the process lifetime. The dictionary
        // retains its font and color values beyond the temporary stack references.
        let base_attributes = unsafe {
            MarkdownTextAttributes::from_slices(
                &[NSFontAttributeName, NSForegroundColorAttributeName],
                &[font_object, foreground_color_object],
            )
        };
        // Install all Rust ivars before superclass initialization can invoke virtual primitives.
        let storage = Self::alloc(mtm).set_ivars(PieceBackedTextStorageIvars {
            view_link: CacheOwner::new(NativeMarkdownStorageViewLink::empty()),
            view_link_bound: Cell::new(false),
            document: RefCell::new(Rc::downgrade(&document)),
            fold_state: RefCell::new(std::rc::Weak::new()),
            string,
            render_payload: RefCell::new(Some(Rc::new(MarkdownStorageRenderPayload {
                base_attributes,
                palette,
            }))),
            font_selection_cache: CacheOwner::new(MarkdownFontSelectionCache::default()),
            attribute_cache: CacheOwner::new(MarkdownTextAttributeCache::default()),
            font_run_scratch: CacheOwner::new(FontRunScratch::new()),
            #[cfg(feature = "native-block-fragment-diagnostic")]
            mode_attribute_diagnostic:
                super::preview_blocks::diagnostic::ModeAttributeDiagnostic::default(),
            presentation: RefCell::new(MarkdownViewportPresentation::default()),
            source_semantics: RefCell::new(None),
            presentation_semantic_epoch: Cell::new(Some(0)),
            presentation_mode: Cell::new(MarkdownPresentationMode::InlinePreview),
            table_row_heights: RefCell::new(None),
            table_geometry_epoch: Cell::new(0),
            table_projection_width_key: Cell::new(i64::MIN),
            pending_selection: Rc::new(RefCell::new(None)),
            #[cfg(feature = "native-markdown-runtime-probe")]
            pending_selection_reads: Rc::new(Cell::new(0)),
            #[cfg(feature = "native-markdown-runtime-probe")]
            pending_selection_fault: RefCell::new(None),
            composition_active: Cell::new(false),
            input_transaction: RefCell::new(None),
            input_transaction_epoch: Cell::new(0),
            input_reservation_empty: Cell::new(true),
            #[cfg(feature = "native-markdown-runtime-probe")]
            content_probe_substring_calls: Cell::new(0),
            #[cfg(feature = "native-markdown-runtime-probe")]
            character_process_count: Cell::new(0),
            #[cfg(feature = "native-markdown-runtime-probe")]
            input_commit_count: Cell::new(0),
            #[cfg(feature = "native-markdown-runtime-probe")]
            input_callback_success_count: Cell::new(0),
            #[cfg(feature = "native-markdown-runtime-probe")]
            autosave_enqueue_count: Cell::new(0),
            accepts_edits: Cell::new(true),
            native_input_depth: Cell::new(0),
            payload_use_depth: Cell::new(0),
            deferred_presentation_invalidation: RefCell::new(None),
        });
        // SAFETY: all required NSTextStorage primitives and Rust ivars are initialized before
        // the superclass initializer can dispatch to the subclass.
        unsafe { msg_send![super(storage), init] }
    }

    /// Temporarily acquires the canonical document only while its foreground model still owns it.
    pub(super) fn document(&self) -> Option<SharedMarkdownDocument> {
        // A weak upgrade cannot recreate a document after the model's teardown boundary.
        self.ivars().document.borrow().upgrade()
    }

    /// 仅composition root在真实view构造后绑定；Native只有Weak，不创建storage↔view强环。
    pub(super) fn bind_text_view_once(
        &self,
        view: &PieceBackedMarkdownTextView,
    ) -> Result<(), String> {
        if !self.ivars().accepts_edits.get() || self.ivars().view_link_bound.get() {
            return Err("正文绘制弱链接不能重复绑定或在关闭后重建".into());
        }
        let CacheAdmission::Ready(mut lease) = self.ivars().view_link.begin() else {
            return Err("正文绘制弱链接正在构造或已关闭".into());
        };
        let original = view
            .ivars()
            .text_storage
            .load()
            .ok_or("正文视图没有原存储".to_owned())?;
        if !std::ptr::eq::<Self>(&*original, self)
            || !self.ivars().accepts_edits.get()
            || view.ivars().textkit_graph_teardown.get()
            || lease.payload_mut().view.is_some()
        {
            return Err("正文绘制弱链接不属于原存储".into());
        }
        let weak = Weak::from(view); // 唯一payload已移到局部lease；weak初始化不跨RefCell借用。
        if !self.ivars().accepts_edits.get() || view.ivars().textkit_graph_teardown.get() {
            return Err("正文绘制弱链接在构造中已撤销".into());
        }
        lease.payload_mut().view = Some(weak);
        self.ivars().view_link_bound.set(true);
        Ok(()) // 归还原weak；teardown封闭的loan永远不能恢复。
    }

    /// 字符提交的原收据只标脏同一真实view的有限视口；不取fresh选择、不排版或另排事件。
    fn invalidate_committed_view(&self, committed: &NativeMarkdownCommittedSelection) {
        if !self.ivars().accepts_edits.get() || !committed.current() {
            return; // 后继编辑/关闭撤旧权限，只停止本次绘图，不回滚已提交正文。
        }
        let CacheAdmission::Ready(mut lease) = self.ivars().view_link.begin() else {
            return; // 同步重入或关闭不借第二份weak，原实际提交不受显示借用影响。
        };
        let Some(weak) = lease.payload_mut().view.as_ref() else {
            return; // 独立storage夹具没有view；只跳过显示，不把提交改成失败。
        };
        let Ok(Some(view)) = committed.native_step(|| weak.load()) else {
            return; // 原Weak已nil或原提交撤权，不搜索后继surface或Native图。
        };
        let _payload_use = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        let current = || {
            self.ivars().accepts_edits.get()
                && !view.ivars().textkit_graph_teardown.get()
                && committed.current()
                && self.document().is_some_and(|document| {
                    view.ivars().folds.matches_document(&document)
                        && committed.current_utf16_len_for(&document).is_some()
                })
        }; // 只升级原Rust Weak并核同g/完整选择，所有Doc借用在Native前结束。
        let Ok(Ok(Some(original_storage))) = native_markdown_table_intent_step(&current, || {
            committed.native_step(|| view.ivars().text_storage.load())
        }) else {
            return;
        };
        if !std::ptr::eq::<Self>(&*original_storage, self) {
            return; // 同一view地址也不能授权另一storage或笔记。
        }
        let Ok(Ok(visible)) = native_markdown_table_intent_step(&current, || {
            committed.native_step(|| view.visibleRect())
        }) else {
            return;
        };
        let right = visible.origin.x + visible.size.width;
        let bottom = visible.origin.y + visible.size.height;
        if ![
            visible.origin.x,
            visible.origin.y,
            visible.size.width,
            visible.size.height,
            right,
            bottom,
        ]
        .into_iter()
        .all(f64::is_finite)
            || visible.size.width <= 0.0
            || visible.size.height <= 0.0
            || right <= visible.origin.x
            || bottom <= visible.origin.y
        {
            return; // 空/异常几何不扩大成全文bounds，也不能靠清旧contents冒充新图。
        }
        let _ = native_markdown_table_intent_step(&current, || {
            committed.native_step(|| view.setNeedsDisplayInRect(visible))
        }); // getter与setter分别前后核同原收据；这里只标脏，实际绘制由原AppKit回调消费。
    }

    /// 原回调在首个字符串 getter 前固定输入 epoch；占用中的槽不允许嵌套覆盖。
    pub(super) fn reserve_input_transaction_epoch(&self) -> Result<u64, String> {
        if !self.ivars().accepts_edits.get() || self.ivars().input_transaction.borrow().is_some() {
            return Err("另一文本输入事务仍在完成中".into());
        }
        let epoch = self
            .ivars()
            .input_transaction_epoch
            .get()
            .checked_add(1)
            .ok_or("文本输入事务代次已耗尽")?;
        self.ivars().input_transaction_epoch.set(epoch);
        self.cancel_source_semantics(); // 原输入epoch实际推进才撤旧来源；耗尽拒绝不能重发相同上下文预算。
        Ok(epoch)
    }

    /// 只发布已校验的原计划；guard 不保留或恢复任何被替换的旧槽。
    pub(super) fn install_input_transaction(
        &self,
        transaction: Rc<NativeMarkdownInputTransaction>,
    ) -> Result<NativeMarkdownInputTransactionGuard<'_>, String> {
        if !self.ivars().accepts_edits.get()
            || self.ivars().input_transaction_epoch.get() != transaction.epoch
            || !transaction.captured.current()
            || self.ivars().input_transaction.borrow().is_some()
        {
            return Err("文本输入计划在安装前已失效".into());
        }
        self.ivars()
            .input_transaction
            .replace(Some(Rc::clone(&transaction)));
        Ok(NativeMarkdownInputTransactionGuard {
            storage: self,
            transaction,
        })
    }

    pub(super) fn input_transaction_is_current(
        &self,
        transaction: &Rc<NativeMarkdownInputTransaction>,
    ) -> bool {
        self.ivars().accepts_edits.get()
            && self.ivars().input_transaction_epoch.get() == transaction.epoch
            && self
                .ivars()
                .input_transaction
                .borrow()
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, transaction))
            && transaction.committed().map_or_else(
                || transaction.captured.current(),
                |committed| committed.current(),
            )
    }

    /// 所有可失败准备在 superclass 之前完成；正文、选择与历史仍保持原版本。
    pub(super) fn prepare_input_transaction(
        &self,
        input: &Rc<NativeMarkdownInputTransaction>,
    ) -> Result<(), String> {
        if !self.input_transaction_is_current(input) || input.unmark_range().is_some() {
            return Err("字符输入没有原准备权限".into());
        }
        let document = self.document().ok_or("笔记输入文档已关闭")?;
        let observer = self
            .prepare_fold_observer(&document)?
            .with_native_before(&input.captured)
            .map_err(|error| error.to_string())?;
        let end = input
            .range
            .location
            .checked_add(input.range.length)
            .ok_or("输入准备范围溢出")?;
        let prepared = {
            let mut document = document.borrow_mut();
            if !observer.is_current(&document) || !input.captured.current_in(&document) {
                return Err("输入在 Core 准备前已撤权".into());
            }
            document
                .prepare_text_input_observed(
                    input.range.location..end,
                    &input.text,
                    input.captured.selection,
                    input.after,
                    observer,
                )
                .map_err(|error| error.to_string())?
        };
        input.prepared.replace(Some(NativeMarkdownPreparedInput {
            token: prepared.0,
            folds: prepared.1,
        }));
        self.ivars().input_reservation_empty.set(false);
        Ok(())
    }

    /// normal return、撤权与 accepted close 共用 nonce 取消；旧 guard 不能取消后继候选。
    pub(super) fn cancel_input_preparation(&self, input: &NativeMarkdownInputTransaction) {
        let sealed = !self.ivars().accepts_edits.get();
        let owns_slot = self
            .ivars()
            .input_transaction
            .borrow()
            .as_ref()
            .is_some_and(|current| std::ptr::eq(&**current, input));
        if !sealed && !owns_slot {
            return;
        }
        let prepared = input.prepared.borrow_mut().take();
        if let Some(document) = self.document() {
            let mut document = document.borrow_mut();
            if let Some(prepared) = prepared {
                let _ = document.cancel_prepared_text_input(&prepared.token);
                // cancel 只消费同 nonce；失败时也不使用无 token drain 撤销另一个候选。
                drop(prepared);
            }
            if sealed {
                document.drain_cancelled_text_input(); // accepted close 已封口，才允许排空异常 Drop 留下的取消候选。
            }
            self.ivars()
                .input_reservation_empty
                .set(!document.has_prepared_text_input());
        } else {
            drop(prepared); // 文档 owner 已消失时 Core 候选随文档释放，纯 Weak token 不能保活它。
            self.ivars().input_reservation_empty.set(true);
        }
    }

    /// unmark 子 primitive 只消费已证明等值的一次参数；没有权限时也不回落为普通编辑。
    fn consume_unmark_bookkeeping(
        &self,
        input: &Rc<NativeMarkdownInputTransaction>,
        range: NSRange,
        replacement: &NSString,
    ) -> bool {
        if input.unmark_range().is_none() {
            return false;
        }
        let child = input.bookkeeping.borrow().as_ref().map(Rc::clone);
        let Some(child) = child else {
            input.reject("解除标记产生了未声明的字符 primitive");
            return true;
        };
        let current = || {
            self.input_transaction_is_current(input)
                && input
                    .bookkeeping
                    .borrow()
                    .as_ref()
                    .is_some_and(|active| Rc::ptr_eq(active, &child))
        };
        let matched = native_markdown_table_intent_step(&current, || {
            autoreleasepool(|pool| {
                // SAFETY: 只借用本次 primitive 字符串，池结束前完成 exact 比较，不保留任何原文。
                let text = unsafe { replacement.to_str(pool) };
                current() && range == child.range && text == child.text && !child.consumed.get()
            })
        })
        .unwrap_or(false);
        if matched {
            child.consumed.set(true); // 无字符变化，故不进入 Core、history、editing group 或 autosave。
        } else {
            input.reject("解除标记的 primitive 不符合原等值子调用");
        }
        true
    }

    /// 只有纯标量变更；flush 留到原 callback 的 Native 输入栈结束之后。
    pub(super) fn set_input_composition_state(&self, composing: bool) {
        if let Some(folds) = self.ivars().fold_state.borrow().upgrade() {
            folds.set_composition(composing);
        }
        self.ivars().composition_active.set(composing);
    }
    /// 在首个Native调用前取得固定fold候选；此后beginEditing重入也不能冒领较新代次。
    fn prepare_fold_observer(
        &self,
        document: &SharedMarkdownDocument,
    ) -> Result<NativeMarkdownFoldObserver, String> {
        let state = self
            .ivars()
            .fold_state
            .borrow()
            .upgrade()
            .ok_or("折叠状态已关闭")?;
        if !state.matches_document(document) {
            return Err("折叠所属文档身份已变更".into());
        }
        state
            .prepare(&document.borrow(), true)
            .map_err(|error| error.to_string())
    }

    /// 成功返回Core后到原生通知前只有纯Rust；一次发布对应同一新generation的fold集合。
    fn publish_prepared_folds(
        &self,
        document: &SharedMarkdownDocument,
        prepared: NativeMarkdownPreparedFolds,
    ) -> (
        Rc<NativeMarkdownCommittedSelection>,
        Option<NativeMarkdownSelectionInstallGuard>,
    ) {
        let committed = Rc::new(prepared.publish(document.borrow().generation()));
        // 紧邻纯发布打开通知lease，连局部pool drain也覆盖；不把active声明留在无guard的slot。
        // 已提交正文不能因安装失败伪装成回滚；None不取得任何Native通知抑制权限。
        let guard = committed.begin_pre_notify_install().ok();
        (committed, guard)
    }

    /// 拷贝有限渲染所有权后立即结束 ivar 借用，避免借用跨越原生回调。
    pub(super) fn render_payload(&self) -> Option<Rc<MarkdownStorageRenderPayload>> {
        self.ivars().render_payload.borrow().clone()
    }

    /// 已释放适配器返回无业务值的不可变字典，不恢复前台缓存。
    fn base_attributes(&self) -> Retained<MarkdownTextAttributes> {
        self.render_payload()
            .map_or_else(MarkdownTextAttributes::new, |render| {
                // 仅复制已有字典；原生使用不持 RefCell 借用。
                render.base_attributes.clone()
            })
    }

    /// accepted release 的不可逆资源边界；原生空壳不再能升级旧文档或重建缓存。
    pub(super) fn release_payload(&self) {
        self.ivars().accepts_edits.set(false);
        self.ivars().view_link.close(); // 仅Weak边也真实清除；在途loan只能析构，不能恢复到关闭槽。
        self.ivars().presentation_semantic_epoch.set(None); // 关闭后的空壳永久没有快照发布权限。
        self.cancel_source_semantics(); // 排队续体只有Weak；同步撤掉Core请求和publication全部项目强槽。
        self.ivars().composition_active.set(false);
        let input_transaction = self.ivars().input_transaction.borrow_mut().take();
        let pending_selection = self.ivars().pending_selection.borrow_mut().take();
        #[cfg(feature = "native-markdown-runtime-probe")]
        let pending_selection_fault = self.ivars().pending_selection_fault.borrow_mut().take();
        if let Some(input) = input_transaction.as_ref() {
            self.cancel_input_preparation(input); // Core 候选在断开 Weak 前取消，不能只释放 Native 强槽。
        } else if let Some(document) = self.document() {
            let mut document = document.borrow_mut();
            document.drain_cancelled_text_input();
            self.ivars()
                .input_reservation_empty
                .set(!document.has_prepared_text_input());
        }
        self.ivars().native_input_depth.set(0);
        let folds = self.ivars().fold_state.replace(std::rc::Weak::new());
        drop(folds);
        let document = self.ivars().document.replace(std::rc::Weak::new());
        let string_document = self
            .ivars()
            .string
            .ivars()
            .document
            .replace(std::rc::Weak::new());
        let render = self.ivars().render_payload.borrow_mut().take();
        let table_row_heights = self.ivars().table_row_heights.borrow_mut().take();
        // 先封闭全部 owner；在 Loaned 时只推进 epoch，归还栈负责实际销毁且不能复活。
        self.ivars().font_selection_cache.close();
        self.ivars().attribute_cache.close();
        self.ivars().font_run_scratch.close();
        let presentation = self
            .ivars()
            .presentation
            .replace(MarkdownViewportPresentation::default());
        let invalidation = self
            .ivars()
            .deferred_presentation_invalidation
            .borrow_mut()
            .take();
        // 原生对象的析构可能回调；全部状态借用已结束后才释放资源。
        drop((
            document,
            string_document,
            render,
            table_row_heights,
            presentation,
            invalidation,
            input_transaction,
        ));
        drop(pending_selection);
        #[cfg(feature = "native-markdown-runtime-probe")]
        drop(pending_selection_fault);
        #[cfg(all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        ))]
        if let Some(values) = &self.ivars().string.ivars().frozen_values {
            // 原Native图已经断开，live DocWeak及全部payload槽也已清空；此时才封源并清Weak缓存。
            // 系统仍持有的不可变副本保留原快照，最后真实归还时由同一家族通知关闭cohort。
            values.seal();
        }
    }

    /// 逐字段证明项目存储 payload 已排空；不把文档弱引用失效当作缓存释放。
    pub(super) fn payload_is_empty(&self) -> bool {
        let pending_empty = self.ivars().pending_selection.borrow().is_none();
        #[cfg(feature = "native-markdown-runtime-probe")]
        let pending_empty =
            pending_empty && self.ivars().pending_selection_fault.borrow().is_none();
        #[cfg(all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        ))]
        let pending_empty = pending_empty
            && self
                .ivars()
                .string
                .ivars()
                .frozen_values
                .as_ref()
                .is_none_or(super::frozen_live_values::FrozenLiveValues::live_cache_is_released);
        pending_empty
            && !self.ivars().accepts_edits.get()
            && self.ivars().view_link.is_released()
            && self.ivars().document.borrow().strong_count() == 0
            && self.ivars().string.ivars().document.borrow().strong_count() == 0
            && self.ivars().render_payload.borrow().is_none()
            && self.ivars().table_row_heights.borrow().is_none()
            && self.ivars().font_selection_cache.is_released()
            && self.ivars().attribute_cache.is_released()
            && self.ivars().font_run_scratch.is_released()
            && self.ivars().presentation.borrow().is_unallocated()
            && self.ivars().source_semantics.borrow().is_none()
            && self
                .ivars()
                .deferred_presentation_invalidation
                .borrow()
                .is_none()
            && !self.ivars().composition_active.get()
            && self.ivars().input_transaction.borrow().is_none()
            && self.ivars().input_reservation_empty.get()
            && self.ivars().native_input_depth.get() == 0
            && self.ivars().payload_use_depth.get() == 0
    }

    /// Detaches any compatibility TextKit 1 graph so it cannot keep this storage alive.
    pub(super) fn detach_legacy_layout_managers(&self) -> usize {
        let managers = self.as_super().layoutManagers();
        let mut detached = 0usize;
        // Reverse removal keeps remaining native indices valid while the array shrinks.
        for index in (0..managers.count()).rev() {
            let manager = managers.objectAtIndex(index);
            manager.setDelegate(None);
            let containers = manager.textContainers();
            // A compatibility manager must stop owning containers before its storage link is detached.
            for container_index in (0..containers.count()).rev() {
                manager.removeTextContainerAtIndex(container_index);
            }
            // The owning storage operation clears the manager's reverse link itself. Directly
            // assigning NSLayoutManager.textStorage would bypass the documented owner-side contract.
            self.as_super().removeLayoutManager(&manager);
            detached = detached.saturating_add(1);
        }
        detached
    }

    /// 已验证的表尾计划在 beginEditing 重入后重新核权；草稿与新行只发布一个历史编辑。
    pub(super) fn apply_planned_table_replacement<'plan>(
        &self,
        captured: &NativeMarkdownCapturedSelection,
        plan: impl FnOnce(
            &MarkdownDocumentBuffer,
        ) -> Result<
            (
                &'plan std::ops::Range<usize>,
                &'plan str,
                Option<MarkdownTextSelection>,
            ),
            String,
        >,
        is_current: &impl Fn() -> bool,
        on_committed: impl FnOnce(&MarkdownHistoryTextEdit, &Rc<NativeMarkdownCommittedSelection>),
    ) -> Result<
        (
            MarkdownHistoryTextEdit,
            Rc<NativeMarkdownCommittedSelection>,
            Option<NativeMarkdownPendingSelectionGuard>,
        ),
        String,
    > {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if !self.ivars().accepts_edits.get() || !is_current() {
            // 首次 Native 调用前已失效的动作不建立编辑组，也不触碰可见字段草稿。
            return Err("表格编辑意图已经改变，草稿将保持打开".into());
        }
        let document = self.document().ok_or("表格所属文档已关闭")?;
        let identity = Rc::downgrade(&document);
        let observer = self
            .prepare_fold_observer(&document)?
            .with_native_before(captured)
            .map_err(|error| error.to_string())?;
        self.beginEditing();
        let result = (|| {
            // 先在可变 Doc 借用外核视图原票；该门禁本身会短读同一 Doc。
            if !self.ivars().accepts_edits.get()
                || !is_current()
                || !std::rc::Weak::ptr_eq(&identity, &self.ivars().document.borrow())
            {
                return Err("表格编辑意图已经改变，草稿将保持打开".into());
            }
            let mut document = document.borrow_mut();
            if !observer.is_current(&document) {
                // 原 fold 候选与源计划必须同时有效，不能在 Native 返回后重领新权限。
                return Err("表格对应的折叠或文档状态已经改变".into());
            }
            let (range, replacement, selection_after) = plan(&document)?;
            let inserted = replacement.encode_utf16().count();
            let caret = range
                .start
                .checked_add(inserted)
                .ok_or("表格插入位置超过 UTF-16 坐标上限")?;
            let inserted = isize::try_from(inserted).map_err(|_| "表格输入超过原生通知上限")?;
            let removed = isize::try_from(range.len()).map_err(|_| "表格替换超过原生通知上限")?;
            let delta = inserted
                .checked_sub(removed)
                .ok_or("表格长度变化超过原生通知上限")?;
            let edit = MarkdownHistoryTextEdit {
                replaced_utf16_range: range.clone(),
                change_in_utf16_length: delta,
                // 普通单格/追加保留原 caret；格内格式的真实映射直接进入同一 canonical Undo。
                selection_after: selection_after.unwrap_or(MarkdownTextSelection::caret(caret)),
            };
            let before = document.selection();
            // 此短借用内仅运行纯 Core：同一 replacement 包含当前格草稿和真实表尾追加。
            let prepared = document
                .replace_utf16_range_with_selection_observed(
                    range.clone(),
                    replacement,
                    before,
                    edit.selection_after,
                    observer,
                )
                .map_err(|error| error.to_string())?;
            Ok((edit, prepared))
        })();
        match result {
            Ok((edit, prepared)) => {
                // 在字符通知可能重入前发布原新代次票和字段基线；回调合同只允许纯 Rust。
                let (committed, _notify_guard) = self.publish_prepared_folds(&document, prepared);
                on_committed(&edit, &committed);
                let pending =
                    self.finish_canonical_edit(Some(&edit), Some(&committed), "table_plan");
                Ok((edit, committed, pending))
            }
            Err(error) => {
                // 拒绝仍平衡唯一编辑组；正文、草稿和历史保持原状，首因直接返回。
                drop(self.finish_canonical_edit(None, None, "table_plan"));
                Err(error)
            }
        }
    }

    /// 插表明确第一格为同一次历史编辑的 after；不在提交后另发一份选择转换。
    pub(super) fn apply_table_insertion(
        &self,
        captured: &NativeMarkdownCapturedSelection,
        range: NSRange,
        replacement: &str,
        after: MarkdownTextSelection,
        is_current: &impl Fn() -> bool,
    ) -> Result<
        (
            MarkdownHistoryTextEdit,
            Rc<NativeMarkdownCommittedSelection>,
            Option<NativeMarkdownPendingSelectionGuard>,
        ),
        String,
    > {
        self.apply_explicit_replacement_inner(
            range,
            replacement,
            Some(captured),
            Some(after),
            is_current,
        )
    }

    fn apply_explicit_replacement_inner(
        &self,
        range: NSRange,
        replacement: &str,
        captured: Option<&NativeMarkdownCapturedSelection>,
        selection_after: Option<MarkdownTextSelection>,
        is_current: &impl Fn() -> bool,
    ) -> Result<
        (
            MarkdownHistoryTextEdit,
            Rc<NativeMarkdownCommittedSelection>,
            Option<NativeMarkdownPendingSelectionGuard>,
        ),
        String,
    > {
        // 原生通知/测量可能重入关闭；栈内载荷租约结束前预检必须拒绝。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

        if !self.ivars().accepts_edits.get() {
            // A delayed import cannot publish text after the accepted save/close boundary.
            return Err("笔记编辑器正在关闭".into());
        }
        // Validate native arithmetic before beginning an editing group or acquiring mutable text.
        let Some(range_end) = range.location.checked_add(range.length) else {
            // Native coordinate failure must be visible even before a canonical transaction starts.
            return Err("插入范围超过 UTF-16 坐标上限".into());
        };
        let inserted = replacement.encode_utf16().count();
        let Some(caret) = range.location.checked_add(inserted) else {
            // Native coordinate failure must be visible even before a canonical transaction starts.
            return Err("插入位置超过 UTF-16 坐标上限".into());
        };
        let inserted_delta = isize::try_from(inserted).map_err(|_| {
            // Signed native notification limits reject this complete insertion without partial source mutation.
            "插入内容超过原生文本通知的长度上限"
        })?;
        let removed_delta = isize::try_from(range.length).map_err(|_| {
            // Signed native notification limits reject this complete insertion without partial source mutation.
            "替换范围超过原生文本通知的长度上限"
        })?;
        let Some(delta) = inserted_delta.checked_sub(removed_delta) else {
            // Native coordinate failure must be visible even before a canonical transaction starts.
            return Err("文本长度变化超过原生坐标上限".into());
        };
        let Some(document) = self.document() else {
            // An absent canonical owner is a finite rejection, never a guessed no-op.
            return Err("编辑器已关闭".into());
        };
        let edit = MarkdownHistoryTextEdit {
            replaced_utf16_range: range.location..range_end,
            change_in_utf16_length: delta,
            selection_after: selection_after.unwrap_or_else(|| MarkdownTextSelection::caret(caret)),
        };
        let observer = self.prepare_fold_observer(&document)?;
        let observer = match captured {
            Some(captured) => observer
                .with_native_before(captured)
                .map_err(|error| error.to_string())?,
            None => observer, // 原导入路径没有额外选择票，仍守其既有 Core before。
        };
        if !is_current() {
            return Err("原文本插入意图已改变".into());
        }
        self.beginEditing();
        let origin_current = is_current(); // beginEditing 可重入，先在 Doc 可变借用之外核原动作。
        let result = {
            let mut current = document.borrow_mut();
            if !origin_current
                || !self.ivars().accepts_edits.get()
                || !observer.is_current(&current)
            {
                Err("折叠对应的笔记状态已变更".to_owned())
            } else {
                current
                    .replace_utf16_range_with_selection_observed(
                        edit.replaced_utf16_range.clone(),
                        replacement,
                        captured.map_or(
                            MarkdownTextSelection {
                                anchor_utf16: range.location,
                                head_utf16: range_end,
                            },
                            |captured| captured.selection,
                        ),
                        edit.selection_after,
                        observer,
                    )
                    .map_err(|error| error.to_string())
            }
        };
        match result {
            // Native observers receive exactly one change after the core committed all capacity/history checks.
            Ok(prepared) => {
                let (committed, _notify_guard) = self.publish_prepared_folds(&document, prepared);
                let pending = self.finish_canonical_edit(Some(&edit), Some(&committed), "explicit");
                Ok((edit, committed, pending))
            }
            // Rejection balances the group and returns ownership to the pending attachment's rollback guard.
            Err(error) => {
                drop(self.finish_canonical_edit(None, None, "explicit"));
                Err(error.to_string())
            }
        }
    }

    /// Find 的匿名记录只在此消费；单次纯事务结束借用后才发布一个 TextKit 几何通知。
    pub(super) fn apply_find_replacements<'a, I, P>(
        &self,
        captured: &NativeMarkdownCapturedSelection,
        replacements: I,
        generation: u64,
        before: MarkdownTextSelection,
        after: MarkdownTextSelection,
    ) -> Result<NativeMarkdownCanonicalPublication<Option<MarkdownHistoryTextEdit>>, String>
    where
        I: IntoIterator<
            Item = Result<
                omni_markdown_core::MarkdownReplacement<P>,
                omni_markdown_core::MarkdownDocumentBufferError,
            >,
        >,
        P: IntoIterator<
            Item = Result<
                omni_markdown_core::MarkdownReplacementPart<'a>,
                omni_markdown_core::MarkdownDocumentBufferError,
            >,
        >,
    {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if !self.ivars().accepts_edits.get() {
            return Err("笔记编辑器正在关闭".into()); // accepted 后的迟到 Find 不能创建新正文。
        }
        let document = self.document().ok_or("笔记文档已关闭")?;
        let observer = self
            .prepare_fold_observer(&document)?
            .with_native_before(captured)
            .map_err(|error| error.to_string())?;
        self.beginEditing();
        let result = {
            // beginEditing 本身可重入；最终代次/权限与纯事务取得同一个短可变借用后核对。
            let mut document = document.borrow_mut();
            if !self.ivars().accepts_edits.get()
                || document.generation() != generation
                || !observer.is_current(&document)
            {
                Err("查找替换期间笔记文档已变更".to_owned())
            } else {
                document
                    .apply_replacements_observed(replacements, before, after, observer)
                    .map_err(|error| error.to_string())
            }
        };
        match result {
            Ok((edit, prepared)) => {
                let (committed, _notify_guard) = self.publish_prepared_folds(&document, prepared);
                // 包括 0 匹配 None；通知路径平衡 begin/end，只有实际提交才产生字符变更/历史。
                let pending = self.finish_canonical_edit(edit.as_ref(), Some(&committed), "find");
                Ok((edit, committed, pending))
            }
            Err(error) => {
                // 输入流/容量/坐标失败已在 core 完整回滚；只结束 Native group，不部分发布。
                drop(self.finish_canonical_edit(None, None, "find"));
                Err(error)
            }
        }
    }

    pub(super) fn apply_format_captured(
        &self,
        action: MarkdownFormatAction,
        captured: NativeMarkdownCapturedSelection,
    ) -> Result<
        (
            MarkdownHistoryTextEdit,
            Rc<NativeMarkdownCommittedSelection>,
            Option<NativeMarkdownPendingSelectionGuard>,
        ),
        String,
    > {
        self.apply_format_with_selection(action, captured.selection, Some(&captured))
    }
    /// 原capture跨beginEditing保持原权限；不能把setter重入后的fold代次贴给旧菜单操作。
    fn apply_format_with_selection(
        &self,
        action: MarkdownFormatAction,
        selection: MarkdownTextSelection,
        captured: Option<&NativeMarkdownCapturedSelection>,
    ) -> Result<
        (
            MarkdownHistoryTextEdit,
            Rc<NativeMarkdownCommittedSelection>,
            Option<NativeMarkdownPendingSelectionGuard>,
        ),
        String,
    > {
        // 原生通知/测量可能重入关闭；栈内载荷租约结束前预检必须拒绝。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

        if !self.ivars().accepts_edits.get() {
            // A closing storage has no authority to create a format/keyboard revision after its last save.
            return Err("笔记编辑器正在关闭".into());
        }
        let Some(document) = self.document() else {
            // An absent canonical owner is a finite rejection, never a guessed no-op.
            return Err("编辑器已关闭".into());
        };
        let observer = self.prepare_fold_observer(&document)?;
        let observer = match captured {
            Some(captured) => observer
                .with_native_before(captured)
                .map_err(|error| error.to_string())?,
            None => observer, // 没有 Native 观察的纯合成入口仍保留原 Core before。
        };
        self.beginEditing();
        let result = {
            let mut current = document.borrow_mut();
            if !self.ivars().accepts_edits.get() || !observer.is_current(&current) {
                Err("折叠对应的笔记状态已变更".to_owned())
            } else {
                current
                    .apply_format_observed(action, selection, observer)
                    .map_err(|error| error.to_string())
            }
        };
        match result {
            // Atomic preparation supplies one before-document interval and final canonical selection.
            Ok((edit, prepared)) => {
                let (committed, _notify_guard) = self.publish_prepared_folds(&document, prepared);
                let pending = self.finish_canonical_edit(Some(&edit), Some(&committed), "format");
                Ok((edit, committed, pending))
            }
            // A rejected format preserves history/generation/source and still closes its native edit group.
            Err(error) => {
                drop(self.finish_canonical_edit(None, None, "format"));
                Err(error.to_string())
            }
        }
    }

    /// Apply a canonical keyboard command while preserving the distinction between fallback and consumed no-op.
    pub(super) fn apply_editing_command_captured(
        &self,
        command: MarkdownEditingCommand,
        captured: NativeMarkdownCapturedSelection,
    ) -> Result<
        (
            MarkdownEditingOutcome,
            Rc<NativeMarkdownCommittedSelection>,
            Option<NativeMarkdownPendingSelectionGuard>,
        ),
        String,
    > {
        let selection = captured.selection;
        // 原生通知/测量可能重入关闭；栈内载荷租约结束前预检必须拒绝。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

        if !self.ivars().accepts_edits.get() {
            // A closing storage has no authority to create a format/keyboard revision after its last save.
            return Err("笔记编辑器正在关闭".into());
        }
        let Some(document) = self.document() else {
            // An absent canonical owner is a finite rejection, never a guessed no-op.
            return Err("编辑器已关闭".into());
        };
        let observer = self
            .prepare_fold_observer(&document)?
            .with_native_before(&captured)
            .map_err(|error| error.to_string())?;
        self.beginEditing();
        let result = {
            let mut current = document.borrow_mut();
            if !self.ivars().accepts_edits.get() || !observer.is_current(&current) {
                Err("折叠对应的笔记状态已变更".to_owned())
            } else {
                current
                    .apply_editing_command_observed(command, selection, observer)
                    .map_err(|error| error.to_string())
            }
        };
        let result = result.map(|(outcome, prepared)| {
            let (committed, notify_guard) = self.publish_prepared_folds(&document, prepared);
            (outcome, committed, notify_guard)
        });
        match result {
            // Only a committed changed result publishes source/selection notifications and autosave dirtiness.
            Ok((MarkdownEditingOutcome::Changed(edit), committed, _notify_guard)) => {
                let pending = self.finish_canonical_edit(Some(&edit), Some(&committed), "keyboard");
                Ok((MarkdownEditingOutcome::Changed(edit), committed, pending))
            }
            // Fallback and consumed no-op both close the group without fabricating text/history changes.
            Ok((outcome, committed, _notify_guard)) => {
                let pending = self.finish_canonical_edit(None, None, "keyboard");
                Ok((outcome, committed, pending))
            }
            // Capacity, scalar or context errors veto the native command without a partial edit.
            Err(error) => {
                drop(self.finish_canonical_edit(None, None, "keyboard"));
                Err(error.to_string())
            }
        }
    }

    /// Restore one structural history root and forward its exact receipt through the same native publisher.
    pub(super) fn restore_history(
        &self,
        direction: MarkdownHistoryDirection,
    ) -> Result<Option<NativeMarkdownCanonicalPublication<MarkdownHistoryTextEdit>>, String> {
        // 原生通知/测量可能重入关闭；栈内载荷租约结束前预检必须拒绝。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

        if !self.ivars().accepts_edits.get() {
            // History is immutable after the accepted close gate, even while AppKit retains its shell.
            return Err("笔记编辑器已关闭".into());
        }
        let Some(document) = self.document() else {
            // Expired native shells cannot reestablish a canonical history owner.
            return Err("笔记编辑器已关闭".into());
        };
        let observer = self.prepare_fold_observer(&document)?;
        self.beginEditing();
        let result = {
            // End the canonical mutable borrow before native observers can request strings/attributes again.
            let mut document = document.borrow_mut();
            if !self.ivars().accepts_edits.get() || !observer.is_current(&document) {
                Err("折叠对应的笔记状态已变更".to_owned())
            } else {
                match direction {
                    // Undo restores the previous shared root and its original semantic selection.
                    MarkdownHistoryDirection::Undo => document.undo_text_edit_observed(observer),
                    // Redo restores the next root unless a new accepted edit invalidated that branch.
                    MarkdownHistoryDirection::Redo => document.redo_text_edit_observed(observer),
                }
                .map_err(|error| error.to_string())
            }
        };
        match result {
            Ok(result) => {
                let result = result.map(|(edit, prepared)| {
                    let (committed, notify_guard) =
                        self.publish_prepared_folds(&document, prepared);
                    (edit, committed, notify_guard)
                });
                let pending = self.finish_canonical_edit(
                    result.as_ref().map(|(edit, _, _)| edit),
                    result.as_ref().map(|(_, committed, _)| committed),
                    "history",
                );
                // 原安装 lease 跨过同步历史通知；同一 pending guard 继续交给调用方完成明确 after 安装。
                Ok(result.map(|(edit, committed, _notify_guard)| (edit, committed, pending)))
            }
            Err(error) => {
                drop(self.finish_canonical_edit(None, None, "history"));
                Err(error)
            }
        }
    }

    /// 平衡一个 beginEditing；原 Rc 的有限 pending owner 覆盖所有通知并严格拒绝未知选择暂态。
    fn finish_canonical_edit(
        &self,
        edit: Option<&MarkdownHistoryTextEdit>,
        _committed: Option<&Rc<NativeMarkdownCommittedSelection>>,
        _source: &'static str,
    ) -> Option<NativeMarkdownPendingSelectionGuard> {
        let pending_trace = match (edit, _committed) {
            (Some(edit), Some(committed)) => Some(NativeMarkdownPendingSelectionGuard {
                slot: Rc::clone(&self.ivars().pending_selection),
                owner: self.begin_pending_selection(
                    committed,
                    NativeMarkdownPendingSelectionSources::canonical(_source, edit),
                ),
            }),
            _ => None, // 无字符提交没有 pending owner，不为旧范围重新签票。
        };
        if let Some(edit) = edit {
            // 原 owner 已固定 before/after 与原长度派生来源；通知期间只能消费该有限声明。
            if let Some(trace) = &pending_trace {
                self.pending_selection_phase(
                    &trace.owner,
                    NativeMarkdownPendingSelectionPhase::EditedNotification,
                );
            }
            self.edited_range_changeInLength(
                NSTextStorageEditActions::EditedCharacters,
                NSRange::new(
                    edit.replaced_utf16_range.start,
                    edit.replaced_utf16_range.len(),
                ),
                edit.change_in_utf16_length,
            );
            if let Some(trace) = &pending_trace {
                self.pending_selection_phase(
                    &trace.owner,
                    NativeMarkdownPendingSelectionPhase::Attributes,
                );
            }
            self.invalidate_presentation_attributes();
        }
        // 成功、无变化、回退与拒绝均恰好结束一次编辑组，测量 owner 不重排通知。
        if let Some(trace) = &pending_trace {
            self.pending_selection_phase(
                &trace.owner,
                NativeMarkdownPendingSelectionPhase::EndEditing,
            );
        }
        self.endEditing();
        if let Some(trace) = &pending_trace {
            self.pending_selection_phase(
                &trace.owner,
                NativeMarkdownPendingSelectionPhase::SuperclassTail,
            );
            // Undo/格式/程序编辑共用同一原字符完成边界，不依赖新选择通知间接刷新正文。
            self.invalidate_committed_view(&trace.owner.committed);
        }
        if edit.is_some() {
            // 只在真实字符提交及 Native 通知结束后排队原弱前台保存动作。
            #[cfg(feature = "native-markdown-runtime-probe")]
            queue_native_markdown_autosave_for_storage(self);
        }
        pending_trace // 同一实际 owner 移交调用方，直到原 explicit-after 安装及 Native claims 析构结束。
    }

    /// 原生 getter/retain 返回后只核对 Rust 状态；没有状态借用跨越原生调用。
    fn attribute_request_is_current(&self, request: &AttributeRequest) -> bool {
        if !self.ivars().accepts_edits.get()
            || self.presentation_mode() != request.mode
            || self.ivars().font_run_scratch.epoch() != request.semantic_epoch
            || self.ivars().table_geometry_epoch.get() != request.table_geometry_epoch
        {
            // 关闭或模式改变使旧请求失效，不能将原有 owner 写回。
            return false;
        }
        let identity = self.ivars().document.borrow().clone();
        if !std::rc::Weak::ptr_eq(&identity, &request.identity) {
            // Weak 保留旧分配身份，阻止相同 generation/地址复用造成的笔记 ABA。
            return false;
        }
        identity.upgrade().is_some_and(|document| {
            // 仅读 canonical 代次，不进入 AppKit；临时强 owner 不进入缓存。
            document.borrow().generation() == request.generation
        })
    }

    /// 字体 LRU 唯一借到局部；重入保持真实字体策略，但不创建第二个 LRU。
    fn resolve_attribute_font(
        &self,
        styles: MarkdownPresentationStyles,
        scalar: char,
        render: &MarkdownStorageRenderPayload,
        request: &AttributeRequest,
    ) -> Option<MarkdownResolvedFont> {
        let result = match self.ivars().font_selection_cache.begin() {
            CacheAdmission::Ready(mut lease) => {
                // native factory/retain/drop 仅操作局部 owner，slot 已结束 RefMut。
                let result = lease.payload_mut().resolve(
                    styles,
                    scalar,
                    &render.palette,
                    request.mode == MarkdownPresentationMode::Source,
                );
                if !self.attribute_request_is_current(request) {
                    // 外层计算期间失效也推进 epoch；旧 owner 不可作为后续查询缓存。
                    self.ivars().font_selection_cache.invalidate();
                    lease.discard();
                }
                result
            }
            CacheAdmission::Loaned => {
                // 没有 LRU 容量或第二个发布 owner；仍选择同样的样式与 Unicode fallback。
                MarkdownFontSelectionCache::resolve_uncached(
                    styles,
                    scalar,
                    &render.palette,
                    request.mode == MarkdownPresentationMode::Source,
                )
            }
            CacheAdmission::Closed => {
                // accepted release 的空壳拒绝原生字体计算。
                return None;
            }
        };
        self.attribute_request_is_current(request).then_some(result)
    }

    /// 字典 factory 也在唯一局部 owner 上执行；Loaned 分支不预留临时 LRU。
    fn cached_attribute_dictionary(
        &self,
        styles: MarkdownPresentationStyles,
        font: &Retained<NSFont>,
        render: &MarkdownStorageRenderPayload,
        request: &AttributeRequest,
    ) -> Option<Retained<MarkdownTextAttributes>> {
        let source = request.mode == MarkdownPresentationMode::Source;
        let list_role = request.list_role;
        let variant = MarkdownTextAttributeVariant {
            source_mode: source,
            table_row_height: request.table_row_height,
            list_role,
        };
        let result = match self.ivars().attribute_cache.begin() {
            CacheAdmission::Ready(mut lease) => {
                // 原生属性构造、retain 与 eviction drop 均不持 slot RefMut。
                let result = lease.payload_mut().get_or_insert(
                    styles,
                    font,
                    &render.palette,
                    &render.base_attributes,
                    variant,
                );
                if !self.attribute_request_is_current(request) {
                    // 样式 key 不授权旧文档请求在模式/代次失效后恢复 owner。
                    self.ivars().attribute_cache.invalidate();
                    lease.discard();
                }
                result
            }
            CacheAdmission::Loaned => {
                if list_role == MarkdownListGlyphRole::Literal
                    && styles.is_empty()
                    && native_markdown_same_font(font, &render.palette.normal_font)
                {
                    // 普通正文继续共享同一个 base 字典，不分配缓存或属性副本。
                    render.base_attributes.clone()
                } else {
                    // 其他样式按完全相同构造函数临时计算，不能伪造 base 字体。
                    build_markdown_text_attributes_with_row_height(
                        styles,
                        font,
                        &render.palette,
                        variant,
                    )
                }
            }
            CacheAdmission::Closed => {
                // 已关闭 owner 不再取得新的原生字典资源。
                return None;
            }
        };
        self.attribute_request_is_current(request).then_some(result)
    }

    /// 只扫描当前 semantic/request/cache 交集；coverage 查询不持任何 RefCell 借用。
    fn font_attributes_for_units(
        &self,
        units: &[u16],
        start: usize,
        scalar_index: usize,
        styles: MarkdownPresentationStyles,
        render: &MarkdownStorageRenderPayload,
        request: &AttributeRequest,
    ) -> Option<(Retained<MarkdownTextAttributes>, NSRange)> {
        let list_role = request.list_role;
        let local_index = scalar_index.checked_sub(start)?;
        let scalar = native_markdown_utf16_scalar_at(units, local_index)?;
        if list_role == MarkdownListGlyphRole::Hidden {
            // 已证明可替换的任务语法只缩小本次有限run；不需要给Core新增或改写style bit。
            let attributes = self.cached_attribute_dictionary(
                styles,
                &render.palette.hidden_font,
                render,
                request,
            )?;
            return self
                .attribute_request_is_current(request)
                .then_some((attributes, NSRange::new(start, units.len())));
        }
        if let Some(glyphs) = render.palette.list_glyphs.as_ref()
            && glyphs.info(list_role).is_some()
        {
            // Core已将该run限定到一个源unit；使用已验证的符号face，不能先按原'-'覆盖率选字体。
            let attributes =
                self.cached_attribute_dictionary(styles, &glyphs.font, render, request)?;
            return self.attribute_request_is_current(request).then_some((
                attributes,
                NSRange::new(start + scalar.start, scalar.end - scalar.start),
            ));
        }
        let resolved = self.resolve_attribute_font(styles, scalar.value, render, request)?;
        let range = native_markdown_font_run_range(
            units,
            start,
            local_index,
            &resolved.base_coverage,
            resolved.fallback_coverage.as_deref(),
        );
        let attributes =
            self.cached_attribute_dictionary(styles, &resolved.font, render, request)?;
        // 最后一次 native 调用返回后核验原 owner，过期临时 native claims 在本栈释放。
        self.attribute_request_is_current(request)
            .then_some((attributes, NSRange::new(range.start, range.len())))
    }

    /// Resolves a scalar-aligned bounded semantic/font run for one canonical index.
    pub(super) fn attributes_at_index(
        &self,
        index: usize,
    ) -> (Retained<MarkdownTextAttributes>, NSRange) {
        #[cfg(feature = "native-block-fragment-diagnostic")]
        self.ivars().mode_attribute_diagnostic.query(index);
        // 原生通知/测量可能重入关闭；栈内载荷租约结束前预检必须拒绝。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

        let Some(render) = self.render_payload() else {
            // accepted release 后不得重新创建缓存或派生属性。
            return (MarkdownTextAttributes::new(), NSRange::new(0, 0));
        };

        // Lazy NSTextStorage must consume its pending attribute-fix bookkeeping.
        // Our finite fixAttributesInRange: override derives no mutable attribute mirror.
        let current_length = self
            .document()
            .map_or(0, |document| document.borrow().utf16_len());
        if index < current_length {
            // In-document accesses consume the finite lazy-fix contract before deriving a run.
            self.ensureAttributesAreFixedInRange(NSRange::new(index, 1));
        }
        let Some(document) = self.document() else {
            // A disconnected adapter has no text range and must not retain a replacement document.
            return (self.base_attributes(), NSRange::new(0, 0));
        };
        let document_ref = document.borrow();
        let document_length = document_ref.utf16_len();
        if document_length == 0 || index >= document_length {
            // 空/EOF只有空range，但仍需原Source健康；不能通过无字符请求绕过永久I/O错误。
            let health = document_ref.copy_utf16_code_units(document_length, &mut []);
            drop(document_ref);
            if let Err(error) = health {
                drop(document);
                drop(render);
                drop(_payload_use);
                return markdown_attribute_read_or_throw(Err(error));
            }
            return (self.base_attributes(), NSRange::new(document_length, 0));
        }

        // 所有边界必须由同一健康canonical证明；失败不能发布空range假装该字符有有效属性。
        let geometry = markdown_attribute_request_window(&document_ref, index).and_then(
            |(window, scalar_index)| {
                let start = markdown_utf16_boundary_down(&document_ref, scalar_index)?;
                let end =
                    markdown_utf16_boundary_up(&document_ref, scalar_index.saturating_add(1))?;
                Ok((window, scalar_index, start..end))
            },
        );
        let (mut requested_window, scalar_index, scalar_range) = match geometry {
            Ok(geometry) => geometry,
            Err(error) => {
                // 外来异常前明确结束Doc借用、业务强租约和payload guard；不重签另一源或写effectiveRange。
                drop(document_ref);
                drop(document);
                drop(render);
                drop(_payload_use);
                return markdown_attribute_read_or_throw(Err(error));
            }
        };
        #[cfg(feature = "native-block-fragment-diagnostic")]
        self.ivars()
            .mode_attribute_diagnostic
            .request(requested_window.len());
        let generation = document_ref.generation();
        drop(document_ref);

        // 属性 run 不得跨越不同高度的 row；未投影 gap 也不能覆盖相邻的已测高行。
        let (table_row_height, height_range) =
            self.table_row_attributes_at(&document, generation, scalar_index, document_length);
        requested_window.start = requested_window.start.max(height_range.start);
        requested_window.end = requested_window.end.min(height_range.end);

        let mut request = AttributeRequest {
            identity: Rc::downgrade(&document),
            generation,
            mode: self.presentation_mode(),
            semantic_epoch: self.ivars().font_run_scratch.epoch(),
            table_geometry_epoch: self.ivars().table_geometry_epoch.get(),
            table_row_height,
            list_role: MarkdownListGlyphRole::Literal,
        };

        let (mut styles, range, list_role) = if let Some(query) =
            self.source_leaf_query(scalar_index)
        {
            // 同一来源只签Root Code/Rule的真实交集；命中不读取或重建legacy block envelope。
            // 代码与分隔线没有list glyph角色，仍由下面同一字体/Unicode/字典管线发布。
            (
                query.styles,
                query.utf16_range,
                MarkdownListGlyphRole::Literal,
            )
        } else {
            let mut presentation = self.ivars().presentation.borrow_mut();
            if presentation.needs_rebuild(generation, scalar_index) {
                // 首屏、普通inline、容器和缺checkpoint区域保持原完整功能；尚未消除其legacy物化。
                let document_ref = document.borrow();
                if let Ok(slice) = document_ref.materialize_markdown_viewport(
                    requested_window.clone(),
                    MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS,
                ) {
                    match slice.semantic_status {
                        MarkdownViewportSemanticStatus::Complete => {
                            let _ =
                                presentation.rebuild(generation, slice.utf16_range, &slice.text);
                        } // 只有原完整上下文才授予对应旧语义，不能用SourceReady空洞伪装Plain。
                        MarkdownViewportSemanticStatus::SourceFallback => {
                            let _ = presentation
                                .record_source_fallback_window(generation, slice.utf16_range);
                        } // 原失败回退仍保持source可读，不新增扩大解析或隐藏。
                    }
                } // 原有限失败保持未知；不为Code Ready缺失而在getter里推进Global。
            }
            let query = presentation.query(scalar_index);
            let (styles, mut range) = query.map_or_else(
                || {
                    (
                        MarkdownPresentationStyles::default(),
                        requested_window.clone(),
                    )
                },
                |query| (query.styles, query.utf16_range.start..query.utf16_range.end),
            );
            // 旧list token和gap仍截断其本地样式，不能把普通空格延伸到另一字形角色。
            if let Some(boundary) = presentation.list_marker_boundary_range_at(scalar_index) {
                range.start = range.start.max(boundary.start);
                range.end = range.end.min(boundary.end);
            }
            let list_role = native_markdown_list_role(
                presentation.list_marker_at(scalar_index),
                scalar_index,
                &mut range,
                request.mode == MarkdownPresentationMode::InlinePreview
                    && render.palette.list_glyphs.is_some(),
            );
            (styles, range, list_role)
        }; // 分支返回前结束所有Core与presentation借用，后续Native调用可重入。
        request.list_role = list_role; // 纯语义角色签入同一请求；后续Native返回仍核原身份与epoch。

        if styles.contains(MarkdownPresentationStyles::TABLE) && table_row_height.is_none() {
            // 没有同epoch的完整行测量就显示literal source，禁止placeholder先隐藏未准备内容。
            styles = MarkdownPresentationStyles::default();
        }

        // Clamp semantic runs before Unicode fallback subdivision so every returned interval stays finite.
        let style_range = clamp_markdown_attribute_range(
            range,
            requested_window.clone(),
            scalar_index,
            document_length,
        );
        let style_range_end = style_range
            .location
            .checked_add(style_range.length)
            .unwrap_or(style_range.location)
            .min(document_length);
        let result = match self.ivars().font_run_scratch.begin() {
            CacheAdmission::Ready(mut lease) => {
                // 主 buffer 独占于同步栈；native 调用期间 slot 没有 RefMut 或第二个 owner。
                let result = lease
                    .payload_mut()
                    .prepare(
                        &document,
                        style_range.location..style_range_end,
                        scalar_range.clone(),
                    )
                    .and_then(|range| {
                        // 交集的两端来自已验证 scalar 边界，字体扫描不能越过当前语义/请求。
                        self.font_attributes_for_units(
                            lease.payload_mut().slice(&range),
                            range.start,
                            scalar_index,
                            styles,
                            &render,
                            &request,
                        )
                    });
                if !self.attribute_request_is_current(&request) {
                    // 封闭或代次改变时丢弃内容、key 与 Weak 身份，不能恢复旧 cache。
                    self.ivars().font_run_scratch.invalidate();
                    lease.discard();
                }
                result
            }
            CacheAdmission::Loaned => {
                // 重入只复制完整查询 scalar 到固定栈数组，绝不分配第二个 4096-unit buffer。
                let mut units = [0_u16; 2];
                if let Some(count) = copy_query_scalar(&document, &scalar_range, &mut units) {
                    // 短 effective range 合法，仍通过完全相同的真实样式/Unicode 字体策略。
                    self.font_attributes_for_units(
                        &units[..count],
                        scalar_range.start,
                        scalar_index,
                        styles,
                        &render,
                        &request,
                    )
                } else {
                    // canonical 范围失败不把部分单元或 replacement 字符发布为有效字体结果。
                    None
                }
            }
            CacheAdmission::Closed => {
                // accepted release 后不再重建 scratch 或字体资源。
                None
            }
        };
        result.unwrap_or_else(|| {
            // 失效请求不发布旧属性；使用已有有限失败回退并重新取得当前 scalar 边界。
            (
                self.base_attributes(),
                self.composition_base_attribute_range(index),
            )
        })
    }

    /// 真实语义转换才推进身份；普通窗口换页和行高安装不能消耗此域。
    fn revoke_presentation_semantics(&self) {
        self.cancel_source_semantics(); // 真实编辑/完整选择/模式撤权同步结束原来源，旧队列不能保活或重签。
        let epoch = &self.ivars().presentation_semantic_epoch;
        // checked 失败保持 None，后续任何调用都不能回绕为旧快照持有的身份。
        epoch.set(epoch.get().and_then(|current| current.checked_add(1)));
    }

    /// canonical 编辑后撤销派生语义，并只通知原缓存实际覆盖的范围。
    pub(super) fn invalidate_presentation_attributes(&self) {
        self.revoke_presentation_semantics();
        // 即使 buffer 正被 native 栈使用也推进 epoch，旧 loan 不得归还为新正文缓存。
        self.ivars().font_run_scratch.invalidate();
        let invalidated = self
            .ivars()
            .presentation
            .borrow_mut()
            .invalidate_document_generation();
        self.notify_or_defer_attribute_range(invalidated);
    }

    /// 同步 canonical 的完整选区和预览标记显隐；源码选区不触发无关的原生属性重建。
    pub(super) fn update_active_selection(
        &self,
        captured: NativeMarkdownCapturedSelection,
    ) -> Option<Rc<NativeMarkdownCommittedSelection>> {
        self.update_active_selection_observed(captured, |_| {})
    }

    /// 同一纯选择转换的观察者只可保存原Rc收据；回调发生在任何属性通知之前且不持Doc借用。
    pub(super) fn update_active_selection_observed(
        &self,
        captured: NativeMarkdownCapturedSelection,
        on_committed: impl FnOnce(&Rc<NativeMarkdownCommittedSelection>),
    ) -> Option<Rc<NativeMarkdownCommittedSelection>> {
        if !self.ivars().accepts_edits.get() {
            // 已关闭空壳不能在等待原生销毁时修改Core选择或历史元数据。
            return None;
        }
        // pending只覆盖同步NSTextStorage字符事务；其中暂态选择不能取代刚提交的Core收据。
        if self
            .ivars()
            .pending_selection
            .borrow()
            .as_ref()
            .is_some_and(|owner| owner.current())
        {
            // 活的字符通知不能自己发布选择；已撤权 tombstone 不阻止新输入凭新原票显式发布。
            // 后继仍须通过下面 captured.current_in；旧通知不能因 tombstone 而重新 capture。
            return None;
        }
        let selection = captured.selection;
        let selection_start = selection.anchor_utf16.min(selection.head_utf16);
        let selection_end = selection.anchor_utf16.max(selection.head_utf16);
        let document = self.document()?;
        if !captured.belongs_to(&document) {
            return None;
        }
        let (selection_changed, committed) = {
            let mut current = document.borrow_mut();
            if !captured.current_in(&current) {
                return None;
            }
            let changed = current.selection() != selection;
            if current.set_selection(selection).is_err() {
                return None;
            }
            // 同一纯栈发布完整方向与自身转换收据；通知之前已经撤旧capture，不借新Native值续权。
            let committed = Rc::new(captured.publish_head(changed));
            (changed, committed)
        };
        if selection_changed {
            // 完整 anchor/head 发生真实转换即撤旧票，方向往返也不能复活原快照。
            self.revoke_presentation_semantics();
        }
        // Find等原请求须在第一次Native重入前接住这份精确收据，不能通知后重新读新代次签票。
        on_committed(&committed);
        if selection_changed {
            // selection 可改变隐藏 marker 样式而不改变正文 generation；只使旧请求 epoch 失效。
            // Ready 的 UTF-16 内容仍完全相同，不清 buffer 或制造重复 copy。
            self.ivars().font_run_scratch.invalidate_loans();
        }
        let invalidated = self
            .ivars()
            .presentation
            .borrow_mut()
            .set_active_selection(selection_start..selection_end);
        // 同步属性通知前先声明原Core before/after；通知撤权时不再发布较新的安装票。
        let notify_guard = committed.begin_pre_notify_install().ok()?;
        if self.presentation_mode() == MarkdownPresentationMode::InlinePreview {
            // 预览的隐藏标记影响字体和行高，仍按原有限覆盖通知 TextKit 重取属性。
            self.notify_or_defer_attribute_range(invalidated);
        }
        // 源码投影始终显示原字符，HIDDEN_MARKER 不参与其字体、颜色和段落几何。
        // Rust 语义与在途 loan 已照常撤权；模式切回预览仍由 set_presentation_mode
        // 发布完整属性失效，故这里仅省掉选区移动导致的冗余原生属性通知。
        drop(notify_guard); // 紧邻通知的lease结束；显示setter须以同一收据另开栈内guard。
        committed.current().then_some(committed)
    }

    /// Changes derived source/preview presentation without changing canonical Markdown bytes.
    /// 宽度失效只撤销标量权限，不在原生 resize 栈中发布属性通知或触发布局。
    pub(super) fn invalidate_table_projection_width(&self, width_key: i64) {
        if !self.ivars().accepts_edits.get() {
            // 迟到 resize 不给已释放空壳取得新的派生权限。
            return;
        }
        if self.ivars().table_projection_width_key.replace(width_key) == width_key {
            // 纯高度变化不改变列宽和换行结果，不使稳定投影反复失效。
            return;
        }
        self.advance_table_geometry_epoch();
    }

    /// 在途查询和绘制使用同一个单调 epoch，不能把旧高度写回新视口。
    #[cfg_attr(feature = "native-table-edit-diagnostic", track_caller)]
    fn advance_table_geometry_epoch(&self) -> u64 {
        let epoch = self
            .ivars()
            .table_geometry_epoch
            .get()
            .checked_add(1)
            .expect("table geometry epoch overflow");
        self.ivars().table_geometry_epoch.set(epoch);
        #[cfg(feature = "native-table-edit-diagnostic")]
        if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
            // 只报原代次推进的调用点，不读取正文/Native或改变失效顺序。
            let caller = std::panic::Location::caller();
            eprintln!(
                "native_table_epoch next={epoch} caller={}:{} formal=false",
                caller.file(),
                caller.line()
            );
        }
        epoch
    }

    /// 仅读有限投影，返回高度和其属性边界；弱身份不能保活正文。
    fn table_row_attributes_at(
        &self,
        document: &SharedMarkdownDocument,
        generation: u64,
        index: usize,
        document_length: usize,
    ) -> (Option<f64>, std::ops::Range<usize>) {
        let projection = self.ivars().table_row_heights.borrow();
        if self.presentation_mode() == MarkdownPresentationMode::InlinePreview
            && let Some(projection) = projection.as_ref()
            && projection.matches(
                &Rc::downgrade(document),
                generation,
                self.ivars().table_projection_width_key.get(),
                self.ivars().table_geometry_epoch.get(),
            )
        {
            // 找到当前行或 gap 后直接返回纯范围，不调用原生或缓存 factory。
            return projection.attributes_at(index, document_length);
        }
        // 源模式或过期投影没有占位高度权限；普通属性仍被原有4KiB窗口约束。
        (None, 0..document_length)
    }

    /// 仅布局 executor 在真实 editing transaction 之外发布整批有限高度。
    pub(super) fn install_table_row_heights(
        &self,
        identity: &WeakMarkdownDocument,
        generation: u64,
        width_key: i64,
        mut rows: Vec<NativeMarkdownTableRowHeight>,
    ) -> Option<bool> {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if !self.ivars().accepts_edits.get()
            || self.ivars().native_input_depth.get() != 0
            || self.ivars().composition_active.get()
            || self.presentation_mode() != MarkdownPresentationMode::InlinePreview
            || self.ivars().table_projection_width_key.get() != width_key
        {
            // 输入或关闭阶段不能安装会触发 native attribute 回调的派生状态。
            return None;
        }
        let document = self.document()?;
        if !std::rc::Weak::ptr_eq(identity, &Rc::downgrade(&document)) {
            // 相同数值代次不授权另一笔记的高度。
            return None;
        }
        {
            let document = document.borrow();
            if document.generation() != generation
                || !NativeMarkdownTableRowHeightProjection::rows_are_valid_for_document(
                    &rows, &document,
                )
            {
                // 整批失败时保留旧状态，不发布部分行高。
                return None;
            }
        }
        let previous = self
            .ivars()
            .table_row_heights
            .borrow()
            .as_ref()
            .map(Rc::clone);
        if previous.as_ref().is_some_and(|old| {
            // 完全相同的高度不重复发属性通知，也不使闲置窗口持续排队。
            old.document_generation == generation
                && old.width_key == width_key
                && old.epoch == self.ivars().table_geometry_epoch.get()
                && std::rc::Weak::ptr_eq(&old.document_identity, identity)
                && (old.rows == rows || old.covers_identical_rows(&rows))
        }) {
            // 完整旧投影仍不超过256行；新批次只消费其中同值行，不因收窄而反复恢复/隐藏离屏源。
            // 关闭、mode/width/g变化、空撤销和任何真实高度差异都不能走此分支。
            return Some(false);
        }
        if let Some(old) = previous.as_ref()
            && old.matches(
                identity,
                generation,
                width_key,
                self.ivars().table_geometry_epoch.get(),
            )
        {
            // 滚动只新增邻近行时，保留同一语义epoch的已测高度，防止视口上方先恢复literal
            // 再缩回placeholder而反复移动Native坐标。合并最多256行；mode/width/g/语义撤权不复用。
            rows = old.merge_known_rows(rows);
        }
        let epoch = self.advance_table_geometry_epoch();
        let projection = Rc::new(NativeMarkdownTableRowHeightProjection {
            document_identity: identity.clone(),
            document_generation: generation,
            width_key,
            epoch,
            rows,
        });
        let obsolete = self
            .ivars()
            .table_row_heights
            .replace(Some(Rc::clone(&projection)));
        self.ivars().font_run_scratch.invalidate();
        self.ivars().attribute_cache.invalidate();
        // 旧/新范围逐项通知；远距离滚动不能把中间整篇范围合成一个 invalidation。
        let mut ranges: Vec<_> = projection
            .rows
            .iter()
            .map(|row| row.source_utf16_range.clone())
            .collect();
        if let Some(old) = previous.as_ref()
            && old.document_generation == generation
            && std::rc::Weak::ptr_eq(&old.document_identity, identity)
        {
            // 已离开视口的旧高度恢复默认估计，也必须撤销其 native 属性缓存。
            ranges.extend(old.rows.iter().map(|row| row.source_utf16_range.clone()));
        }
        ranges.sort_unstable_by_key(|range| range.start);
        ranges.dedup();
        drop(obsolete);
        for range in ranges {
            // RefCell 与 canonical 借用均已结束；沿用已有 clamped 属性通知原语。
            self.notify_attribute_range(Some(range));
            if !self.ivars().accepts_edits.get()
                || self.ivars().table_geometry_epoch.get() != epoch
                || document.borrow().generation() != generation
            {
                // 任一原生通知重入修改后停止，调用方不得使用本批旧 geometry。
                return None;
            }
        }
        Some(true)
    }

    pub(super) fn set_presentation_mode(&self, mode: MarkdownPresentationMode) {
        if let Some(folds) = self.ivars().fold_state.borrow().upgrade() {
            folds.set_mode(mode);
        }
        if self.ivars().presentation_mode.replace(mode) == mode {
            // Reselecting the same native mode preserves the current bounded syntax cache.
            return;
        }
        self.revoke_presentation_semantics();
        // 模式切换撤销整批表格占位高度；原有模式失效通知负责后续 native 重取属性。
        self.advance_table_geometry_epoch();
        let previous_table_rows = self.ivars().table_row_heights.borrow_mut().take();
        drop(previous_table_rows);
        // Loaned 的旧模式计算也必须失效，不能在 native 返回后恢复原 owner。
        self.ivars().font_run_scratch.invalidate();
        self.ivars().font_selection_cache.invalidate();
        self.ivars().attribute_cache.invalidate();
        // The semantic parser stays in its complete syntax mode; only the native projection changes.
        self.ivars()
            .presentation
            .borrow_mut()
            .invalidate_document_generation();
        // Native可保留离屏paragraph属性；一次全范围metadata通知撤销旧模式，不能只通知最后cache。
        // 这里不复制/扫描正文或ensure布局；Foundation内部实际工作仍须由非默认诊断验收。
        let length = self
            .document()
            .map_or(0, |document| document.borrow().utf16_len());
        self.notify_or_defer_attribute_range(Some(0..length));
    }

    /// Reads the current derived presentation mode without allocating native text.
    pub(super) fn presentation_mode(&self) -> MarkdownPresentationMode {
        self.ivars().presentation_mode.get()
    }
    /// Prepares Markdown semantics for one explicit bounded viewport.
    ///
    /// Table layout runs in a main-queue task after drawing. It must not indirectly invoke
    /// TextKit attribute fixing through attributes_at_index and reenter drawRect: during
    /// layout. This method materializes only its UTF-16 window into the shared cache.
    pub(super) fn table_presentation_snapshot(
        &self,
        requested_utf16_window: std::ops::Range<usize>,
    ) -> Option<NativeMarkdownTablePresentationSnapshot> {
        if self.presentation_mode() != MarkdownPresentationMode::InlinePreview
            || self.ivars().composition_active.get()
        {
            // Source mode and provisional IME input expose source text rather than table overlays.
            return None;
        }
        let document = self.document()?;
        let semantic_epoch = self.ivars().presentation_semantic_epoch.get()?;
        let (generation, document_length, selection, requested_utf16_window) = {
            let document_ref = document.borrow();
            let document_length = document_ref.utf16_len();
            if document_length == 0 {
                // Empty canonical text cannot contribute a semantic table or a native layout range.
                return None;
            }
            let start = requested_utf16_window.start.min(document_length);
            let end = requested_utf16_window.end.min(document_length).max(start);
            // 原源读取错误使本次表格准备保持未知；不能继续用未对齐或EOF替代范围。
            let start = markdown_utf16_boundary_down(&document_ref, start).ok()?;
            let end = markdown_utf16_boundary_up(&document_ref, end)
                .ok()?
                .max(start);
            (
                document_ref.generation(),
                document_length,
                document_ref.selection(),
                start..end.min(document_length),
            )
        };
        if document_length == 0 {
            // No canonical code unit remains to anchor a presentation query.
            return None;
        }

        if let Some(proof) = self.source_no_tables_proof(&requested_utf16_window) {
            return Some(NativeMarkdownTablePresentationSnapshot {
                document_generation: generation,
                utf16_window: requested_utf16_window,
                identity: Rc::downgrade(&document),
                document_length,
                selection,
                semantic_epoch,
                semantics: None,
                verified_no_tables: Some(proof),
            }); // 只省去本窗口已证明无表时的legacy物化；Find/Mode仍沿原无表几何流程。
        }

        let needs_rebuild = {
            let presentation = self.ivars().presentation.borrow();
            let probe = requested_utf16_window
                .start
                .min(document_length.saturating_sub(1));
            presentation.needs_rebuild(generation, probe)
                || (requested_utf16_window.end > requested_utf16_window.start
                    && presentation
                        .needs_rebuild(generation, requested_utf16_window.end.saturating_sub(1)))
        };
        if needs_rebuild {
            // Rebuild only when generation or either requested endpoint escapes the current bounded window.
            let rebuild_result = {
                let document_ref = document.borrow();
                document_ref.materialize_markdown_viewport(
                    requested_utf16_window.clone(),
                    MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS,
                )
            };
            let mut presentation = self.ivars().presentation.borrow_mut();
            match rebuild_result {
                // A successfully copied window still distinguishes complete syntax from bounded source fallback.
                Ok(slice) => match slice.semantic_status {
                    MarkdownViewportSemanticStatus::Complete => {
                        // Complete bounded semantics can populate the viewport cache safely.
                        let _ = presentation.rebuild(generation, slice.utf16_range, &slice.text);
                    }
                    MarkdownViewportSemanticStatus::SourceFallback => {
                        // Incomplete bounded syntax stays readable as source without a whole-document fallback.
                        let _ = presentation
                            .record_source_fallback_window(generation, slice.utf16_range);
                    }
                },
                Err(_) => {
                    // Cache a stable bounded source-mode fallback after materialization failure. Repeated
                    // draws must neither reschedule the same failed request nor parse the full document.
                    let _ = presentation
                        .record_source_fallback_window(generation, requested_utf16_window.clone());
                }
            }
        }

        let presentation = self.ivars().presentation.borrow();
        let utf16_window = presentation.cached_utf16_window()?;
        Some(NativeMarkdownTablePresentationSnapshot {
            document_generation: generation,
            utf16_window,
            identity: Rc::downgrade(&document),
            document_length,
            selection,
            semantic_epoch,
            semantics: presentation.semantic_snapshot(),
            verified_no_tables: None,
        })
    }

    /// Copies one bounded exact cell range for explicit single-cell editing.
    pub(super) fn table_cell_source(&self, range: &std::ops::Range<usize>) -> Option<String> {
        let document = self.document()?;
        let document_ref = document.borrow();
        let slice = document_ref.materialize_viewport(range.clone(), 0).ok()?;
        (slice.utf16_range == *range).then_some(slice.text)
    }

    /// 同一次有限显示投影同时交付不可变属性串和源run；绘制/命中不能各自重读或重算单格。
    pub(super) fn table_cell_attributed_text(
        &self,
        snapshot: &NativeMarkdownTablePresentationSnapshot,
        table_index: usize,
        cell_index: usize,
    ) -> Option<(
        Retained<NSAttributedString>,
        Vec<NativeMarkdownTableCellSourceSpan>,
    )> {
        // 原生通知/测量可能重入关闭；栈内载荷租约结束前预检必须拒绝。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

        if !snapshot.is_current(self) {
            // 原快照失去源/选择权限后，不采当前 generation 给旧 cell 补票。
            return None;
        }
        let render = self.render_payload()?;
        let request = AttributeRequest {
            identity: snapshot.identity.clone(),
            generation: snapshot.document_generation,
            mode: MarkdownPresentationMode::InlinePreview,
            // 字体 scratch 仍服从当前几何；原源语义权限由 snapshot 独立在前后核验。
            semantic_epoch: self.ivars().font_run_scratch.epoch(),
            table_geometry_epoch: self.ivars().table_geometry_epoch.get(),
            table_row_height: None,
            list_role: MarkdownListGlyphRole::Literal,
        };
        if !self.attribute_request_is_current(&request) {
            // Source/关闭空壳不进入可见表格字体构造。
            return None;
        }
        let display = self.table_cell_display_text(snapshot, table_index, cell_index)?;
        let string = NSString::from_str(&display.text);
        // SAFETY: 本模块只生成固定类型的属性字典；可变属性串限于原有限可见单格，不缓存整篇正文。
        let attributed = unsafe {
            NSMutableAttributedString::initWithString_attributes(
                NSMutableAttributedString::alloc(),
                &string,
                Some(&render.base_attributes),
            )
        };
        for span in display.style_spans {
            // 样式区间只指向同一显示串，不能把隐藏语法的源范围当作Native属性范围。
            if span.utf16_range.is_empty() {
                // 没有实际显示单元就不调用属性setter。
                continue;
            }
            let font = native_markdown_base_font(span.styles, &render.palette);
            // 复用原编辑器有限LRU的完整不可变样式字典，不为命中复制另一套字体或布局。
            let attributes =
                self.cached_attribute_dictionary(span.styles, &font, &render, &request)?;
            // SAFETY: 此区间来自同一显示串的完整UTF16标量追加，始终位于当前属性串长度内。
            unsafe {
                attributed.addAttributes_range(
                    &attributes,
                    NSRange::new(
                        span.utf16_range.start,
                        span.utf16_range.end - span.utf16_range.start,
                    ),
                );
            }
        }
        // addAttributes 也可能重入；只向当前原始 owner 返回已完成的候选。
        (self.attribute_request_is_current(&request) && snapshot.is_current(self))
            .then_some((attributed.into_super(), display.source_spans))
    }

    /// Resolves visible cell text and style spans while keeping original Markdown source authoritative.
    pub(super) fn table_cell_display_text(
        &self,
        snapshot: &NativeMarkdownTablePresentationSnapshot,
        table_index: usize,
        cell_index: usize,
    ) -> Option<NativeMarkdownTableCellDisplayText> {
        if !snapshot.is_current(self) {
            // 原快照失权时不读取正文，也不从当前可变缓存重建后继权限。
            return None;
        }
        // cell 身份从原不可变目录 O(1) 取得；不收裸 range，也不为每一可见格遍历整表。
        let range = &snapshot
            .tables()
            .get(table_index)?
            .cells
            .get(cell_index)?
            .utf16_range;
        let source = self.table_cell_source(range)?;
        let mut missing_semantics = false;
        let display = native_markdown_table_cell_display_text_from_source(
            &source,
            range.start,
            |absolute_utf16| {
                match snapshot.query(absolute_utf16) {
                    Some(query) => query.styles, // 同一原始目录中的精确 run 或已证明默认 gap。
                    None => {
                        // 转换器先保持有界纯值；缺失证明使整格失败，绝不发布半份默认样式。
                        missing_semantics = true;
                        MarkdownPresentationStyles::default()
                    }
                }
            },
        );
        (!missing_semantics && snapshot.is_current(self)).then_some(display)
    }

    /// Defers semantic style notifications while the native input method owns provisional text.
    pub(super) fn begin_composition(&self) {
        // 输入法取得正文所有权即撤旧语义，随后取消 marked text 也不能恢复旧快照身份。
        self.revoke_presentation_semantics();
        if let Some(folds) = self.ivars().fold_state.borrow().upgrade() {
            folds.set_composition(true);
        }
        self.ivars().composition_active.set(true);
    }

    /// Ends provisional composition and publishes or defers its coalesced style invalidation.
    pub(super) fn finish_composition(&self) {
        if let Some(folds) = self.ivars().fold_state.borrow().upgrade() {
            folds.set_composition(false);
        }
        if !self.ivars().composition_active.replace(false) {
            // Repeated unmark/close callbacks cannot publish the same deferred style invalidation twice.
            return;
        }
        let invalidated = self
            .ivars()
            .deferred_presentation_invalidation
            .borrow_mut()
            .take();
        self.notify_or_defer_attribute_range(invalidated);
    }

    /// Starts a balanced native-input scope without retaining a second document or native graph.
    pub(super) fn begin_native_input(&self) {
        self.ivars()
            .native_input_depth
            .set(self.ivars().native_input_depth.get().saturating_add(1));
    }

    /// Publishes coalesced derived attributes only after NSTextView closes its outer edit transaction.
    pub(super) fn end_native_input(&self) {
        let depth = self.ivars().native_input_depth.get().saturating_sub(1);
        self.ivars().native_input_depth.set(depth);
        if depth == 0 && !self.ivars().composition_active.get() {
            // Character edits are already committed; deferred style edits can no longer invalidate
            // element caches while NSTextView's insertion code is scrolling its caret to visible.
            let invalidated = self
                .ivars()
                .deferred_presentation_invalidation
                .borrow_mut()
                .take();
            self.notify_attribute_range(invalidated);
        }
    }

    /// 输入法组合期间仅发布健康原源中已证明的当前标量范围。
    pub(super) fn composition_base_attribute_range(&self, index: usize) -> NSRange {
        let Some(document_owner) = self.document() else {
            // 已真实断开的live适配器没有正文；不重建或保活替代文档。
            return NSRange::new(0, 0);
        };
        let document = document_owner.borrow();
        let document_length = document.utf16_len();
        let range = if index >= document_length {
            // EOF无标量，仍核健康；空范围不会分配正文窗口。
            document
                .copy_utf16_code_units(document_length, &mut [])
                .map(|()| NSRange::new(document_length, 0))
        } else {
            markdown_utf16_boundary_down(&document, index).and_then(|start| {
                let end = markdown_utf16_boundary_up(&document, index.saturating_add(1))?;
                Ok(NSRange::new(start, end - start))
            })
        };
        drop(document);
        drop(document_owner); // 外来异常前既无RefCell借用，也无本栈活动文档强租约。
        markdown_attribute_read_or_throw(range)
    }

    /// Coalesces style-range metadata until both composition and native input transactions permit notification.
    pub(super) fn notify_or_defer_attribute_range(
        &self,
        invalidated: Option<std::ops::Range<usize>>,
    ) {
        if !self.ivars().composition_active.get() && self.ivars().native_input_depth.get() == 0 {
            // Outside native input and composition, style notifications can be delivered synchronously.
            self.notify_attribute_range(invalidated);
            return;
        }
        let Some(invalidated) = invalidated else {
            // A cache that was never populated contributes no stale style interval to defer.
            return;
        };
        // Merge only range metadata while native editing owns element-cache consistency.
        let mut deferred = self.ivars().deferred_presentation_invalidation.borrow_mut();
        *deferred = Some(match deferred.take() {
            // Multiple edits coalesce to one interval spanning every previously invalidated style.
            Some(previous) => {
                previous.start.min(invalidated.start)..previous.end.max(invalidated.end)
            }
            // The first invalidation needs only its own bounded coordinates.
            None => invalidated,
        });
    }

    /// Clamps stale derived ranges to the current document before notifying native attribute consumers.
    pub(super) fn notify_attribute_range(&self, invalidated: Option<std::ops::Range<usize>>) {
        // 原生通知/测量可能重入关闭；栈内载荷租约结束前预检必须拒绝。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

        let Some(range) = invalidated else {
            // An unpopulated presentation cache has no attribute notification to publish.
            return;
        };
        // Presentation caches are deliberately allowed to describe the window
        // that was visible before an edit.  A deletion can therefore shrink the
        // piece tree before this notification is delivered.  NSTextStorage
        // validates attribute ranges against its *current* NSString length;
        // forwarding the stale end would raise NSRangeException (for example
        // old `0..8259` after the new document is `8258` units long).
        let Some(document) = self.document() else {
            return;
        };
        let document_length = document.borrow().utf16_len();
        let start = range.start.min(document_length);
        let end = range.end.min(document_length).max(start);
        let length = end.saturating_sub(start);
        if length == 0 {
            // Deletion may remove the whole stale range; Foundation must not receive a phantom edit.
            return;
        }
        self.edited_range_changeInLength(
            NSTextStorageEditActions::EditedAttributes,
            NSRange::new(start, length),
            0,
        );
    }
}

/// 返回包含原查询单元的有限标量对齐窗口；任何源错误原样失败，不扫到0或EOF补范围。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn markdown_attribute_request_window(
    document: &MarkdownDocumentBuffer,
    index: usize,
) -> Result<(std::ops::Range<usize>, usize), MarkdownDocumentBufferError> {
    let document_length = document.utf16_len();
    // 空文档与纯EOF查询同样核原Source健康，空读取不创建第二个正文分配。
    document.copy_utf16_code_units(document_length, &mut [])?;
    if document_length == 0 {
        // 空文档没有包含标量，真实健康检查完成后返回唯一空窗口。
        return Ok((0..0, 0));
    }

    let probe = index.min(document_length.saturating_sub(1));
    let maximum_window = NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS;

    // TextKit可查询emoji低代理项；返回窗口仍必须包含其完整标量，不能把半个字符作为属性边界。
    let probe_start = markdown_utf16_boundary_down(document, probe)?;
    let probe_end =
        markdown_utf16_boundary_up(document, probe.saturating_add(1).min(document_length))?
            .max(probe_start.saturating_add(1))
            .min(document_length);
    let probe_length = probe_end.saturating_sub(probe_start);
    if probe_length >= maximum_window {
        // 查询标量必须完整保留；当前固定4096预算远大于真实标量最多两个单元。
        return Ok((probe_start..probe_end, probe));
    }

    let context_budget = maximum_window - probe_length;
    let left_budget = context_budget / 2;
    let right_budget = context_budget - left_budget;
    let mut scalar_start =
        markdown_utf16_boundary_down(document, probe_start.saturating_sub(left_budget))?;
    let mut scalar_end = markdown_utf16_boundary_up(
        document,
        probe_end.saturating_add(right_budget).min(document_length),
    )?
    .min(document_length);

    // 两边对齐可能额外增加一个代理单元；重新裁剪仍须包含原标量，并严格保持4096硬界。
    if scalar_end.saturating_sub(scalar_start) > maximum_window {
        let bounded_start =
            markdown_utf16_boundary_up(document, scalar_end.saturating_sub(maximum_window))?;
        if bounded_start <= probe_start {
            // 可保留原查询标量时只收紧左边，不额外扩大另一侧。
            scalar_start = bounded_start;
        } else {
            // 左侧不能收紧时裁剪右侧，原标量和真实左锚点仍必须完整保留。
            scalar_end = markdown_utf16_boundary_down(
                document,
                scalar_start.saturating_add(maximum_window),
            )?
            .max(probe_end)
            .min(document_length);
        }
    }

    // 有效UTF8的查询标量最多两个单元；防御分支也只使用上面已证明的完整原标量。
    if scalar_start >= scalar_end {
        // 上下文交集为空时只保留已经验证的查询标量，不猜源范围。
        Ok((probe_start..probe_end, probe))
    } else {
        // 正常路径交付原查询两侧的有限真实上下文。
        Ok((scalar_start..scalar_end, probe))
    }
}

/// 将低代理项位置移至前一真实标量边界；一次读取即可判定，不执行字节坐标转换。
pub(super) fn markdown_utf16_boundary_down(
    document: &MarkdownDocumentBuffer,
    offset: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    markdown_utf16_boundary(document, offset, false)
}

/// 将低代理项位置移至后一真实标量边界；错误立即传播，不循环搜索替代范围。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn markdown_utf16_boundary_up(
    document: &MarkdownDocumentBuffer,
    offset: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    markdown_utf16_boundary(document, offset, true)
}

fn markdown_utf16_boundary(
    document: &MarkdownDocumentBuffer,
    offset: usize,
    forward: bool,
) -> Result<usize, MarkdownDocumentBufferError> {
    markdown_utf16_boundary_by(document.utf16_len(), offset, forward, |position| {
        if position == 0 || position == document.utf16_len() {
            // 聚合已证明首尾边界；仍核真实Source健康，不为零正文读取触发页分配。
            document.copy_utf16_code_units(position, &mut [])?;
            Ok(None)
        } else {
            document.utf16_code_unit_at(position).map(Some)
        }
    })
}

fn markdown_utf16_boundary_by(
    length: usize,
    offset: usize,
    forward: bool,
    mut read: impl FnMut(usize) -> Result<Option<u16>, MarkdownDocumentBufferError>,
) -> Result<usize, MarkdownDocumentBufferError> {
    if offset > length {
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument); // 不把非法请求饱和为EOF。
    }
    let unit = read(offset)?; // 源错误优先返回，不能当作“可能是代理项”继续扫描。
    if offset == 0 || offset == length {
        return Ok(offset); // 首尾仍已通过上面同一真实读取健康门。
    }
    match unit {
        Some(0xdc00..=0xdfff) if forward => Ok(offset + 1),
        Some(0xdc00..=0xdfff) => Ok(offset - 1),
        Some(_) => Ok(offset), // canonical来自有效UTF8，只有低代理项可能位于标量内部。
        None => Err(MarkdownDocumentBufferError::RangeOutsideDocument), // 中间缺单元不能假称已对齐。
    }
}

/// 仅在全部Rust读取claims已结束后把真实错误转为公开异常；成功值保持原样。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn markdown_attribute_read_or_throw<T>(read: Result<T, MarkdownDocumentBufferError>) -> T {
    match read {
        Ok(value) => value,
        // 原错误仍由Source永久保留；异常只中止这次无法认证的Foundation属性请求。
        Err(_) => throw_markdown_storage_unavailable(),
    }
}

/// Foundation原语不能携带Rust错误；中止原读取，不制造有效属性range或隐藏sticky源错误。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn throw_markdown_storage_unavailable() -> ! {
    throw_markdown_native_exception(
        ns_string!("NSInternalInconsistencyException"),
        ns_string!("原正文读取不可用，无法发布未经验证的文本属性"),
    )
}

/// 唯一公开异常工厂只接固定元数据；调用方须先结束Doc/RefCell借用和业务payload guard。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn throw_markdown_native_exception(name: &NSString, reason: &NSString) -> ! {
    let class = objc2::runtime::AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: 公开NSException工厂接固定名称/原因和nil userInfo；ABI与Foundation声明一致。
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class, exceptionWithName: name, reason: reason, userInfo: None::<&objc2::runtime::AnyObject>]
    };
    // objc2官方throw使用C-unwind；不以Rust panic越过ABI，也不尝试清除原Source的永久错误。
    objc2::exception::throw(exception)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One decoded scalar and its local half-open UTF-16 interval in a bounded scratch window.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct NativeMarkdownUtf16Scalar {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) value: char,
}

/// Decodes the complete scalar containing a bounded UTF-16 code-unit index.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_utf16_scalar_at(
    code_units: &[unichar],
    index: usize,
) -> Option<NativeMarkdownUtf16Scalar> {
    if code_units.is_empty() || index >= code_units.len() {
        // The caller cannot derive a scalar from an empty or out-of-window native query.
        return None;
    }

    let mut start = index;
    if is_native_markdown_low_surrogate(code_units[start])
        && start > 0
        && is_native_markdown_high_surrogate(code_units[start - 1])
    {
        // Low-surrogate queries refer to the same complete scalar as their preceding high unit.
        start -= 1;
    }
    let first = code_units[start];
    if is_native_markdown_high_surrogate(first)
        && start + 1 < code_units.len()
        && is_native_markdown_low_surrogate(code_units[start + 1])
    {
        // Decode a complete pair as one scalar so fallback ranges cannot split an emoji.
        let high = u32::from(first) - 0xd800;
        let low = u32::from(code_units[start + 1]) - 0xdc00;
        return Some(NativeMarkdownUtf16Scalar {
            start,
            end: start + 2,
            value: char::from_u32(0x1_0000 + (high << 10) + low)
                .unwrap_or(char::REPLACEMENT_CHARACTER),
        });
    }

    // BMP input occupies one unit; isolated malformed surrogates use the replacement character.
    Some(NativeMarkdownUtf16Scalar {
        start,
        end: start + 1,
        value: char::from_u32(u32::from(first)).unwrap_or(char::REPLACEMENT_CHARACTER),
    })
}

/// Identify the leading half of a supplementary Unicode scalar in Foundation UTF-16 storage.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
const fn is_native_markdown_high_surrogate(code_unit: unichar) -> bool {
    code_unit >= 0xd800 && code_unit <= 0xdbff
}

/// Identify the trailing half before extending an attribute range across its paired leading unit.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
const fn is_native_markdown_low_surrogate(code_unit: unichar) -> bool {
    code_unit >= 0xdc00 && code_unit <= 0xdfff
}

/// Finds adjacent scalar-safe text sharing the resolved fallback-font policy.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_font_run_range(
    code_units: &[unichar],
    global_start: usize,
    query_index: usize,
    base_coverage: &NSCharacterSet,
    fallback_coverage: Option<&NSCharacterSet>,
) -> std::ops::Range<usize> {
    native_markdown_font_run_range_by(code_units, global_start, query_index, |scalar| {
        let base_covers = native_markdown_character_set_covers(base_coverage, scalar);
        fallback_coverage.map_or(base_covers, |fallback| {
            // A fallback run stops where the base face can draw or the selected fallback cannot.
            !base_covers && native_markdown_character_set_covers(fallback, scalar)
        })
    })
}

/// Expands a bounded font run through neighboring scalars accepted by the supplied face predicate.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn native_markdown_font_run_range_by(
    code_units: &[unichar],
    global_start: usize,
    query_index: usize,
    mut is_compatible: impl FnMut(char) -> bool,
) -> std::ops::Range<usize> {
    let Some(query) = native_markdown_utf16_scalar_at(code_units, query_index) else {
        // No scalar means no valid native attribute interval to extend.
        return global_start..global_start;
    };
    let mut run_start = query.start;
    let mut run_end = query.end;

    while run_start > 0 {
        // Extend left through adjacent complete scalars only while the same face policy applies.
        let Some(previous) = native_markdown_utf16_scalar_at(code_units, run_start - 1) else {
            // A missing predecessor ends the bounded run without scanning outside scratch.
            break;
        };
        if previous.end != run_start || !is_compatible(previous.value) {
            // A gap or different font requirement is the exact left boundary of this run.
            break;
        }
        run_start = previous.start;
    }

    while run_end < code_units.len() {
        // Extend right using the same scalar/coverage rule and the fixed scratch endpoint.
        let Some(next) = native_markdown_utf16_scalar_at(code_units, run_end) else {
            // An undecodable successor cannot extend a safe effective range.
            break;
        };
        if next.start != run_end || !is_compatible(next.value) {
            // A nonadjacent or differently covered scalar begins the next native font run.
            break;
        }
        run_end = next.end;
    }

    global_start.saturating_add(run_start)..global_start.saturating_add(run_end)
}

/// Intersects a candidate attribute run with its bounded fallback while preserving the queried index.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn clamp_markdown_attribute_range(
    range: std::ops::Range<usize>,
    fallback_range: std::ops::Range<usize>,
    index: usize,
    document_length: usize,
) -> NSRange {
    let fallback_start = fallback_range.start.min(document_length);
    let fallback_end = fallback_range.end.min(document_length).max(fallback_start);
    let start = range.start.max(fallback_start).min(fallback_end);
    let end = range.end.min(fallback_end).max(start);
    if start < end && start <= index && index < end {
        // The exact semantic/font intersection is valid and still contains the queried unit.
        NSRange::new(start, end - start)
    } else if fallback_start < fallback_end {
        // If the semantic run missed the query, use its already scalar-aligned bounded fallback.
        NSRange::new(fallback_start, fallback_end - fallback_start)
    } else if index < document_length {
        // The caller has already aligned the fallback window to scalar
        // boundaries. This branch is only a defensive zero-length case.
        NSRange::new(index.min(document_length.saturating_sub(1)), 1)
    } else {
        // Native EOF queries expose an empty interval, never a fabricated trailing code unit.
        NSRange::new(document_length, 0)
    }
}

#[cfg(test)]
mod native_markdown_table_cell_source_tests {
    use super::*;

    #[test]
    fn hidden_markers_and_escaped_pipe_keep_absolute_unicode_source_runs() {
        let display = native_markdown_table_cell_display_text_from_source(
            " **中😀** \\| 乙 ",
            40,
            |absolute| match absolute - 40 {
                1 | 2 | 6 | 7 => MarkdownPresentationStyles::HIDDEN_MARKER,
                3..=5 => MarkdownPresentationStyles::STRONG,
                _ => MarkdownPresentationStyles::default(),
            },
        );
        assert_eq!(display.text, " 中😀 | 乙 ");
        assert_eq!(
            display.source_spans,
            [
                NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 0..1,
                    source_utf16_range: 40..41,
                },
                NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 1..4,
                    source_utf16_range: 43..46,
                },
                NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 4..5,
                    source_utf16_range: 48..49,
                },
                NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 5..9,
                    source_utf16_range: 50..54,
                },
            ]
        ); // 中与emoji仍一一对应；移除粗体标记和转义slash后，每个真实源gap都独立保留。
        assert!(display.style_spans.iter().any(|span| {
            span.utf16_range == (1..4) && span.styles.contains(MarkdownPresentationStyles::STRONG)
        }));
    }

    #[test]
    fn ordinary_backslash_pairs_map_the_visible_second_scalar_in_each_pair() {
        let display =
            native_markdown_table_cell_display_text_from_source(r"\\| \\\| \中", 7, |_| {
                MarkdownPresentationStyles::default()
            });
        assert_eq!(display.text, r"\| \| \中");
        assert_eq!(
            display.source_spans,
            [
                NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 0..3,
                    source_utf16_range: 8..11,
                },
                NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 3..4,
                    source_utf16_range: 12..13,
                },
                NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 4..8,
                    source_utf16_range: 14..18,
                },
            ]
        ); // 偶数对slash隐藏首个，奇数末slash隐藏自身；非标点前的slash与中文完整保留。
    }

    #[test]
    fn inline_code_hides_only_the_table_escape_and_keeps_visible_eof_before_delimiter() {
        let display =
            native_markdown_table_cell_display_text_from_source(r"`a\\\|b`", 100, |absolute| {
                match absolute - 100 {
                    0 | 7 => MarkdownPresentationStyles::HIDDEN_MARKER,
                    _ => MarkdownPresentationStyles::INLINE_CODE,
                }
            });
        assert_eq!(display.text, r"a\\|b");
        assert_eq!(
            display.source_spans,
            [
                NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 0..3,
                    source_utf16_range: 101..104,
                },
                NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 3..5,
                    source_utf16_range: 105..107,
                },
            ]
        ); // display 0对应首个可见a；EOF对应b后的原位置，不能跳过隐藏的末尾code标记。
    }

    #[test]
    fn empty_hidden_and_whitespace_cells_do_not_invent_visible_source_units() {
        for source in ["", "**"] {
            let display = native_markdown_table_cell_display_text_from_source(source, 120, |_| {
                MarkdownPresentationStyles::HIDDEN_MARKER
            });
            assert!(display.text.is_empty());
            assert!(display.source_spans.is_empty()); // 空/全隐藏格交原cell边界决定caret，不造零长假run。
        }
        for (source, start, units) in [("中😀 ", 0, 4), (" \t ", 37, 3)] {
            let display =
                native_markdown_table_cell_display_text_from_source(source, start, |_| {
                    MarkdownPresentationStyles::default()
                });
            assert_eq!(display.text, source);
            assert_eq!(
                display.source_spans,
                [NativeMarkdownTableCellSourceSpan {
                    display_utf16_range: 0..units,
                    source_utf16_range: start..start + units,
                }]
            ); // UTF16零点、emoji中间两单元、尾空格和真实EOF保持同一线性域。
        }
    }
}

#[cfg(test)]
mod native_markdown_font_run_tests {
    use super::{native_markdown_font_run_range_by, native_markdown_utf16_scalar_at};

    #[test]
    /// Resolve either half of a supplementary scalar to the same complete native UTF-16 character range.
    pub(super) fn utf16_scalar_lookup_keeps_a_supplementary_character_whole() {
        let source: Vec<u16> = "A😀B".encode_utf16().collect();

        let high = native_markdown_utf16_scalar_at(&source, 1).expect("emoji high surrogate");
        let low = native_markdown_utf16_scalar_at(&source, 2).expect("emoji low surrogate");

        assert_eq!(high, low);
        assert_eq!(high.start..high.end, 1..3);
        assert_eq!(high.value, '😀');
    }

    #[test]
    /// Stop fallback runs at adjacent ASCII boundaries while preserving their absolute canonical offsets.
    pub(super) fn fallback_font_run_contains_only_adjacent_non_ascii_scalars() {
        let source: Vec<u16> = "A中文B".encode_utf16().collect();
        let run = native_markdown_font_run_range_by(&source, 40, 1, |scalar| !scalar.is_ascii());

        assert_eq!(run, 41..43);
    }

    #[test]
    /// Expand fallback text across complete emoji scalars even when the requested index is a low surrogate.
    pub(super) fn fallback_font_run_does_not_split_emoji_surrogate_pairs() {
        let source: Vec<u16> = "A😀😃B".encode_utf16().collect();
        let run = native_markdown_font_run_range_by(&source, 7, 2, |scalar| !scalar.is_ascii());

        assert_eq!(run, 8..12);
    }
}

#[cfg(test)]
mod native_markdown_attribute_window_tests {
    use std::cell::Cell;

    use super::*;

    fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        // 只在自有目录创建有限合成UTF8输入；不初始化AppKit或读取用户笔记。
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("attribute-window.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        (directory, document)
    }

    fn is_boundary(units: &[u16], offset: usize) -> bool {
        offset == units.len() || !(0xdc00..=0xdfff).contains(&units[offset])
    }

    #[test]
    fn cached_unit_alignment_matches_original_scalar_coordinates_across_window_edges() {
        let text = format!("{}😀{}😀尾", "a".repeat(4095), "中".repeat(1400));
        let (_directory, document) = fixture(&text);
        let units: Vec<u16> = text.encode_utf16().collect();
        for index in [
            0,
            1,
            2047,
            2048,
            4094,
            4095,
            4096,
            4097,
            units.len() - 2,
            units.len() - 1,
            units.len(),
            units.len() + 100,
        ] {
            let (window, probe) = markdown_attribute_request_window(&document, index).unwrap();
            assert_eq!(probe, index.min(units.len() - 1));
            assert!(window.contains(&probe));
            assert!(window.len() <= NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS);
            assert!(is_boundary(&units, window.start));
            assert!(is_boundary(&units, window.end));
        }
        for offset in [0, 4095, 4096, 4097, units.len() - 2, units.len()] {
            let expected_down = if is_boundary(&units, offset) {
                offset
            } else {
                offset - 1
            };
            let expected_up = if is_boundary(&units, offset) {
                offset
            } else {
                offset + 1
            };
            assert_eq!(
                markdown_utf16_boundary_down(&document, offset),
                Ok(expected_down)
            );
            assert_eq!(
                markdown_utf16_boundary_up(&document, offset),
                Ok(expected_up)
            );
        }
    }

    #[test]
    fn supplementary_only_windows_never_exceed_the_real_4096_unit_budget() {
        let (_directory, document) = fixture(&"😀".repeat(3000));
        for index in (0..document.utf16_len()).step_by(127) {
            // 奇偶位置分别命中高/低代理项；左右对齐不能把窗口扩大为4097或4098单元。
            let (window, probe) = markdown_attribute_request_window(&document, index).unwrap();
            assert!(window.len() <= 4096);
            assert_eq!(window.start % 2, 0);
            assert_eq!(window.end % 2, 0);
            assert!(window.contains(&probe));
        }
    }

    #[test]
    fn source_failure_is_returned_after_one_attempt_without_scanning_to_an_endpoint() {
        let reads = Cell::new(0);
        let failure = MarkdownDocumentBufferError::OriginalBackingUnavailable(5);
        for forward in [false, true] {
            reads.set(0);
            let result = markdown_utf16_boundary_by(1_000_000, 500_000, forward, |_| {
                reads.set(reads.get() + 1);
                Err(failure)
            });
            assert_eq!(result, Err(failure));
            assert_eq!(reads.get(), 1); // 失败不是代理项证据，不能继续尝试499999等坐标。
        }
    }

    #[test]
    fn endpoints_use_the_health_callback_and_invalid_offsets_never_request_a_unit() {
        let failure = MarkdownDocumentBufferError::OriginalBackingUnavailable(5);
        for offset in [0, 100] {
            let calls = Cell::new(0);
            assert_eq!(
                markdown_utf16_boundary_by(100, offset, false, |_| {
                    calls.set(calls.get() + 1);
                    Err(failure)
                }),
                Err(failure)
            );
            assert_eq!(calls.get(), 1);
        }
        assert_eq!(
            markdown_utf16_boundary_by(100, 101, true, |_| {
                panic!("越界请求不能读取单元")
            }),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        let (_directory, empty) = fixture("");
        assert_eq!(markdown_attribute_request_window(&empty, 0), Ok((0..0, 0)));
        assert_eq!(markdown_utf16_boundary_up(&empty, 0), Ok(0));
        assert_eq!(markdown_utf16_boundary_down(&empty, 0), Ok(0));
    }

    #[test]
    fn missing_middle_unit_is_rejected_instead_of_publishing_a_guessed_boundary() {
        assert_eq!(
            markdown_utf16_boundary_by(10, 5, false, |_| Ok(None)),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        assert_eq!(
            markdown_utf16_boundary_by(10, 5, false, |_| Ok(Some(0xdc00))),
            Ok(4)
        );
        assert_eq!(
            markdown_utf16_boundary_by(10, 5, true, |_| Ok(Some(0xdc00))),
            Ok(6)
        );
    }

    #[test]
    fn real_foundation_exception_precedes_the_effective_range_write_on_read_failure() {
        let sentinel = NSRange::new(777, 999);
        let output = Cell::new(sentinel);
        // sentinel仅是固定Copy值，异常后逐值核验；没有跨异常的源或RefCell借用。
        let result = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            let read = markdown_utf16_boundary_by(10, 5, false, |_| {
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(5))
            });
            // 这里使用生产相同错误适配器；只有返回成功值后Foundation primitive才写输出。
            let start = markdown_attribute_read_or_throw(read);
            output.set(NSRange::new(start, 1));
        }));
        let exception = result.unwrap_err().expect("必须是实际Objective-C异常");
        assert_eq!(output.get(), sentinel);
        // SAFETY: 上面唯一生产工厂返回真实NSException；公开name getter不读取用户正文。
        let name: Retained<NSString> = unsafe { msg_send![&exception, name] };
        assert_eq!(name.to_string(), "NSInternalInconsistencyException");
    }
}
