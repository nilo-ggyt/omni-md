//! Opt-in TextKit content-provider attribution. Fixed chunks investigate element access only;
//! they establish no soft-wrap, bidi, editing, or default paged-editor lazy-loading equivalence.

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

/// Record fixed phases of existing calls only, without Native getters or retained/read text.
struct ProviderPhaseTrace {
    emitted: Cell<u8>,
    seen_phases: Cell<u128>,
    calls: Cell<u64>,
    last_phase: Cell<&'static str>,
    last_returned: Cell<bool>,
}

impl ProviderPhaseTrace {
    fn new() -> Self {
        // Idle diagnostics allocate no text or native owner; phase metadata is finite scalars.
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
            // Emit each fixed phase's first entry only, so attribute runs cannot consume later
            // setter/block visibility. The total log allowance remains the original finite budget.
            eprintln!(
                "content_storage_probe native_phase={phase} call={call} previous_phase={previous_phase} previous_returned={previous_returned} diagnostic_formal=false"
            );
        }
        let value = operation();
        // Mark return only after normal completion. Foreign exceptions and Rust panic leave
        // false rather than misreporting stack unwind as a successful native return.
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
        } // An unused probe allocates neither Source text nor element capacity.
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
            Self::Busy => "reentrant_provider", // Do not escape reentry through whole superclass enumeration.
            Self::Source => "source_unavailable", // Original weak owner ended or was closed.
            Self::Stale => "original_ticket_changed", // Original generation/mode/input authority changed.
            Self::Graph => "storage_graph_changed", // Provider no longer observes the same piece storage.
            Self::Location => "global_location_failed", // Public positions fail exact roundtrip.
            Self::Range => "finite_source_range_failed", // Actual finite Source bounds are unverified.
            Self::WholeRead => "whole_read_guard", // This bounded prototype issues no bulk equal to original full-text length.
            Self::Capacity => "finite_cache_exhausted", // Do not evict elements still referenced weakly by fragments.
            Self::Attributes => "finite_attributes_failed", // Original font/semantic run does not cover this Source position.
            Self::Ledger => "owned_registration_failed", // Never publish an unregistered owned object.
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
        // This ticket checks original document identity and derived state only, not a post-read
        // Source-health result. The reference live NSString's 0xfffd fallback semantics stay intact.
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
            return false; // The same content object cannot renew an old ticket for successor input, mode, or note.
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
            return Err(ProviderFailure::Stale); // Verify original authority before Native, without replacing it with a current ticket.
        }
        let value = self.trace.call(phase, operation);
        if self.current() {
            Ok(value)
        } else {
            Err(ProviderFailure::Stale) // The returned value drops on this stack; revoked work publishes no object.
        }
    }
}

