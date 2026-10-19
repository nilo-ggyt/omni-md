//! 诊断内容入口及有限元素读取；固定分块仅供归因，不代表软换行、双向文本或编辑功能验收。

use super::cache_owner::{CacheAdmission, CacheOwner, CachePayload};
use super::*;
use objc2::runtime::Bool;
use objc2_app_kit::{
    NSTextContentManagerEnumerationOptions, NSTextContentStorage, NSTextElement, NSTextLocation,
    NSTextParagraph,
};
use objc2_foundation::NSMutableString;
use std::ops::Range;

mod fragment_range;
mod virtual_paragraph;
pub(in crate::native_surface) use fragment_range::install_fragment_range_probe;

const CONTENT_PROBE_LOG_LIMIT: u8 = 24;
const CONTENT_PROBE_PHASE_LIMIT: u8 = 64;
const CONTENT_PROBE_UTF16_LIMIT: usize = 4096;
const CONTENT_PROBE_CACHE_LIMIT: usize = 8;
const CONTENT_PROBE_PHASES: &[&str] = &[
    "attributes.add",
    "attributes.get",
    "builder.alloc",
    "builder.init",
    "cache.retain",
    "cache.store_retain",
    "capture.document_range",
    "capture.end",
    "capture.length",
    "capture.manager_content",
    "capture.observer_storage",
    "capture.origin",
    "capture.primary_manager",
    "capture.weak_storage",
    "elements.array",
    "elements.end",
    "elements.start",
    "enumeration.forward_edge",
    "enumeration.original_block",
    "enumeration.reverse_edge",
    "frozen.alloc",
    "frozen.init",
    "global.location",
    "global.offset",
    "owned.include",
    "paragraph.alloc",
    "paragraph.init",
    "paragraph.set_manager",
    "paragraph.set_range",
    "paragraph.value",
    "paragraph.value_length",
    "range.alloc",
    "range.init",
    "read.get_characters",
    "read.length",
    "read.string",
    "string.alloc",
    "string.init",
    "verify.content_range",
    "verify.element_range",
    "verify.manager",
    "verify.range_end",
    "verify.range_start",
    "verify.represented",
    "verify.roundtrip_end",
    "verify.roundtrip_start",
    "verify.separator_range",
    "verify.transaction",
    "virtual.attr_alloc",
    "virtual.attr_attributes",
    "virtual.attr_copy",
    "virtual.attr_full_substring",
    "virtual.attr_init",
    "virtual.attr_slice_alloc",
    "virtual.attr_slice_init",
    "virtual.attr_string",
    "virtual.original_string",
    "virtual.owned_include",
    "virtual.paragraph_alloc",
    "virtual.paragraph_content",
    "virtual.paragraph_init",
    "virtual.paragraph_manager",
    "virtual.paragraph_range",
    "virtual.paragraph_separator",
    "virtual.paragraph_value",
    "virtual.range_alloc",
    "virtual.range_init",
    "virtual.slice_source_retain",
    "virtual.string_alloc",
    "virtual.string_bulk",
    "virtual.string_character",
    "virtual.string_copy",
    "virtual.string_full_substring",
    "virtual.string_init",
    "virtual.string_source_retain",
];

/// 只记录现有调用的固定阶段；不访问Native对象，也不读取或保留正文。
struct ProviderPhaseTrace {
    emitted: Cell<u8>,
    seen_phases: Cell<u128>,
    calls: Cell<u64>,
    last_phase: Cell<&'static str>,
    last_returned: Cell<bool>,
}

impl ProviderPhaseTrace {
    fn new() -> Self {
        Self {
            emitted: Cell::new(0),
            seen_phases: Cell::new(0),
            calls: Cell::new(0),
            last_phase: Cell::new("not_entered"),
            last_returned: Cell::new(true),
        }
    }

    fn call<T>(&self, phase: &'static str, operation: impl FnOnce() -> T) -> T {
        let previous_phase = self.last_phase.replace(phase);
        let previous_returned = self.last_returned.replace(false);
        let call = self.calls.get().saturating_add(1);
        self.calls.set(call);
        let emitted = self.emitted.get();
        let phase_bit = CONTENT_PROBE_PHASES
            .iter()
            .position(|candidate| *candidate == phase)
            .map_or(1u128 << 127, |index| 1u128 << index);
        if emitted < CONTENT_PROBE_PHASE_LIMIT && self.seen_phases.get() & phase_bit == 0 {
            self.emitted.set(emitted + 1);
            self.seen_phases.set(self.seen_phases.get() | phase_bit);
            // 每个固定阶段只输出首次入口，属性run不能刷掉后续setter/block；总预算仍最多64行。
            eprintln!(
                "content_storage_probe native_phase={phase} call={call} previous_phase={previous_phase} previous_returned={previous_returned} diagnostic_formal=false"
            );
        }
        let value = operation();
        // 只在原调用正常返回后置位；外来异常或Rust panic均保持false，不把展开误报为return。
        self.last_returned.set(true);
        value
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ContentKey {
    generation: u64,
    length: usize,
    selection: MarkdownTextSelection,
    mode: MarkdownPresentationMode,
    semantic_epoch: Option<u64>,
    table_epoch: u64,
    input_epoch: u64,
    input_depth: usize,
    composition: bool,
}

struct FiniteParagraph {
    key: ContentKey,
    source: Range<usize>,
    paragraph: Retained<NSTextParagraph>,
}

struct FiniteParagraphCache {
    entries: Vec<FiniteParagraph>,
}

impl CachePayload for FiniteParagraphCache {
    fn empty() -> Self {
        Self {
            entries: Vec::new(),
        } // 未使用的探针不分配源文本或元素容量。
    }
}

#[derive(Clone, Copy)]
#[repr(u16)]
enum ProviderFailure {
    Busy = 1,
    Source = 2,
    Stale = 4,
    Graph = 8,
    Location = 16,
    Range = 32,
    WholeRead = 64,
    Capacity = 128,
    Attributes = 256,
    Ledger = 512,
}

impl ProviderFailure {
    fn code(self) -> &'static str {
        match self {
            Self::Busy => "reentrant_provider", // 不以super完整枚举解除递归。
            Self::Source => "source_unavailable", // 关闭或原Weak失效。
            Self::Stale => "original_ticket_changed", // 原代次/模式/输入已撤权。
            Self::Graph => "storage_graph_changed", // provider不再消费同一piece storage。
            Self::Location => "global_location_failed", // 公开位置不能精确往返。
            Self::Range => "finite_source_range_failed", // 无法证明真实有限源边界。
            Self::WholeRead => "whole_read_guard", // 此原型绝不发出等于原文长度的bulk读取。
            Self::Capacity => "finite_cache_exhausted", // 不淘汰仍可能被weak fragment引用的元素。
            Self::Attributes => "finite_attributes_failed", // 字体/语义run未覆盖本次源位置。
            Self::Ledger => "owned_registration_failed", // 未登记对象不能发布给系统。
        }
    }
}

struct ProviderTicket {
    storage: Retained<PieceBackedMarkdownTextStorage>,
    document: WeakMarkdownDocument,
    key: ContentKey,
    trace: Rc<ProviderPhaseTrace>,
}

impl ProviderTicket {
    fn current(&self) -> bool {
        let ivars = self.storage.ivars();
        if !ivars.accepts_edits.get()
            || !std::rc::Weak::ptr_eq(&self.document, &ivars.document.borrow())
            || self.storage.presentation_mode() != self.key.mode
            || ivars.presentation_semantic_epoch.get() != self.key.semantic_epoch
            || ivars.table_geometry_epoch.get() != self.key.table_epoch
            || ivars.input_transaction_epoch.get() != self.key.input_epoch
            || ivars.native_input_depth.get() != self.key.input_depth
            || ivars.composition_active.get() != self.key.composition
        {
            return false; // 同一内容对象也不能给后继输入、模式或笔记续旧票。
        }
        self.document.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document.generation() == self.key.generation
                && document.utf16_len() == self.key.length
                && document.selection() == self.key.selection
        })
    }

    fn native<T>(
        &self,
        phase: &'static str,
        operation: impl FnOnce() -> T,
    ) -> Result<T, ProviderFailure> {
        if !self.current() {
            return Err(ProviderFailure::Stale); // 原生调用前先核原权限，不换成当前票。
        }
        let value = self.trace.call(phase, operation);
        if self.current() {
            Ok(value)
        } else {
            Err(ProviderFailure::Stale) // 返回值在本栈析构，不发布撤权后的对象。
        }
    }
}

/// 只保存真实公开range的固定标量；None绝不表示有效的0坐标。
#[derive(Debug)]
struct ParagraphRangeFacts {
    present: bool,
    start_present: bool,
    end_present: bool,
    start: Option<usize>,
    end: Option<usize>,
    roundtrip: bool,
}

struct ProviderGuard<'a>(&'a Cell<bool>);
impl Drop for ProviderGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false); // 每次同步入口结束即归还阶段，不安排重试或嵌套run loop。
    }
}

pub(super) struct ContentStorageProbeIvars {
    elements_calls: Cell<u64>,
    enumeration_calls: Cell<u64>,
    emitted: Cell<u8>,
    bounded: bool,
    virtual_paragraph: bool,
    storage: Weak<PieceBackedMarkdownTextStorage>,
    document: WeakMarkdownDocument,
    ledger: std::rc::Weak<NativeMarkdownOwnedLedger>,
    busy: Cell<bool>,
    failures: Cell<u16>,
    cache: CacheOwner<FiniteParagraphCache>,
    cached_entries: Cell<usize>,
    paragraph_fact_logs: Cell<u8>,
    mapping_observation_calls: Cell<u8>,
    trace: Rc<ProviderPhaseTrace>,
    fragment_probe: RefCell<Option<fragment_range::FragmentProbeOwner>>,
}

impl Drop for ContentStorageProbeIvars {
    fn drop(&mut self) {
        let fragment_owner = self.fragment_probe.get_mut().take();
        drop(fragment_owner); // provider析构先移出原槽，清理weak manager委托时不持RefCell借用。
        // 仅真实对象析构时报告固定计数；没有Native getter、正文、路径或延后重试。
        eprintln!(
            "content_storage_probe stage=drop elements_calls={} enumeration_calls={} bounded={} cached_entries={} last_native_phase={} last_native_returned={} native_calls={} diagnostic_formal=false",
            self.elements_calls.get(),
            self.enumeration_calls.get(),
            self.bounded,
            self.cached_entries.get(),
            self.trace.last_phase.get(),
            self.trace.last_returned.get(),
            self.trace.calls.get(),
        );
    }
}