/// Fixed scalar facts from actual public ranges; None never means a valid zero coordinate.
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
        self.0.set(false); // End this synchronous phase, without scheduling a retry or nested run loop.
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
        drop(fragment_owner); // Remove the owner from its slot before weak-manager delegate cleanup; no RefCell borrow crosses Native.
        // Report scalar counts at actual object destruction only, without Native getters, text,
        // paths, or deferred retries.
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
    // SAFETY: Restricts this class to main-thread use without overriding storage, editing
    // transactions, or position mapping; those retain their standard superclass contracts.
    #[unsafe(super = NSTextContentStorage)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ContentStorageProbeIvars]
    pub(super) struct NativeMarkdownContentStorageProbe;

    // SAFETY: No extra NSObject obligations; the original surface still owns this actual object and requires its release.
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
                    None // Stop incrementing after the finite observation budget, avoiding diagnostic overflow during long enumeration.
                }
            } else {
                None // Default/=1/bounded modes preserve superclass mapping without adding element-content getters.
            };
            if let Some(call) = observed {
                eprintln!(
                    "content_storage_probe stage=element_mapping_enter call={} element_class={} synthetic_only=true diagnostic_formal=false",
                    call, element.class().name().to_string_lossy(),
                );
            }
            // SAFETY: Forward the original public NSTextElement once to super with its nullable
            // return ABI, without a manufactured intermediate value.
            let value: Option<Retained<NSAttributedString>> = unsafe {
                msg_send![super(self), attributedStringForTextElement: element]
            };
            if let Some(call) = observed {
                let original_storage = self.ivars().storage.load(); // Temporary weak upgrade compares exact identity only, without text-field reads.
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
                ); // Read neither string nor length; returning original storage/framework values does not change the result.
            }
            value // The sole noncopy method_id return transfers the original nullable value without another claim.
        }

        #[unsafe(method_id(textElementsForRange:))]
        #[unsafe(method_family = none)]
        fn observed_elements_for_range(&self, range: &NSTextRange) -> Retained<NSArray<NSTextElement>> {
            self.ivars().elements_calls.set(self.ivars().elements_calls.get().saturating_add(1));
            self.emit("elements_enter", 0, true);
            let elements = if self.ivars().bounded || self.ivars().virtual_paragraph {
                // Finite modes never request whole superclass paragraphs. Failure yields an
                // explicit empty array, not fabricated Ready or full-request coverage.
                match self.bounded_elements(range) {
                    Ok(elements) => elements,
                    Err(error) => { self.reject(error); NSArray::new() }
                }
            } else {
                // SAFETY: Pass through once with the original ABI, without copying or enumerating the original array.
                unsafe { msg_send![super(self), textElementsForRange: range] }
            };
            self.emit("elements_return", 0, true);
            // The sole method_id tail follows the nonnew autorelease-return convention;
            // no additional array retain is stored here.
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
                // Fixed chunks test access only, delivering at most one actual element per
                // enumeration. objc2 catches Objective-C exceptions only; Rust panic still unwinds.
                // No state borrow crosses the callback. AssertUnwindSafe covers this synchronous
                // diagnostic boundary; existing RAII still returns phase/cache leases.
                match objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
                    self.bounded_enumeration(location, options, block)
                })) {
                    Ok(Ok(end)) => end,
                    Ok(Err(error)) => { self.reject(error); None }
                    Err(exception) => self.report_and_rethrow(exception),
                }
            } else {
                // SAFETY: Lend the original NS_NOESCAPE block directly to super, without wrapping,
                // copying, storing, or altering nil/direction semantics.
                unsafe { msg_send![super(self), enumerateTextElementsFromLocation: location, options: options, usingBlock: block] }
            };
            self.emit("enumeration_return", options.bits(), end.is_some());
            // Transfer the optional location through the original nonnew method_id tail;
            // retain no Native position cache.
            end
        }
    }
);

impl NativeMarkdownContentStorageProbe {
    /// Without explicit opt-in, construct the standard reference class. When enabled, the
    /// original surface field still owns this same actual content storage.
    pub(super) fn new_if_requested(
        storage: &Retained<PieceBackedMarkdownTextStorage>,
        ledger: &Rc<NativeMarkdownOwnedLedger>,
        mtm: MainThreadMarker,
    ) -> Result<Retained<NSTextContentStorage>, String> {
        let configured = std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_PROBE");
        let bounded = configured.as_deref() == Some(std::ffi::OsStr::new("bounded"));
        let virtual_paragraph =
            configured.as_deref() == Some(std::ffi::OsStr::new("virtual-paragraph"));
        // Whole physical-paragraph experiments require the additional explicit synthetic gate;
        // this provider is no general replacement for user documents or production pagination.
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
            return Ok(NSTextContentStorage::new()); // Ordinary reference construction allocates no probe counters or replacement system class.
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
        // SAFETY: Scalar state is initialized. Invoke standard NSTextContentStorage init once,
        // without constructing a second backing store.
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
        // into_super moves the one +1; manager/observer/view still consume the original public base type.
        Ok(content.into_super())
    }