define_class!(
    // SAFETY: 只收紧为主线程类；不覆写存储、编辑事务或位置映射，沿用标准父类契约。
    #[unsafe(super = NSTextContentStorage)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ContentStorageProbeIvars]
    pub(super) struct NativeMarkdownContentStorageProbe;

    // SAFETY: NSObject协议没有额外实现义务；此对象仍由原surface独占并要求实际释放。
    unsafe impl NSObjectProtocol for NativeMarkdownContentStorageProbe {}

    impl NativeMarkdownContentStorageProbe {
        #[unsafe(method_id(attributedStringForTextElement:))]
        #[unsafe(method_family = none)]
        fn observed_attributed_mapping(&self, element: &NSTextElement) -> Option<Retained<NSAttributedString>> {
            let observed = if receiver_observation_requested() {
                let count = self.ivars().mapping_observation_calls.get();
                if count < 4 {
                    self.ivars().mapping_observation_calls.set(count + 1);
                    Some(count + 1)
                } else {
                    None // 固定观察预算耗尽后不再递增，避免长枚举把诊断计数变成溢出故障。
                }
            } else {
                None // 默认/=1/bounded仍原样向父类映射，未增加任何元素内容getter。
            };
            if let Some(call) = observed {
                eprintln!(
                    "content_storage_probe stage=element_mapping_enter call={} element_class={} synthetic_only=true diagnostic_formal=false",
                    call, element.class().name().to_string_lossy(),
                );
            }
            // SAFETY: 公开方法参数是原NSTextElement，nullable返回按绑定原ABI单次super转发，无自造中间值。
            let value: Option<Retained<NSAttributedString>> = unsafe {
                msg_send![super(self), attributedStringForTextElement: element]
            };
            if let Some(call) = observed {
                let original_storage = self.ivars().storage.load(); // 临时weak升级只用于精确身份，不读原文字段。
                let is_original_storage = value.as_deref().zip(original_storage.as_deref()).map(|(value, storage)| {
                    let original: &NSAttributedString = storage;
                    std::ptr::eq(value, original)
                });
                eprintln!(
                    "content_storage_probe stage=element_mapping_return call={} present={} result_class={} result_is_virtual={} result_is_original_storage={:?} synthetic_only=true diagnostic_formal=false",
                    call,
                    value.is_some(),
                    value.as_deref().map_or_else(|| "nil".into(), |value| value.class().name().to_string_lossy()),
                    value.as_deref().is_some_and(virtual_attributed_receiver_is_ours),
                    is_original_storage,
                ); // 不取string/length，也不因返回原storage或framework对象而改变结果。
            }
            value // 唯一尾表达式按noncopy method_id移交原nullable对象，不做额外claim。
        }

        #[unsafe(method_id(textElementsForRange:))]
        #[unsafe(method_family = none)]
        fn observed_elements_for_range(&self, range: &NSTextRange) -> Retained<NSArray<NSTextElement>> {
            self.ivars().elements_calls.set(self.ivars().elements_calls.get().saturating_add(1));
            self.emit("elements_enter", 0, true);
            let elements = if self.ivars().bounded || self.ivars().virtual_paragraph {
                // 有限模式绝不向super索取完整paragraph；未准备好时明确提供空数组而非假Ready。
                match self.bounded_elements(range) {
                    Ok(elements) => elements,
                    Err(error) => { self.reject(error); NSArray::new() }
                }
            } else {
                // SAFETY: pass-through仍按原ABI单次转发，不复制或枚举原数组。
                unsafe { msg_send![super(self), textElementsForRange: range] }
            };
            self.emit("elements_return", 0, true);
            // 唯一尾表达式交给method_id执行非new selector的autorelease-return，未额外保留数组。
            elements
        }

        #[unsafe(method_id(enumerateTextElementsFromLocation:options:usingBlock:))]
        #[unsafe(method_family = none)]
        fn observed_enumeration(
            &self,
            location: Option<&ProtocolObject<dyn NSTextLocation>>,
            options: NSTextContentManagerEnumerationOptions,
            block: &block2::DynBlock<dyn Fn(NonNull<NSTextElement>) -> Bool + '_>,
        ) -> Option<Retained<ProtocolObject<dyn NSTextLocation>>> {
            self.ivars().enumeration_calls.set(self.ivars().enumeration_calls.get().saturating_add(1));
            self.emit("enumeration_enter", options.bits(), location.is_some());
            let end = if self.ivars().bounded || self.ivars().virtual_paragraph {
                // 固定分块只是读取路径试验；一次至多提供一个真实元素，公开协议允许有限枚举。
                // 官方helper只捕获ObjC异常；Rust panic仍原样展开。这里没有状态借用跨callback。
                // AssertUnwindSafe只声明本同步诊断边界：原phase/cache lease仍由既有RAII归还。
                match objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
                    self.bounded_enumeration(location, options, block)
                })) {
                    Ok(Ok(end)) => end,
                    Ok(Err(error)) => { self.reject(error); None }
                    Err(exception) => self.report_and_rethrow(exception),
                }
            } else {
                // SAFETY: 原NS_NOESCAPE block直接借给父类，不包装/复制/保存，也不改nil或方向。
                unsafe { msg_send![super(self), enumerateTextElementsFromLocation: location, options: options, usingBlock: block] }
            };
            self.emit("enumeration_return", options.bits(), end.is_some());
            // Optional位置仍由method_id在唯一尾表达式按原非new返回约定移交；不缓存Native位置。
            end
        }
    }
);

impl NativeMarkdownContentStorageProbe {
    /// 未显式启用时直接构造原标准类；启用时仍由原surface字段持有同一个实际content storage。
    pub(super) fn new_if_requested(
        storage: &Retained<PieceBackedMarkdownTextStorage>,
        ledger: &Rc<NativeMarkdownOwnedLedger>,
        mtm: MainThreadMarker,
    ) -> Result<Retained<NSTextContentStorage>, String> {
        let configured = std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_PROBE");
        let bounded = configured.as_deref() == Some(std::ffi::OsStr::new("bounded"));
        let virtual_paragraph =
            configured.as_deref() == Some(std::ffi::OsStr::new("virtual-paragraph"));
        if virtual_paragraph
            && std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_SYNTHETIC_EXCEPTION").as_deref()
                != Some(std::ffi::OsStr::new("1"))
        {
            return Err("虚拟段落实验只允许显式合成验收输入".to_owned());
        }
        if !bounded
            && !virtual_paragraph
            && configured.as_deref() != Some(std::ffi::OsStr::new("1"))
        {
            return Ok(NSTextContentStorage::new()); // 默认产品不分配计数对象或替换系统类。
        }
        let allocated = Self::alloc(mtm).set_ivars(ContentStorageProbeIvars {
            elements_calls: Cell::new(0),
            enumeration_calls: Cell::new(0),
            emitted: Cell::new(0),
            bounded,
            virtual_paragraph,
            storage: Weak::from_retained(storage),
            document: storage.ivars().document.borrow().clone(),
            ledger: Rc::downgrade(ledger),
            busy: Cell::new(false),
            failures: Cell::new(0),
            cache: CacheOwner::new(FiniteParagraphCache::empty()),
            cached_entries: Cell::new(0),
            paragraph_fact_logs: Cell::new(0),
            mapping_observation_calls: Cell::new(0),
            trace: Rc::new(ProviderPhaseTrace::new()),
            fragment_probe: RefCell::new(None),
        });
        // SAFETY: 固定计数已初始化，标准NSTextContentStorage的init仍只调用一次，无第二套backing。
        let content: Retained<Self> = unsafe { msg_send![super(allocated), init] };
        ledger.include("editor.content_storage_probe", &content)?;
        content.emit("installed", 0, true);
        if bounded {
            eprintln!(
                "content_storage_probe mode=bounded diagnostic_formal=false parity=false chunk_utf16_limit={CONTENT_PROBE_UTF16_LIMIT} cache_limit={CONTENT_PROBE_CACHE_LIMIT} eviction=false"
            );
        }
        if virtual_paragraph {
            eprintln!(
                "content_storage_probe mode=virtual-paragraph synthetic_only=true read_only=true parity=false diagnostic_formal=false physical_paragraph=true whole_bulk_forwarded=true"
            );
        }
        // into_super移动唯一+1所有权；manager/observer/view继续消费原来的公开基类类型。
        Ok(content.into_super())
    }

    fn emit(&self, stage: &'static str, options: usize, present: bool) {
        let emitted = self.ivars().emitted.get();
        if emitted >= CONTENT_PROBE_LOG_LIMIT {
            return; // 最多24条固定标量，计数继续累计但不输出Native地址、source或输入内容。
        }
        self.ivars().emitted.set(emitted + 1);
        eprintln!(
            "content_storage_probe stage={stage} options={options} present={present} elements_calls={} enumeration_calls={} last_native_phase={} last_native_returned={} native_calls={}",
            self.ivars().elements_calls.get(),
            self.ivars().enumeration_calls.get(),
            self.ivars().trace.last_phase.get(),
            self.ivars().trace.last_returned.get(),
            self.ivars().trace.calls.get(),
        );
    }

    fn report_and_rethrow(&self, exception: Option<Retained<objc2::exception::Exception>>) -> ! {
        let trace = &self.ivars().trace;
        let synthetic_detail = std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_SYNTHETIC_EXCEPTION")
            .as_deref()
            == Some(std::ffi::OsStr::new("1"));
        if let Some(exception) = exception {
            // class来自公开runtime元数据，不调用任意异常对象的description或Debug，避免泄露地址/正文。
            let class = exception.class();
            eprintln!(
                "content_storage_probe stage=objc_exception class={} last_native_phase={} last_native_returned={} native_calls={} synthetic_detail={} diagnostic_formal=false",
                class.name().to_string_lossy(),
                trace.last_phase.get(),
                trace.last_returned.get(),
                trace.calls.get(),
                synthetic_detail,
            );
            let exception_class = objc2::runtime::AnyClass::get(c"NSException");
            let mut ancestor = Some(class);
            let mut is_exception = false;
            for _ in 0..64 {
                let Some(candidate) = ancestor else {
                    break; // 任意ObjC对象也可以被throw；未证明NSException就不能发送name/reason。
                };
                if exception_class.is_some_and(|expected| std::ptr::eq(candidate, expected)) {
                    is_exception = true;
                    break; // 只核公开继承链，绝不推断或调用异常对象的私有ABI。
                }
                ancestor = candidate.superclass();
            }
            if is_exception {
                // SAFETY: runtime继承链已证明NSException；公开name属性返回非空NSString且getter不改异常。
                let name: Retained<NSString> = unsafe { msg_send![&exception, name] };
                eprintln!(
                    "content_storage_probe stage=objc_exception_name name={} diagnostic_formal=false",
                    fixed_exception_name(&name),
                );
                if synthetic_detail {
                    // SAFETY: 同一已认证NSException的公开reason允许nil；仅显式合成fixture开关读取此诊断文本。
                    let reason: Option<Retained<NSString>> =
                        unsafe { msg_send![&exception, reason] };
                    if let Some(reason) = reason {
                        eprintln!(
                            "content_storage_probe stage=objc_exception_synthetic_reason reason={:?} diagnostic_formal=false",
                            finite_exception_reason(&reason),
                        );
                    } else {
                        eprintln!(
                            "content_storage_probe stage=objc_exception_synthetic_reason reason=nil diagnostic_formal=false"
                        );
                    }
                }
            } else {
                eprintln!(
                    "content_storage_probe stage=objc_exception_name name=not_nsexception diagnostic_formal=false"
                );
            }
            // 官方throw消费helper返回的原+1异常；不换异常、不继续provider，也不改变原失败/释放门槛。
            objc2::exception::throw(exception)
        } else {
            eprintln!(
                "content_storage_probe stage=objc_exception class=nil name=nil last_native_phase={} last_native_returned={} native_calls={} diagnostic_formal=false",
                trace.last_phase.get(),
                trace.last_returned.get(),
                trace.calls.get(),
            );
            // SAFETY: helper明确用None表示原throw nil；Apple现代runtime支持nil，原样抛出不伪造异常对象。
            unsafe { objc2::ffi::objc_exception_throw(std::ptr::null_mut()) }
        }
    }

    fn reject(&self, failure: ProviderFailure) {
        let bit = failure as u16;
        let previous = self.ivars().failures.get();
        if previous & bit == 0 {
            self.ivars().failures.set(previous | bit);
            // 拒绝独立于普通日志预算；容量耗尽或映射失败不能被早期调用刷掉。
            eprintln!(
                "content_storage_probe mode={} rejected={} diagnostic_formal=false",
                if self.ivars().virtual_paragraph {
                    "virtual-paragraph"
                } else {
                    "bounded"
                },
                failure.code()
            );
        }
    }