    fn emit(&self, stage: &'static str, options: usize, present: bool) {
        let emitted = self.ivars().emitted.get();
        if emitted >= CONTENT_PROBE_LOG_LIMIT {
            return; // Keep the existing finite scalar log budget; never emit Native addresses, Source text, or input contents.
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
        // Diagnose only the exception already caught by this synthetic provider operation,
        // then rethrow that same outcome. No failure becomes a successful fallback paragraph.
        let trace = &self.ivars().trace;
        let synthetic_detail = std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_SYNTHETIC_EXCEPTION")
            .as_deref()
            == Some(std::ffi::OsStr::new("1"));
        if let Some(exception) = exception {
            // Class comes from public runtime metadata. Do not invoke arbitrary exception
            // description/Debug methods that might disclose addresses or document contents.
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
                    break; // Any ObjC object may be thrown; without proven NSException ancestry, send neither name nor reason.
                };
                if exception_class.is_some_and(|expected| std::ptr::eq(candidate, expected)) {
                    is_exception = true;
                    break; // Inspect public ancestry only; infer or invoke no private exception ABI.
                }
                ancestor = candidate.superclass();
            }
            if is_exception {
                // SAFETY: Runtime ancestry proved NSException; public name returns nonnull
                // NSString without mutating the original exception.
                let name: Retained<NSString> = unsafe { msg_send![&exception, name] };
                eprintln!(
                    "content_storage_probe stage=objc_exception_name name={} diagnostic_formal=false",
                    fixed_exception_name(&name),
                );
                if synthetic_detail {
                    // SAFETY: The same verified NSException's public reason may be nil. Read this
                    // diagnostic text only under the explicit synthetic-fixture gate.
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
            // objc2 throw consumes the original caught +1, without replacing the exception,
            // resuming provider work, or changing functional/release requirements.
            objc2::exception::throw(exception)
        } else {
            eprintln!(
                "content_storage_probe stage=objc_exception class=nil name=nil last_native_phase={} last_native_returned={} native_calls={} diagnostic_formal=false",
                trace.last_phase.get(),
                trace.last_returned.get(),
                trace.calls.get(),
            );
            // SAFETY: The catch helper explicitly represents thrown nil as None. Preserve that
            // modern Apple runtime outcome instead of manufacturing another exception object.
            unsafe { objc2::ffi::objc_exception_throw(std::ptr::null_mut()) }
        }
    }

    fn reject(&self, failure: ProviderFailure) {
        let bit = failure as u16;
        let previous = self.ivars().failures.get();
        if previous & bit == 0 {
            self.ivars().failures.set(previous | bit);
            // Rejection has an independent finite per-cause log slot; early ordinary calls
            // cannot conceal capacity exhaustion or failed location mapping.
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
            return Err(ProviderFailure::Busy); // Do not wrap/replay the block or escape into recursively expanding superclass enumeration.
        }
        Ok(ProviderGuard(&self.ivars().busy))
    }

    fn capture(
        &self,
    ) -> Result<(ProviderTicket, Retained<ProtocolObject<dyn NSTextLocation>>), ProviderFailure>
    {
        // Capture original weak document identity and complete derived-state scalars before
        // querying the installed Native graph; release the document borrow before those calls.
        let storage = self
            .ivars()
            .trace
            .call("capture.weak_storage", || self.ivars().storage.load())
            .ok_or(ProviderFailure::Source)?;
        if !std::rc::Weak::ptr_eq(&self.ivars().document, &storage.ivars().document.borrow()) {
            return Err(ProviderFailure::Source); // The same adapter rebound to a successor note cannot supply this provider's Source.
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
            return Err(ProviderFailure::Graph); // The graph must still observe the one original piece storage.
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
            return Err(ProviderFailure::Graph); // Equal numeric positions cannot admit elements for another manager.
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(self.as_super());
        // Use public global-position APIs only, without superclass element enumeration or
        // paragraph construction merely to discover document coordinates.
        let document_range =
            ticket.native("capture.document_range", || provider.documentRange())?;
        let origin = ticket.native("capture.origin", || document_range.location())?;
        let end = ticket.native("capture.end", || document_range.endLocation())?;
        let length = ticket.native("capture.length", || {
            self.offsetFromLocation_toLocation(&origin, &end)
        })?;
        if usize::try_from(length).ok() != Some(key.length) {
            return Err(ProviderFailure::Location); // Global document length cannot be replaced by a current chunk or another Source's extent.
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
            return Err(ProviderFailure::Location); // NSNotFound or foreign-document coordinates cannot be clamped into a valid index.
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
            return Err(ProviderFailure::Location); // Every published endpoint must roundtrip exactly through the same original origin.
        }
        Ok(location)
    }

    fn include_owned(
        &self,
        ticket: &ProviderTicket,
        role: &'static str,
        object: &AnyObject,
    ) -> Result<(), ProviderFailure> {
        // Actual owned Native objects must enter the original ledger before publication; its
        // weak identity alone does not retain the editor or authorize unregistered resources.
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
        // This bounded reference mode allocates one finite UTF-16 copy. Its original live string
        // may fill a failed Core copy with 0xfffd; ticket/length checks do not add a post-read
        // Source-health receipt, so this probe alone cannot certify successful Source I/O.
        if source.is_empty()
            || source.end > ticket.key.length
            || source.len() > CONTENT_PROBE_UTF16_LIMIT
        {
            return Err(ProviderFailure::Range); // Every actual bulk request retains the same existing 4096-unit limit.
        }
        if source.start == 0 && source.end == ticket.key.length {
            return Err(ProviderFailure::WholeRead); // Even a small document cannot bypass this prototype's explicit whole-read rejection.
        }
        let string = ticket.native("read.string", || ticket.storage.as_super().string())?;
        if ticket.native("read.length", || string.length())? != ticket.key.length {
            return Err(ProviderFailure::Graph); // Read the original virtual NSString, not a different text mirror.
        }
        let mut units = vec![0u16; source.len()];
        ticket.native("read.get_characters", || {
            // SAFETY: The finite Vec exactly fits the validated range. Invoke the original
            // adapter primitive, preserving its actual read-request diagnostics and error semantics.
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
            return Ok(entry.source.clone()); // Reuse only coordinates of an element published under this same ticket, without another text copy.
        }
        let grid = {
            let document = ticket.document.upgrade().ok_or(ProviderFailure::Source)?;
            let document = document.borrow();
            finite_source_grid(ticket.key.length, offset, |point| {
                if document.byte_offset_for_utf16(point).is_err() {
                    false // Neither a surrogate midpoint nor an out-of-bounds position is a valid grid boundary.
                } else if point == 0 || point == ticket.key.length {
                    true // Actual scalar-verified document endpoints need no read of nonexistent neighboring units.
                } else {
                    match (
                        document.utf16_code_unit_at(point - 1),
                        document.utf16_code_unit_at(point),
                    ) {
                        (Ok(13), Ok(10)) => false, // A CRLF midpoint is not an admitted experimental paragraph boundary.
                        (Ok(_), Ok(_)) => true, // Both actual neighboring units were read successfully and are not CRLF.
                        (Err(_), _) | (_, Err(_)) => false, // Read failure is not silently converted into a valid boundary.
                    }
                }
            })
            .ok_or(ProviderFailure::Range)?
        }; // End the canonical borrow before actual Native reading; retain only finite boundary metadata.
        let units = self.read_units(ticket, &grid)?;
        segment_in_grid(&units, grid.start, offset).ok_or(ProviderFailure::Range)
    }

    fn build_paragraph(
        &self,
        ticket: &ProviderTicket,
        origin: &ProtocolObject<dyn NSTextLocation>,
        source: &Range<usize>,
    ) -> Result<Retained<NSTextParagraph>, ProviderFailure> {
        // Construct actual finite strings/attributed containers. This prototype deliberately
        // copies the bounded chunk and does not establish physical-paragraph or editing parity.
        let mut units = self.read_units(ticket, source)?;
        let allocated_string = ticket.native("string.alloc", NSMutableString::alloc)?;
        let string = ticket.native("string.init", || {
            // SAFETY: Temporary mutable string copies finite UTF-16 rather than borrowing the
            // Vec; avoiding a tagged short NSString keeps actual native release observable.
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
                // SAFETY: Stack range output remains live through the synchronous getter;
                // attributes still come from original piece storage, without another model.
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
                return Err(ProviderFailure::Attributes); // Empty/noncovering runs cannot be patched with default style and called successful coverage.
            }
            let end = end.min(source.end);
            let local = NSRange::new(cursor - source.start, end - cursor);
            ticket.native("attributes.add", || {
                // SAFETY: Dictionary comes from the correctly typed original attribute
                // primitive, and local lies wholly inside the finite builder's text range.
                unsafe { builder.addAttributes_range(&attributes, local) };
            })?;
            cursor = end; // Each turn advances at least one unit, with at most source.len turns and no whole-document scan.
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
        // Exact finite string/range length is only this prototype's admission fact, not proof
        // of soft-line, UBA, or IME equivalence across chunks.
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
        // Cache only elements issued under the same complete ticket. Bounded chunks and one
        // synthetic full physical paragraph are different experiments, not interchangeable layouts.
        if ticket.key.length == 0 || (!reverse && offset == ticket.key.length) {
            return Ok(None); // With no Source character, create neither an EOF paragraph nor another visual line.
        }
        let mut source = if self.ivars().virtual_paragraph {
            0..ticket.key.length // The synthetic physical paragraph covers actual 0..EOF; no 4096-unit grid invents paragraph boundaries.
        } else {
            self.source_for_offset(ticket, cache, offset.min(ticket.key.length - 1))?
        };
        if reverse && offset < ticket.key.length {
            if source.start == 0 {
                return Ok(None); // Reverse enumeration starts before the element containing the supplied location.
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
                .map(Some); // Preserve the same element identity with one temporary +1 only.
        }
        if cache.entries.len() == CONTENT_PROBE_CACHE_LIMIT {
            return Err(ProviderFailure::Capacity); // Do not evict published paragraphs while active; weak fragment relationships must remain intact.
        }
        let paragraph = if self.ivars().virtual_paragraph {
            virtual_paragraph::build(self, ticket, origin)? // Construct weak-bound read-only virtual values, without copying text or attribute runs here.
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
        // Nullable public ranges/endpoints remain explicit absence. Observations cannot repair
        // missing Native coordinates with this request's source interval or document origin.
        let mut facts = ParagraphRangeFacts {
            present: range.is_some(),
            start_present: false,
            end_present: false,
            start: None,
            end: None,
            roundtrip: false,
        };
        let Some(range) = range else {
            return Ok(facts); // Record an absent nullable subrange without manufactured endpoints or passing nil to offset.
        };
        let start: Option<Retained<ProtocolObject<dyn NSTextLocation>>> =
            ticket.native("verify.range_start", || {
                // SAFETY: Public location getter with the same object ABI; Option explicitly
                // observes an actual nil that would violate its declared nonnull contract.
                unsafe { msg_send![range, location] }
            })?;
        let end: Option<Retained<ProtocolObject<dyn NSTextLocation>>> =
            ticket.native("verify.range_end", || {
                // SAFETY: Public endLocation on this same NSTextRange; nil is absence only and
                // never enters coordinate arithmetic.
                unsafe { msg_send![range, endLocation] }
            })?;
        facts.start_present = start.is_some();
        facts.end_present = end.is_some();
        let (Some(start), Some(end)) = (start, end) else {
            return Ok(facts); // Missing actual endpoints cannot be replaced by document origin or requested Source coordinates.
        };
        for (location, offset) in [(&start, &mut facts.start), (&end, &mut facts.end)] {
            match self.offset(ticket, origin, location) {
                Ok(actual) => *offset = Some(actual),
                Err(ProviderFailure::Location) => {} // Unmappable coordinates remain None, not a valid interval.
                Err(error) => return Err(error), // Original-ticket revocation ends observation before reading successor state.
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
            facts.roundtrip = start_equal && end_equal; // Positions rebuilt by the same provider must compare equal to actual original endpoints.
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
        // Before invoking the caller's block, require the original manager and the element's
        // actual complete range. Content/separator ranges are separately observed nullable facts.
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
            // Four initial scalar observations distinguish construction from later cache hits;
            // emit no object addresses, attribute values, or text.
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
            return Err(ProviderFailure::Range); // A Native-revoked element cannot be repaired/rebuilt and reused for the original callback.
        }
        // Observe nullable paragraph subranges only; do not replace unknown ranges with Source
        // coordinates or alter the existing setter sequence to obtain a desired diagnostic result.
        range.ok_or(ProviderFailure::Range)
    }

    fn bounded_elements(
        &self,
        requested: &NSTextRange,
    ) -> Result<Retained<NSArray<NSTextElement>>, ProviderFailure> {
        // The local phase and real cache lease reject reentry without another graph or fallback.
        // A single available element is only partial protocol delivery, never a complete layout receipt.
        let _phase = self.enter()?;
        let (ticket, origin) = self.capture()?;
        let location = ticket.native("elements.start", || requested.location())?;
        let end = ticket.native("elements.end", || requested.endLocation())?;
        let offset = self.offset(&ticket, &origin, &location)?;
        if self.offset(&ticket, &origin, &end)? < offset {
            return Err(ProviderFailure::Location); // A reversed/foreign range cannot be transformed into another valid request.
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
            Ok(elements) // Return only the synchronously available intersecting prefix, without claiming full requested coverage.
        } else {
            Ok(NSArray::new()) // An uncached standard empty array proves no completed layout.
        }
    }

    fn bounded_enumeration(
        &self,
        location: Option<&ProtocolObject<dyn NSTextLocation>>,
        options: NSTextContentManagerEnumerationOptions,
        block: &block2::DynBlock<dyn Fn(NonNull<NSTextElement>) -> Bool + '_>,
    ) -> Result<Option<Retained<ProtocolObject<dyn NSTextLocation>>>, ProviderFailure> {
        // Preserve actual forward/reverse/nil-location semantics, but deliver at most one
        // admitted element. Cache ownership leaves no RefCell borrow across Native callbacks.
        let _phase = self.enter()?;
        let (ticket, origin) = self.capture()?;
        let reverse = options.contains(NSTextContentManagerEnumerationOptions::Reverse);
        let offset = match location {
            Some(location) => self.offset(&ticket, &origin, location)?, // Preserve the original public location, without guessing a nearby paragraph.
            None if reverse => ticket.key.length, // Public nil reverse starts at actual document.end.
            None => 0, // Public nil forward starts at actual document.location.
        };
        let mut lease = match self.ivars().cache.begin() {
            CacheAdmission::Ready(lease) => lease,
            CacheAdmission::Loaned | CacheAdmission::Closed => return Err(ProviderFailure::Busy),
        };
        let Some(paragraph) =
            self.paragraph(&ticket, &origin, offset, reverse, lease.payload_mut())?
        else {
            return Ok(None); // Without an actual element, fabricate no enumeration endpoint.
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
            (index + 1, entry.source.clone()) // Cache-slot number is scalar only; end the payload borrow before Native validation.
        };
        let range = self.validate_paragraph_before_callback(
            &ticket, &origin, &paragraph, &source, cache_slot,
        )?;
        let edge = if reverse {
            ticket.native("enumeration.reverse_edge", || range.location())? // Reverse returns the actual enumerated element's leading edge.
        } else {
            ticket.native("enumeration.forward_edge", || range.endLocation())? // Forward returns the actual enumerated element's trailing edge.
        };
        let element: &NSTextElement = &paragraph;
        let _continue_requested = ticket.native("enumeration.original_block", || {
            block.call((NonNull::from(element),))
        })?;
        // Deliver one element only. The original block ran unchanged; true requests continuation
        // but does not force unbounded enumeration or eviction of already published cache entries.
        Ok(Some(edge))
    }
}

/// Observe only when both explicit gates match; acquire no Native object or text owner here.
pub(super) fn receiver_observation_requested() -> bool {
    std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_PROBE").as_deref()
        == Some(std::ffi::OsStr::new("virtual-paragraph"))
        && std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_SYNTHETIC_EXCEPTION").as_deref()
            == Some(std::ffi::OsStr::new("1"))
}

/// Classify public runtime class identity only, without forced registration or object-data getters.
pub(super) fn virtual_attributed_receiver_is_ours(value: &NSAttributedString) -> bool {
    virtual_paragraph::virtual_attributed_receiver_is_ours(value)
}

/// Ordinary logs emit fixed public exception-name labels, without formatting arbitrary names or objects.
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
            return label; // Emit the matched fixed label only, never arbitrary exception-name text.
        }
    }
    "other_nsexception" // Preserve unrecognized original exceptions instead of relabeling them as a known contract error.
}

/// Explicit synthetic fixtures only: copy at most 1024 UTF-16 units of a system exception reason,
/// without touching the document adapter.
fn finite_exception_reason(reason: &NSString) -> String {
    let length = reason.length().min(1024);
    let mut units = [0u16; 1024];
    if length > 0 {
        // SAFETY: Public NSString length bounds this 0..length read inside the actual reason;
        // fixed stack storage covers every requested unit.
        unsafe {
            reason.getCharacters_range(
                NonNull::new(units.as_mut_ptr()).expect("固定非空诊断缓冲"),
                NSRange::new(0, length),
            )
        };
    }
    String::from_utf16_lossy(&units[..length]) // A truncated surrogate affects synthetic diagnostic display only, never original exception or Source data.
}

/// Fixed Source grid is attribution only. Reserve one stride unit for scalar/CRLF adjustment;
/// the actual hard limit remains the existing 4096 units.
fn finite_source_grid(
    length: usize,
    offset: usize,
    mut boundary: impl FnMut(usize) -> bool,
) -> Option<Range<usize>> {
    if offset >= length {
        return None; // EOF contains no Source character; infer no replacement request from the tail.
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
        bucket = bucket.checked_add(1)?; // An offset at a grid's backward-adjusted end permits checking only the next bucket.
    }
    None // Reject unverified boundaries rather than falling back to an over-budget or whole physical paragraph.
}

/// Within a grid, split at actual CR/LF/PS so a value containing several physical paragraphs
/// does not impersonate one NSTextParagraph. Grid edges still remain experimental artificial cuts.
fn segment_in_grid(units: &[u16], origin: usize, offset: usize) -> Option<Range<usize>> {
    let mut start = 0;
    let mut index = 0;
    while index < units.len() {
        let separator = matches!(units[index], 10 | 13 | 0x2029);
        index += 1;
        if separator && units[index - 1] == 13 && units.get(index) == Some(&10) {
            index += 1; // Actual CRLF contributes one boundary, without an invented standalone LF paragraph.
        }
        if separator || index == units.len() {
            if origin + start <= offset && offset < origin + index {
                return Some(origin + start..origin + index);
            }
            start = index; // Advance only at actual separators or the finite grid edge, without inserting Source characters.
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_grid_covers_shifted_scalar_boundary_without_exceeding_limit() {
        // Pure range arithmetic uses this existing injected boundary predicate. It checks grid
        // adjustment only, not real Source reading, paragraph parity, or Native fragment behavior.
        let boundary = |point| point != 4095;
        assert_eq!(finite_source_grid(20_000, 4094, boundary), Some(4094..8190));
        assert_eq!(finite_source_grid(20_000, 4095, boundary), Some(4094..8190));
        assert_eq!(finite_source_grid(20_000, 20_000, boundary), None);
    }

    #[test]
    fn finite_segments_preserve_real_crlf_and_global_coordinates() {
        // Independent finite fixture units verify CRLF/line boundaries and original global
        // offsets; no extra paragraph or source character may be manufactured.
        let units: Vec<_> = "a\r\n中\n末".encode_utf16().collect();
        assert_eq!(segment_in_grid(&units, 100, 102), Some(100..103));
        assert_eq!(segment_in_grid(&units, 100, 103), Some(103..105));
        assert_eq!(segment_in_grid(&units, 100, 105), Some(105..106));
        assert_eq!(segment_in_grid(&units, 100, 106), None);
    }
}