    fn enter(&self) -> Result<ProviderGuard<'_>, ProviderFailure> {
        if self.ivars().busy.replace(true) {
            return Err(ProviderFailure::Busy); // 不包装block，也不以super枚举制造无限递归。
        }
        Ok(ProviderGuard(&self.ivars().busy))
    }

    fn capture(
        &self,
    ) -> Result<(ProviderTicket, Retained<ProtocolObject<dyn NSTextLocation>>), ProviderFailure>
    {
        let storage = self
            .ivars()
            .trace
            .call("capture.weak_storage", || self.ivars().storage.load())
            .ok_or(ProviderFailure::Source)?;
        if !std::rc::Weak::ptr_eq(&self.ivars().document, &storage.ivars().document.borrow()) {
            return Err(ProviderFailure::Source); // 不采纳后继笔记重新绑定的同一个adapter。
        }
        let (generation, length, selection) = {
            let document = self
                .ivars()
                .document
                .upgrade()
                .ok_or(ProviderFailure::Source)?;
            let document = document.borrow();
            (
                document.generation(),
                document.utf16_len(),
                document.selection(),
            )
        };
        let key = ContentKey {
            generation,
            length,
            selection,
            mode: storage.presentation_mode(),
            semantic_epoch: storage.ivars().presentation_semantic_epoch.get(),
            table_epoch: storage.ivars().table_geometry_epoch.get(),
            input_epoch: storage.ivars().input_transaction_epoch.get(),
            input_depth: storage.ivars().native_input_depth.get(),
            composition: storage.ivars().composition_active.get(),
        };
        let ticket = ProviderTicket {
            storage,
            document: self.ivars().document.clone(),
            key,
            trace: Rc::clone(&self.ivars().trace),
        };
        let observer = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(self.as_super());
        let installed = ticket
            .native("capture.observer_storage", || observer.textStorage())?
            .ok_or(ProviderFailure::Graph)?;
        if !std::ptr::eq::<NSTextStorage>(&*installed, ticket.storage.as_super()) {
            return Err(ProviderFailure::Graph); // 仍须是原graph上的唯一piece storage。
        }
        let manager = ticket
            .native("capture.primary_manager", || {
                self.primaryTextLayoutManager()
            })?
            .ok_or(ProviderFailure::Graph)?;
        let actual_content = ticket
            .native("capture.manager_content", || manager.textContentManager())?
            .ok_or(ProviderFailure::Graph)?;
        if !std::ptr::eq::<objc2_app_kit::NSTextContentManager>(
            &*actual_content,
            self.as_super().as_super(),
        ) {
            return Err(ProviderFailure::Graph); // 不能给另一manager提供同数值位置的元素。
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(self.as_super());
        // 只使用公开全局位置API，不调用super元素枚举或paragraph创建路径。
        let document_range =
            ticket.native("capture.document_range", || provider.documentRange())?;
        let origin = ticket.native("capture.origin", || document_range.location())?;
        let end = ticket.native("capture.end", || document_range.endLocation())?;
        let length = ticket.native("capture.length", || {
            self.offsetFromLocation_toLocation(&origin, &end)
        })?;
        if usize::try_from(length).ok() != Some(key.length) {
            return Err(ProviderFailure::Location); // 全局文档范围不能缩成当前chunk或另一来源。
        }
        Ok((ticket, origin))
    }

    fn offset(
        &self,
        ticket: &ProviderTicket,
        origin: &ProtocolObject<dyn NSTextLocation>,
        location: &ProtocolObject<dyn NSTextLocation>,
    ) -> Result<usize, ProviderFailure> {
        let offset = ticket.native("global.offset", || {
            self.offsetFromLocation_toLocation(origin, location)
        })?;
        let offset = usize::try_from(offset).map_err(|_| ProviderFailure::Location)?;
        if offset > ticket.key.length {
            return Err(ProviderFailure::Location); // NSNotFound或异文档位置不能截成合法索引。
        }
        Ok(offset)
    }

    fn location(
        &self,
        ticket: &ProviderTicket,
        origin: &ProtocolObject<dyn NSTextLocation>,
        offset: usize,
    ) -> Result<Retained<ProtocolObject<dyn NSTextLocation>>, ProviderFailure> {
        let offset_value = isize::try_from(offset).map_err(|_| ProviderFailure::Location)?;
        let location = ticket
            .native("global.location", || {
                self.locationFromLocation_withOffset(origin, offset_value)
            })?
            .ok_or(ProviderFailure::Location)?;
        if self.offset(ticket, origin, &location)? != offset {
            return Err(ProviderFailure::Location); // 每个发布端点都须按同一origin精确反查。
        }
        Ok(location)
    }

    fn include_owned(
        &self,
        ticket: &ProviderTicket,
        role: &'static str,
        object: &AnyObject,
    ) -> Result<(), ProviderFailure> {
        let ledger = self
            .ivars()
            .ledger
            .upgrade()
            .ok_or(ProviderFailure::Ledger)?;
        ticket
            .native("owned.include", || ledger.include(role, object))?
            .map_err(|_| ProviderFailure::Ledger)
    }

    fn read_units(
        &self,
        ticket: &ProviderTicket,
        source: &Range<usize>,
    ) -> Result<Vec<u16>, ProviderFailure> {
        if source.is_empty()
            || source.end > ticket.key.length
            || source.len() > CONTENT_PROBE_UTF16_LIMIT
        {
            return Err(ProviderFailure::Range); // 每一个实际bulk请求均受同一4096上限约束。
        }
        if source.start == 0 && source.end == ticket.key.length {
            return Err(ProviderFailure::WholeRead); // 小文档也不能借探针清空原全文读取门禁。
        }
        let string = ticket.native("read.string", || ticket.storage.as_super().string())?;
        if ticket.native("read.length", || string.length())? != ticket.key.length {
            return Err(ProviderFailure::Graph); // 必须读取原虚拟NSString，不从另一镜像获得正文。
        }
        let mut units = vec![0u16; source.len()];
        ticket.native("read.get_characters", || {
            // SAFETY: 固定Vec恰好容纳已校验范围；仍调用原adapter primitive，原读量诊断完整保留。
            unsafe {
                string.getCharacters_range(
                    NonNull::new(units.as_mut_ptr()).expect("非空源窗口"),
                    NSRange::new(source.start, source.len()),
                )
            };
        })?;
        Ok(units)
    }

    fn source_for_offset(
        &self,
        ticket: &ProviderTicket,
        cache: &FiniteParagraphCache,
        offset: usize,
    ) -> Result<Range<usize>, ProviderFailure> {
        if let Some(entry) = cache
            .entries
            .iter()
            .find(|entry| entry.key == ticket.key && entry.source.contains(&offset))
        {
            return Ok(entry.source.clone()); // 只复用同票已发布元素的纯范围，不重新复制内容。
        }
        let grid = {
            let document = ticket.document.upgrade().ok_or(ProviderFailure::Source)?;
            let document = document.borrow();
            finite_source_grid(ticket.key.length, offset, |point| {
                if document.byte_offset_for_utf16(point).is_err() {
                    false // 不能把surrogate中点或越界位置解释为合法网格边界。
                } else if point == 0 || point == ticket.key.length {
                    true // 已由scalar索引证明的真实文档两端无需读取不存在的相邻unit。
                } else {
                    match (
                        document.utf16_code_unit_at(point - 1),
                        document.utf16_code_unit_at(point),
                    ) {
                        (Ok(13), Ok(10)) => false, // CRLF中点不是本试验允许的paragraph边界。
                        (Ok(_), Ok(_)) => true,    // 两侧真实unit读取成功且不是CRLF才允许。
                        (Err(_), _) | (_, Err(_)) => false, // 读取错误一律拒绝，不能静默变成有效边界。
                    }
                }
            })
            .ok_or(ProviderFailure::Range)?
        }; // canonical借用在实际Native读取前结束；只用小量边界元数据。
        let units = self.read_units(ticket, &grid)?;
        segment_in_grid(&units, grid.start, offset).ok_or(ProviderFailure::Range)
    }

    fn build_paragraph(
        &self,
        ticket: &ProviderTicket,
        origin: &ProtocolObject<dyn NSTextLocation>,
        source: &Range<usize>,
    ) -> Result<Retained<NSTextParagraph>, ProviderFailure> {
        let mut units = self.read_units(ticket, source)?;
        let allocated_string = ticket.native("string.alloc", NSMutableString::alloc)?;
        let string = ticket.native("string.init", || {
            // SAFETY: 临时mutable字符串复制有限UTF16，不借Vec；避免短值成为永不nil的tagged NSString。
            unsafe {
                NSMutableString::initWithCharacters_length(
                    allocated_string,
                    NonNull::new(units.as_mut_ptr()).expect("非空源段"),
                    units.len(),
                )
            }
        })?;
        self.include_owned(ticket, "editor.content_provider.string", &string)?;
        let allocated_builder = ticket.native("builder.alloc", NSMutableAttributedString::alloc)?;
        let builder = ticket.native("builder.init", || {
            NSMutableAttributedString::initWithString(allocated_builder, &string)
        })?;
        self.include_owned(ticket, "editor.content_provider.builder", &builder)?;
        let mut cursor = source.start;
        while cursor < source.end {
            let mut effective = NSRange::new(0, 0);
            let attributes = ticket.native("attributes.get", || {
                // SAFETY: 栈上range输出活到同步getter返回；属性仍来自原piece storage，未引入第二模型。
                unsafe {
                    ticket
                        .storage
                        .as_super()
                        .attributesAtIndex_effectiveRange(cursor, &mut effective)
                }
            })?;
            let end = effective
                .location
                .checked_add(effective.length)
                .ok_or(ProviderFailure::Attributes)?;
            if effective.location > cursor || end <= cursor {
                return Err(ProviderFailure::Attributes); // 空run或不覆盖cursor时不能填默认样式伪成功。
            }
            let end = end.min(source.end);
            let local = NSRange::new(cursor - source.start, end - cursor);
            ticket.native("attributes.add", || {
                // SAFETY: 字典来自正确类型的原属性primitive；local严格属于有限builder字符范围。
                unsafe { builder.addAttributes_range(&attributes, local) };
            })?;
            cursor = end; // 每轮至少前进一个unit，最多source.len轮，绝不扫描全文。
        }
        let allocated_frozen = ticket.native("frozen.alloc", NSAttributedString::alloc)?;
        let frozen = ticket.native("frozen.init", || {
            NSAttributedString::initWithAttributedString(allocated_frozen, &builder)
        })?;
        self.include_owned(ticket, "editor.content_provider.attributed", &frozen)?;
        let start = self.location(ticket, origin, source.start)?;
        let end = self.location(ticket, origin, source.end)?;
        let allocated_range = ticket.native("range.alloc", NSTextRange::alloc)?;
        let range = ticket
            .native("range.init", || {
                NSTextRange::initWithLocation_endLocation(allocated_range, &start, Some(&end))
            })?
            .ok_or(ProviderFailure::Location)?;
        self.include_owned(ticket, "editor.content_provider.range", &range)?;
        let allocated_paragraph = ticket.native("paragraph.alloc", NSTextParagraph::alloc)?;
        let paragraph = ticket.native("paragraph.init", || {
            NSTextParagraph::initWithAttributedString(allocated_paragraph, Some(&frozen))
        })?;
        self.include_owned(ticket, "editor.content_provider.paragraph", &paragraph)?;
        ticket.native("paragraph.set_range", || {
            paragraph.setElementRange(Some(&range))
        })?;
        ticket.native("paragraph.set_manager", || {
            paragraph.setTextContentManager(Some(self.as_super().as_super()))
        })?;
        // 冻结值的长度与源range严格一致；这没有承诺跨chunk的软行/UBA/IME等价。
        let actual_value = ticket.native("paragraph.value", || paragraph.attributedString())?;
        if ticket.native("paragraph.value_length", || actual_value.length())? != source.len() {
            return Err(ProviderFailure::Range);
        }
        Ok(paragraph)
    }

    fn paragraph(
        &self,
        ticket: &ProviderTicket,
        origin: &ProtocolObject<dyn NSTextLocation>,
        offset: usize,
        reverse: bool,
        cache: &mut FiniteParagraphCache,
    ) -> Result<Option<Retained<NSTextParagraph>>, ProviderFailure> {
        if ticket.key.length == 0 || (!reverse && offset == ticket.key.length) {
            return Ok(None); // 没有源字符就不制造EOF paragraph或额外视觉行。
        }
        let mut source = if self.ivars().virtual_paragraph {
            0..ticket.key.length // 合成单物理段完整覆盖0..EOF，绝不使用4096网格伪造段界。
        } else {
            self.source_for_offset(ticket, cache, offset.min(ticket.key.length - 1))?
        };
        if reverse && offset < ticket.key.length {
            if source.start == 0 {
                return Ok(None); // Reverse按公开契约从包含location的前一个element开始。
            }
            source = self.source_for_offset(ticket, cache, source.start - 1)?;
        }
        if let Some(entry) = cache
            .entries
            .iter()
            .find(|entry| entry.key == ticket.key && entry.source == source)
        {
            return ticket
                .native("cache.retain", || entry.paragraph.clone())
                .map(Some); // 同一element身份稳定，只有临时+1claim。
        }
        if cache.entries.len() == CONTENT_PROBE_CACHE_LIMIT {
            return Err(ProviderFailure::Capacity); // 活动期间不淘汰任何已发布paragraph，避免weak fragment断联。
        }
        let paragraph = if self.ivars().virtual_paragraph {
            virtual_paragraph::build(self, ticket, origin)? // 不复制文本或属性run，只构造Weak只读虚拟值。
        } else {
            self.build_paragraph(ticket, origin, &source)?
        };
        let cached = ticket.native("cache.store_retain", || paragraph.clone())?;
        cache.entries.push(FiniteParagraph {
            key: ticket.key,
            source,
            paragraph: cached,
        });
        self.ivars().cached_entries.set(cache.entries.len());
        Ok(Some(paragraph))
    }

    fn paragraph_range_facts(
        &self,
        ticket: &ProviderTicket,
        origin: &ProtocolObject<dyn NSTextLocation>,
        range: Option<&NSTextRange>,
    ) -> Result<ParagraphRangeFacts, ProviderFailure> {
        let mut facts = ParagraphRangeFacts {
            present: range.is_some(),
            start_present: false,
            end_present: false,
            start: None,
            end: None,
            roundtrip: false,
        };
        let Some(range) = range else {
            return Ok(facts); // nullable子范围未提供时只记录缺失，不能制造端点或向offset传nil。
        };
        let start: Option<Retained<ProtocolObject<dyn NSTextLocation>>> =
            ticket.native("verify.range_start", || {
                // SAFETY: 发送公开location getter；使用相同对象ABI的Option专门检测违反nonnull的实际nil。
                unsafe { msg_send![range, location] }
            })?;
        let end: Option<Retained<ProtocolObject<dyn NSTextLocation>>> =
            ticket.native("verify.range_end", || {
                // SAFETY: 同一公开NSTextRange的endLocation getter；nil只记为缺失，绝不参与位置运算。
                unsafe { msg_send![range, endLocation] }
            })?;
        facts.start_present = start.is_some();
        facts.end_present = end.is_some();
        let (Some(start), Some(end)) = (start, end) else {
            return Ok(facts); // 缺失的真实端点不能用document origin或请求source替代。
        };
        for (location, offset) in [(&start, &mut facts.start), (&end, &mut facts.end)] {
            match self.offset(ticket, origin, location) {
                Ok(actual) => *offset = Some(actual),
                Err(ProviderFailure::Location) => {} // 无法映射保留None，不把未知坐标当合法范围。
                Err(error) => return Err(error),     // 原票撤权等错误立即结束，不继续读取后继状态。
            }
        }
        if let (Some(start_offset), Some(end_offset)) = (facts.start, facts.end)
            && start_offset <= end_offset
        {
            let original_start = self.location(ticket, origin, start_offset)?;
            let original_end = self.location(ticket, origin, end_offset)?;
            let start_equal = ticket.native("verify.roundtrip_start", || {
                start.compare(&original_start) == objc2_foundation::NSComparisonResult::Same
            })?;
            let end_equal = ticket.native("verify.roundtrip_end", || {
                end.compare(&original_end) == objc2_foundation::NSComparisonResult::Same
            })?;
            facts.roundtrip = start_equal && end_equal; // 同一provider重建位置须与实际端点公开比较相等。
        }
        Ok(facts)
    }

    fn validate_paragraph_before_callback(
        &self,
        ticket: &ProviderTicket,
        origin: &ProtocolObject<dyn NSTextLocation>,
        paragraph: &NSTextParagraph,
        source: &Range<usize>,
        cache_slot: usize,
    ) -> Result<Retained<NSTextRange>, ProviderFailure> {
        let manager = ticket.native("verify.manager", || paragraph.textContentManager())?;
        let manager_current = manager.as_deref().is_some_and(|manager| {
            std::ptr::eq::<objc2_app_kit::NSTextContentManager>(manager, self.as_super().as_super())
        });
        let represented =
            ticket.native("verify.represented", || paragraph.isRepresentedElement())?;
        let transaction = ticket.native("verify.transaction", || self.hasEditingTransaction())?;
        let range = ticket.native("verify.element_range", || paragraph.elementRange())?;
        let content =
            ticket.native("verify.content_range", || paragraph.paragraphContentRange())?;
        let separator = ticket.native("verify.separator_range", || {
            paragraph.paragraphSeparatorRange()
        })?;
        let element_facts = self.paragraph_range_facts(ticket, origin, range.as_deref())?;
        let content_facts = self.paragraph_range_facts(ticket, origin, content.as_deref())?;
        let separator_facts = self.paragraph_range_facts(ticket, origin, separator.as_deref())?;
        let valid_element = manager_current
            && element_facts.present
            && element_facts.start_present
            && element_facts.end_present
            && element_facts.start == Some(source.start)
            && element_facts.end == Some(source.end)
            && element_facts.roundtrip;
        let emitted = self.ivars().paragraph_fact_logs.get();
        if emitted < 4 {
            self.ivars().paragraph_fact_logs.set(emitted + 1);
            // 固定前四次发布事实足够区分首轮构造与后续命中；不输出对象地址、属性值或正文。
            eprintln!(
                "content_storage_probe stage=paragraph_facts enumeration_calls={} cache_slot={} source_start={} source_end={} manager_present={} manager_current={} represented={} transaction={} element={:?} content={:?} separator={:?} valid_element={} diagnostic_formal=false",
                self.ivars().enumeration_calls.get(),
                cache_slot,
                source.start,
                source.end,
                manager.is_some(),
                manager_current,
                represented,
                transaction,
                element_facts,
                content_facts,
                separator_facts,
                valid_element,
            );
        }
        if !valid_element {
            return Err(ProviderFailure::Range); // Native撤销的旧元素不能重贴range或静默重建后继续原callback。
        }
        // paragraph子range按公开nullable契约只观察，不把未知范围强行补成source，也不改setter顺序。
        range.ok_or(ProviderFailure::Range)
    }

    fn bounded_elements(
        &self,
        requested: &NSTextRange,
    ) -> Result<Retained<NSArray<NSTextElement>>, ProviderFailure> {
        let _phase = self.enter()?;
        let (ticket, origin) = self.capture()?;
        let location = ticket.native("elements.start", || requested.location())?;
        let end = ticket.native("elements.end", || requested.endLocation())?;
        let offset = self.offset(&ticket, &origin, &location)?;
        if self.offset(&ticket, &origin, &end)? < offset {
            return Err(ProviderFailure::Location); // 不把倒序或异源范围变成另一个有效请求。
        }
        let mut lease = match self.ivars().cache.begin() {
            CacheAdmission::Ready(lease) => lease,
            CacheAdmission::Loaned | CacheAdmission::Closed => return Err(ProviderFailure::Busy),
        };
        let paragraph = self.paragraph(&ticket, &origin, offset, false, lease.payload_mut())?;
        if let Some(paragraph) = paragraph {
            let elements = ticket.native("elements.array", || {
                NSArray::from_retained_slice(&[paragraph.into_super()])
            })?;
            self.include_owned(&ticket, "editor.content_provider.array", &elements)?;
            Ok(elements) // 协议允许只返回已同步可用的相交前缀，不宣称填满请求范围。
        } else {
            Ok(NSArray::new()) // 标准空数组不缓存，也不作为已布局证明。
        }
    }

    fn bounded_enumeration(
        &self,
        location: Option<&ProtocolObject<dyn NSTextLocation>>,
        options: NSTextContentManagerEnumerationOptions,
        block: &block2::DynBlock<dyn Fn(NonNull<NSTextElement>) -> Bool + '_>,
    ) -> Result<Option<Retained<ProtocolObject<dyn NSTextLocation>>>, ProviderFailure> {
        let _phase = self.enter()?;
        let (ticket, origin) = self.capture()?;
        let reverse = options.contains(NSTextContentManagerEnumerationOptions::Reverse);
        let offset = match location {
            Some(location) => self.offset(&ticket, &origin, location)?, // 保留原公开位置，不猜附近段落。
            None if reverse => ticket.key.length, // 公开nil反向枚举从真实document.end开始。
            None => 0,                            // 公开nil正向枚举从真实document.location开始。
        };
        let mut lease = match self.ivars().cache.begin() {
            CacheAdmission::Ready(lease) => lease,
            CacheAdmission::Loaned | CacheAdmission::Closed => return Err(ProviderFailure::Busy),
        };
        let Some(paragraph) =
            self.paragraph(&ticket, &origin, offset, reverse, lease.payload_mut())?
        else {
            return Ok(None); // 未提供任何元素时不伪造枚举边界。
        };
        let (cache_slot, source) = {
            let cache = lease.payload_mut();
            let (index, entry) = cache
                .entries
                .iter()
                .enumerate()
                .find(|(_, entry)| {
                    entry.key == ticket.key
                        && std::ptr::eq::<NSTextParagraph>(&*entry.paragraph, &*paragraph)
                })
                .ok_or(ProviderFailure::Range)?;
            (index + 1, entry.source.clone()) // 固定缓存槽号是纯标量；Native验证前结束局部payload借用。
        };
        let range = self.validate_paragraph_before_callback(
            &ticket, &origin, &paragraph, &source, cache_slot,
        )?;
        let edge = if reverse {
            ticket.native("enumeration.reverse_edge", || range.location())? // Reverse返回本次真实已枚举元素的前缘。
        } else {
            ticket.native("enumeration.forward_edge", || range.endLocation())? // Forward返回本次真实已枚举元素的后缘。
        };
        let element: &NSTextElement = &paragraph;
        let _continue_requested = ticket.native("enumeration.original_block", || {
            block.call((NonNull::from(element),))
        })?;
        // 一次固定只交付一个元素；block已原样调用，true也不要求无界枚举或淘汰已发布缓存。
        Ok(Some(edge))
    }
}

/// 两个开关同时匹配才观察；此helper不取得任何Native对象或正文owner。
pub(super) fn receiver_observation_requested() -> bool {
    std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_PROBE").as_deref()
        == Some(std::ffi::OsStr::new("virtual-paragraph"))
        && std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_SYNTHETIC_EXCEPTION").as_deref()
            == Some(std::ffi::OsStr::new("1"))
}

/// 只用公开runtime类身份分类，不强制注册新类或向对象发送data getter。
pub(super) fn virtual_attributed_receiver_is_ours(value: &NSAttributedString) -> bool {
    virtual_paragraph::virtual_attributed_receiver_is_ours(value)
}

/// 默认日志只发布固定的公开异常名，不格式化任意name字符串或异常对象。
fn fixed_exception_name(name: &NSString) -> &'static str {
    for (known, label) in [
        (ns_string!("NSRangeException"), "NSRangeException"),
        (
            ns_string!("NSInvalidArgumentException"),
            "NSInvalidArgumentException",
        ),
        (
            ns_string!("NSInternalInconsistencyException"),
            "NSInternalInconsistencyException",
        ),
        (ns_string!("NSGenericException"), "NSGenericException"),
        (ns_string!("NSMallocException"), "NSMallocException"),
    ] {
        if name.isEqualToString(known) {
            return label; // 只输出匹配的固定标签，绝不把任意异常name当用户可见正文。
        }
    }
    "other_nsexception" // 未识别名称仍保留原异常，不能改判成已知契约错误。
}

/// 只供显式合成fixture：最多复制1024 UTF16单位的系统异常reason，不接触文档adapter。
fn finite_exception_reason(reason: &NSString) -> String {
    let length = reason.length().min(1024);
    let mut units = [0u16; 1024];
    if length > 0 {
        // SAFETY: 公开NSString长度已认证，固定栈缓冲容纳本次0..length读取且不越过reason末尾。
        unsafe {
            reason.getCharacters_range(
                NonNull::new(units.as_mut_ptr()).expect("固定非空诊断缓冲"),
                NSRange::new(0, length),
            )
        };
    }
    String::from_utf16_lossy(&units[..length]) // 截断surrogate只影响合成诊断显示，不更改原异常或源数据。
}

/// 固定source网格只是归因参数；stride留一个unit给scalar/CRLF边界，硬上限始终4096。
fn finite_source_grid(
    length: usize,
    offset: usize,
    mut boundary: impl FnMut(usize) -> bool,
) -> Option<Range<usize>> {
    if offset >= length {
        return None; // EOF没有源字符，不从末尾猜另一个请求。
    }
    let stride = CONTENT_PROBE_UTF16_LIMIT - 1;
    let mut bucket = offset / stride;
    for _ in 0..2 {
        let mut start = bucket.checked_mul(stride)?;
        let mut end = start.saturating_add(stride).min(length);
        if start > 0 && !boundary(start) {
            start -= 1;
        }
        if end > 0 && !boundary(end) {
            end -= 1;
        }
        if start <= offset
            && offset < end
            && end - start <= CONTENT_PROBE_UTF16_LIMIT
            && boundary(start)
            && boundary(end)
        {
            return Some(start..end);
        }
        bucket = bucket.checked_add(1)?; // offset恰在向前收缩的grid边界时，只再看后一块。
    }
    None // 不满足真实边界就拒绝，不能用超预算或完整段落兜底。
}

/// 网格内部仍按真实CR/LF/PS分段，避免把包含多个物理段落的值伪装成单个NSTextParagraph。
fn segment_in_grid(units: &[u16], origin: usize, offset: usize) -> Option<Range<usize>> {
    let mut start = 0;
    let mut index = 0;
    while index < units.len() {
        let separator = matches!(units[index], 10 | 13 | 0x2029);
        index += 1;
        if separator && units[index - 1] == 13 && units.get(index) == Some(&10) {
            index += 1; // 真实CRLF只有一个段界，不能添加孤立LF段。
        }
        if separator || index == units.len() {
            if origin + start <= offset && offset < origin + index {
                return Some(origin + start..origin + index);
            }
            start = index; // 每次只推进真实分隔符或有限grid末端，不插入源字符。
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_grid_covers_shifted_scalar_boundary_without_exceeding_limit() {
        let boundary = |point| point != 4095;
        assert_eq!(finite_source_grid(20_000, 4094, boundary), Some(4094..8190));
        assert_eq!(finite_source_grid(20_000, 4095, boundary), Some(4094..8190));
        assert_eq!(finite_source_grid(20_000, 20_000, boundary), None);
    }

    #[test]
    fn finite_segments_preserve_real_crlf_and_global_coordinates() {
        let units: Vec<_> = "a\r\n中\n末".encode_utf16().collect();
        assert_eq!(segment_in_grid(&units, 100, 102), Some(100..103));
        assert_eq!(segment_in_grid(&units, 100, 103), Some(103..105));
        assert_eq!(segment_in_grid(&units, 100, 105), Some(105..106));
        assert_eq!(segment_in_grid(&units, 100, 106), None);
    }
}
