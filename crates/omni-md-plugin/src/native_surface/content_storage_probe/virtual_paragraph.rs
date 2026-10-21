//! Synthetic read-only single-paragraph experiment only. Preserve complete physical-paragraph
//! coordinates and query original adapter attributes through public primitives. Whole bulk is
//! still forwarded; this is not the default editor's bounded viewport implementation.

use super::*;
use objc2::Message;
use objc2::runtime::NSZone;

mod slice;
mod string_slice;
use slice::Utf16Slice;
use string_slice::SyntheticVirtualStringSlice;

const SYNTHETIC_GIANT_BYTES: usize = 345_112;
const SYNTHETIC_GIANT_UTF16: usize = 165_042;

/// Original ticket binding retains weak identities and scalars only. Native value lifetime does
/// not extend the canonical document or piece root lifetime.
struct VirtualBinding {
    storage: Weak<PieceBackedMarkdownTextStorage>,
    document: WeakMarkdownDocument,
    key: ContentKey,
    trace: Rc<ProviderPhaseTrace>,
    ledger: std::rc::Weak<NativeMarkdownOwnedLedger>,
    counts: VirtualCounts,
    attributed_substring_logs: Cell<u8>,
    string_substring_logs: Cell<u8>,
}

impl VirtualBinding {
    fn ticket(&self) -> ProviderTicket {
        // Native string values remain valid only for this original read-only experiment state,
        // unlike independent frozen snapshots. Closing or editing explicitly invalidates them.
        let Some(storage) = self.storage.load() else {
            throw_virtual_contract(); // A closed original adapter cannot be replaced by successor text or a fabricated empty value.
        };
        let ticket = ProviderTicket {
            storage,
            document: self.document.clone(),
            key: self.key,
            trace: Rc::clone(&self.trace),
        };
        if !ticket.current() {
            throw_virtual_contract(); // This experiment promises only its original read-only ticket; edits/mode changes fail without rewriting the old value.
        }
        ticket
    }

    fn include_owned(
        &self,
        ticket: &ProviderTicket,
        role: &'static str,
        object: &AnyObject,
    ) -> Result<(), ProviderFailure> {
        let ledger = self.ledger.upgrade().ok_or(ProviderFailure::Ledger)?;
        ticket
            .native("virtual.owned_include", || ledger.include(role, object))?
            .map_err(|_| ProviderFailure::Ledger) // Every new slice enters the original ledger; small-object status does not bypass release observation.
    }

    fn log_substring(&self, attributed: bool, requested: NSRange, actual: Utf16Slice) {
        let logs = if attributed {
            &self.attributed_substring_logs
        } else {
            &self.string_substring_logs
        };
        let count = logs.get();
        if count < 4 {
            logs.set(count + 1);
            // All slices of one original ticket share four log entries per substring kind;
            // new object construction must not reset diagnostic emission budgets.
            eprintln!(
                "content_storage_probe stage=virtual_substring kind={} location={} length={} global_location={} global_length={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
                if attributed { "attributed" } else { "string" },
                requested.location,
                requested.length,
                actual.start,
                actual.length,
            );
        }
    }

    fn checked<T>(&self, phase: &'static str, operation: impl FnOnce() -> T) -> T {
        let ticket = self.ticket();
        match ticket.native(phase, operation) {
            Ok(value) => value,
            Err(_) => throw_virtual_contract(), // Revocation during getter reentry cannot publish successor text or shorten original length.
        }
    }
}

#[derive(Default)]
struct VirtualCounts {
    copy_calls: Cell<u64>,
    full_substrings: Cell<u64>,
    partial_substrings: Cell<u64>,
    attribute_reads: Cell<u64>,
    substring_entries: Cell<u64>,
    string_copy_entries: Cell<u8>,
    longest_calls: Cell<u64>,
    longest_null: Cell<u64>,
    longest_nonnull: Cell<u64>,
}

impl Drop for VirtualBinding {
    fn drop(&mut self) {
        // Aggregate derived values once per original binding. Original string diagnostics still
        // observe actual bulk requests; shared-value counts prove no framework-wide absence of copies.
        eprintln!(
            "content_storage_probe stage=virtual_attributed_drop copy_calls={} full_substring_shares={} partial_substrings={} attribute_reads={} substring_entries={} longest_calls={} longest_null={} longest_nonnull={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
            self.counts.copy_calls.get(),
            self.counts.full_substrings.get(),
            self.counts.partial_substrings.get(),
            self.counts.attribute_reads.get(),
            self.counts.substring_entries.get(),
            self.counts.longest_calls.get(),
            self.counts.longest_null.get(),
            self.counts.longest_nonnull.get(),
        );
    }
}

pub(super) struct VirtualAttributedIvars {
    binding: Rc<VirtualBinding>,
    source_string: Retained<NSString>,
    view_string: Retained<NSString>,
    slice: Utf16Slice,
}

define_class!(
    // SAFETY: Implements the two public NSAttributedString primitives, returning actual slices
    // within the original read-only ticket without retaining a text mirror.
    #[unsafe(super = NSAttributedString)]
    #[thread_kind = MainThreadOnly]
    #[ivars = VirtualAttributedIvars]
    struct SyntheticVirtualAttributedString;

    // SAFETY: No extra NSObject obligations; this value and all Native access stay on the original main-thread graph.
    unsafe impl NSObjectProtocol for SyntheticVirtualAttributedString {}

    impl SyntheticVirtualAttributedString {
        #[unsafe(method_id(string))]
        #[unsafe(method_family = none)]
        fn virtual_string(&self) -> Retained<NSString> {
            self.ivars().binding.checked("virtual.attr_string", || {
                self.ivars().view_string.clone() // Full/partial immutable views both retain exact ranges over the one original Source string.
            })
        }

        #[unsafe(method(length))]
        fn virtual_length(&self) -> NSUInteger {
            let _ticket = self.ivars().binding.ticket();
            self.ivars().slice.length // Exact original value/subrange UTF-16 length, without a viewport or fabricated full-range substitute.
        }

        #[unsafe(method_id(attributesAtIndex:effectiveRange:))]
        #[unsafe(method_family = none)]
        /// # Safety
        /// effective_range must be null or writable for one NSRange during this synchronous call.
        unsafe fn virtual_attributes(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
        ) -> Retained<MarkdownTextAttributes> {
            let ticket = self.ivars().binding.ticket();
            let global_index = self.ivars().slice.index(index).unwrap_or_else(|| throw_virtual_range());
            let reads = self.ivars().binding.counts.attribute_reads.get().saturating_add(1);
            self.ivars().binding.counts.attribute_reads.set(reads);
            if reads <= 262_144 && reads.is_power_of_two() {
                // Actual slices share cumulative accounting and the finite existing power-of-two
                // log schedule; observing counts adds no attribute query.
                eprintln!(
                    "content_storage_probe stage=virtual_attribute_reads attribute_reads={} original_key_length={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
                    reads,
                    ticket.key.length,
                );
            }
            let mut global_effective = NSRange::new(0, 0);
            let output: *mut NSRange = if effective_range.is_null() {
                std::ptr::null_mut() // When caller needs no range, pass null to the original getter too; request no extra metadata.
            } else {
                &mut global_effective
            };
            let attributes = match ticket.native("virtual.attr_attributes", || {
                // SAFETY: global_index belongs to original storage; output is null or a stack
                // range alive through this synchronous return, with no borrowed state crossing Native.
                unsafe { ticket.storage.as_super().attributesAtIndex_effectiveRange(global_index, output) }
            }) {
                Ok(attributes) => attributes,
                Err(_) => throw_virtual_contract(), // Revocation cannot publish mixed original/successor font or color attributes.
            };
            if !effective_range.is_null() {
                let (location, length) = self.ivars().slice.local_effective_range(
                    index, global_effective.location, global_effective.length, ticket.key.length,
                ).unwrap_or_else(|| throw_virtual_contract());
                // SAFETY: Caller supplies writable NSRange. Intersect the actual global run with
                // this slice, then convert to local coordinates still covering the requested index.
                unsafe { effective_range.write(NSRange::new(location, length)) };
            }
            attributes // The sole noncopy method_id tail transfers this dictionary without an extra retained claim.
        }

        #[unsafe(method_id(attributesAtIndex:longestEffectiveRange:inRange:))]
        #[unsafe(method_family = none)]
        /// # Safety
        /// effective_range must be null or writable for one NSRange during this synchronous call.
        unsafe fn virtual_longest_attributes(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
            range_limit: NSRange,
        ) -> Retained<MarkdownTextAttributes> {
            let _ticket = self.ivars().binding.ticket();
            if self.ivars().slice.index(index).is_none()
                || self.ivars().slice.subrange(range_limit.location, range_limit.length).is_none()
            {
                throw_virtual_range(); // Null skips longest-range calculation, not the public bounds exceptions for index/rangeLimit.
            }
            let counts = &self.ivars().binding.counts;
            let calls = counts.longest_calls.get().saturating_add(1);
            counts.longest_calls.set(calls);
            if effective_range.is_null() {
                counts.longest_null.set(counts.longest_null.get().saturating_add(1));
            } else {
                counts.longest_nonnull.set(counts.longest_nonnull.get().saturating_add(1));
            }
            if calls <= 4 || (calls <= 262_144 && calls.is_power_of_two()) {
                // All slices share the original finite initial/power-of-two log budget, observing
                // null-output frequency without issuing another attribute request.
                eprintln!(
                    "content_storage_probe stage=virtual_longest_attributes calls={} null={} nonnull={} index={} limit_location={} limit_length={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
                    calls, counts.longest_null.get(), counts.longest_nonnull.get(),
                    index, range_limit.location, range_limit.length,
                );
            }
            if effective_range.is_null() {
                // With no longest-range output, only actual index determines attributes; no
                // adjacent-run scan is requested. SAFETY: Index is validated and null is allowed
                // by the public primitive, which still performs complete original-ticket checks.
                unsafe { self.as_super().attributesAtIndex_effectiveRange(index, std::ptr::null_mut()) }
            } else {
                // SAFETY: Forward original index/rangeLimit and caller's writable output once to
                // super. Preserve actual longest equal-value range, rather than substituting a
                // bounded canonical window or dictionary identity for Foundation's semantics.
                self.ivars().binding.checked("virtual.attr_longest", || unsafe {
                    msg_send![super(self), attributesAtIndex: index, longestEffectiveRange: effective_range, inRange: range_limit]
                })
            } // Both branches share the noncopy method_id return, with no additional retained result.
        }

        #[unsafe(method_id(copyWithZone:))]
        #[unsafe(method_family = copy)]
        fn immutable_copy(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.ivars().binding.checked("virtual.attr_copy", || {
                let counts = &self.ivars().binding.counts;
                counts.copy_calls.set(counts.copy_calls.get().saturating_add(1));
                self.retain() // Copy convention transfers this one +1; this read-only value itself copies no characters.
            })
        }

        #[unsafe(method_id(attributedSubstringFromRange:))]
        #[unsafe(method_family = none)]
        fn virtual_substring(&self, range: NSRange) -> Retained<NSAttributedString> {
            let entries = self.ivars().binding.counts.substring_entries.get().saturating_add(1);
            self.ivars().binding.counts.substring_entries.set(entries);
            if entries <= 4 && super::receiver_observation_requested() {
                // Entry observation precedes the first ticket/Native Weak load, distinguishing
                // an override never invoked from one entered but not yet returned.
                eprintln!(
                    "content_storage_probe stage=virtual_substring_entry call={} receiver=virtual_attributed location={} length={} synthetic_only=true diagnostic_formal=false",
                    entries, range.location, range.length,
                );
            }
            self.requested_substring(range) // Actual partial ranges return independent virtual values, without an initial superclass full copy.
        }
    }
);

impl SyntheticVirtualAttributedString {
    fn new(
        provider: &NativeMarkdownContentStorageProbe,
        ticket: &ProviderTicket,
        binding: Rc<VirtualBinding>,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>, ProviderFailure> {
        // The retained original live NSString is only an adapter with a weak document link;
        // retaining this complete-range view does not itself freeze or duplicate canonical text.
        let source_string = ticket.native("virtual.original_string", || {
            ticket.storage.as_super().string()
        })?;
        let slice = Utf16Slice::new(0, ticket.key.length, ticket.key.length)
            .ok_or(ProviderFailure::Range)?;
        let view_source = ticket.native("virtual.slice_source_retain", || source_string.clone())?;
        // The complete physical paragraph uses the same immutable NSString view with actual
        // 0..EOF coordinates; neither paragraph nor bulk requests are shortened. Its existing
        // factory registers the native view and preserves original-ticket/read diagnostics.
        let view_string =
            SyntheticVirtualStringSlice::create(Rc::clone(&binding), view_source, slice, mtm)?;
        let allocated = ticket.native("virtual.attr_alloc", || {
            Self::alloc(mtm).set_ivars(VirtualAttributedIvars {
                binding,
                source_string,
                view_string,
                slice,
            })
        })?;
        // SAFETY: Required custom NSAttributedString primitives and ivars are ready. Superclass
        // init receives no complete document-text value.
        let value: Retained<Self> = ticket.native("virtual.attr_init", || unsafe {
            msg_send![super(allocated), init]
        })?;
        if !std::ptr::eq(value.class(), Self::class()) {
            return Err(ProviderFailure::Graph); // A class-cluster replacement permits no access to another class's Rust ivars.
        }
        provider.include_owned(ticket, "editor.virtual_paragraph.attributed", &value)?;
        Ok(value)
    }

    fn new_slice(
        binding: Rc<VirtualBinding>,
        source_string: Retained<NSString>,
        slice: Utf16Slice,
        mtm: MainThreadMarker,
    ) -> Result<Retained<NSAttributedString>, ProviderFailure> {
        // Derived values retain the original Source string and binding directly, not their
        // parent substring. The same exact global range is validated before Native publication.
        let ticket = binding.ticket();
        let source = ticket.native("virtual.slice_source_retain", || source_string.clone())?;
        let view_string =
            SyntheticVirtualStringSlice::create(Rc::clone(&binding), source, slice, mtm)?;
        let allocated = ticket.native("virtual.attr_slice_alloc", || {
            Self::alloc(mtm).set_ivars(VirtualAttributedIvars {
                binding: Rc::clone(&binding),
                source_string,
                view_string,
                slice,
            })
        })?;
        // SAFETY: Both public primitives and actual slice are initialized; init scans or copies
        // no original Source text.
        let value: Retained<Self> = ticket.native("virtual.attr_slice_init", || unsafe {
            msg_send![super(allocated), init]
        })?;
        if !std::ptr::eq(value.class(), Self::class()) {
            return Err(ProviderFailure::Graph); // An unexpected class cannot continue serving this virtual attributed value.
        }
        binding.include_owned(&ticket, "editor.virtual_paragraph.attributed_slice", &value)?;
        Ok(value.into_super())
    }

    fn requested_substring(&self, range: NSRange) -> Retained<NSAttributedString> {
        // Receiver-local bounds constrain this request independently of original Source length.
        // Only the exact complete receiver may share self; partial ranges own new virtual views.
        let ticket = self.ivars().binding.ticket();
        let actual = self
            .ivars()
            .slice
            .subrange(range.location, range.length)
            .unwrap_or_else(|| throw_virtual_range());
        self.ivars().binding.log_substring(true, range, actual);
        if actual == self.ivars().slice {
            self.ivars().binding.checked("virtual.attr_full_substring", || {
                let counts = &self.ivars().binding.counts;
                let count = counts.full_substrings.get().saturating_add(1);
                counts.full_substrings.set(count);
                if count == 1 {
                    eprintln!(
                        "content_storage_probe stage=virtual_full_substring_shared count=1 source_start={} source_end={} synthetic_only=true read_only=true parity=false diagnostic_formal=false bulk_result=unmeasured",
                        actual.start, actual.end(),
                    );
                }
                self.retain().into_super() // Share only this exact complete receiver; method_id handles the noncopy return convention.
            })
        } else {
            let counts = &self.ivars().binding.counts;
            counts
                .partial_substrings
                .set(counts.partial_substrings.get().saturating_add(1));
            let source = ticket
                .native("virtual.slice_source_retain", || {
                    self.ivars().source_string.clone()
                })
                .unwrap_or_else(|_| throw_virtual_contract());
            let mtm = MainThreadMarker::new().unwrap_or_else(|| throw_virtual_contract());
            Self::new_slice(Rc::clone(&self.ivars().binding), source, actual, mtm)
                .unwrap_or_else(|_| throw_virtual_contract()) // Original Source plus global offset/length, without text copy or retained parent-slice chains.
        }
    }
}

pub(super) struct VirtualParagraphIvars {
    binding: Rc<VirtualBinding>,
    value: Retained<SyntheticVirtualAttributedString>,
    content: Retained<NSTextRange>,
    separator: Retained<NSTextRange>,
}

define_class!(
    // SAFETY: The complete actual single paragraph uses immutable attributed views and original
    // provider global ranges, without private setters or inserted separator characters.
    #[unsafe(super = NSTextParagraph)]
    #[thread_kind = MainThreadOnly]
    #[ivars = VirtualParagraphIvars]
    struct SyntheticVirtualPhysicalParagraph;

    // SAFETY: No extra NSObject obligations. Manager remains the inherited weak property, without a strong provider cycle.
    unsafe impl NSObjectProtocol for SyntheticVirtualPhysicalParagraph {}

    impl SyntheticVirtualPhysicalParagraph {
        #[unsafe(method_id(attributedString))]
        #[unsafe(method_family = none)]
        fn virtual_value(&self) -> Retained<NSAttributedString> {
            self.ivars().binding.checked("virtual.paragraph_value", || {
                self.ivars().value.clone().into_super() // Retain the same value, without copying through NSConcreteAttributedString here.
            })
        }

        #[unsafe(method_id(paragraphContentRange))]
        #[unsafe(method_family = none)]
        fn virtual_content_range(&self) -> Option<Retained<NSTextRange>> {
            Some(self.ivars().binding.checked("virtual.paragraph_content", || {
                self.ivars().content.clone() // This read-only fixture has no paragraph separator; content covers actual 0..EOF.
            }))
        }

        #[unsafe(method_id(paragraphSeparatorRange))]
        #[unsafe(method_family = none)]
        fn virtual_separator_range(&self) -> Option<Retained<NSTextRange>> {
            Some(self.ivars().binding.checked("virtual.paragraph_separator", || {
                self.ivars().separator.clone() // Actual EOF has no terminator; the empty range still has valid original-provider endpoints.
            }))
        }
    }
);

/// Support only the composition root's declared read-only giant fixture, whose generator owns
/// the no-CR/LF/PS guarantee. A line count of one alone does not prove absence of PS.
pub(super) fn build(
    provider: &NativeMarkdownContentStorageProbe,
    ticket: &ProviderTicket,
    origin: &ProtocolObject<dyn NSTextLocation>,
) -> Result<Retained<NSTextParagraph>, ProviderFailure> {
    if std::env::var_os("OMNI_NATIVE_CONTENT_STORAGE_SYNTHETIC_EXCEPTION").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return Err(ProviderFailure::Source); // Require explicitly admitted synthetic input; never route user notes into this experimental provider.
    }
    let document = ticket.document.upgrade().ok_or(ProviderFailure::Source)?;
    let supported = {
        // Fixed fixture dimensions and unchanged generation/input state select this experiment;
        // these checks do not scan arbitrary source text or validate general paragraph semantics.
        let document = document.borrow();
        document.byte_len() == SYNTHETIC_GIANT_BYTES
            && document.utf16_len() == SYNTHETIC_GIANT_UTF16
            && document.line_count() == 1
            && document.generation() == 0
            && ticket.key.input_depth == 0
            && !ticket.key.composition
    };
    drop(document); // End the borrow and temporary canonical owner before subsequent Native initialization.
    if !supported || !ticket.current() {
        return Err(ProviderFailure::Range); // Neither scan the whole document for substitute evidence nor silently fall back to artificial finite paragraphs.
    }
    let mtm = MainThreadMarker::new().ok_or(ProviderFailure::Graph)?;
    let binding = Rc::new(VirtualBinding {
        storage: Weak::from_retained(&ticket.storage),
        document: ticket.document.clone(),
        key: ticket.key,
        trace: Rc::clone(&ticket.trace),
        ledger: provider.ivars().ledger.clone(),
        counts: VirtualCounts::default(),
        attributed_substring_logs: Cell::new(0),
        string_substring_logs: Cell::new(0),
    });
    let value = SyntheticVirtualAttributedString::new(provider, ticket, Rc::clone(&binding), mtm)?;
    // Content remains the complete physical paragraph; separator is a real zero-width EOF
    // range. These native objects preserve full coordinates even when fragments later request slices.
    let start = provider.location(ticket, origin, 0)?;
    let end = provider.location(ticket, origin, ticket.key.length)?;
    let content = original_range(
        provider,
        ticket,
        &start,
        &end,
        "editor.virtual_paragraph.content",
    )?;
    let separator = original_range(
        provider,
        ticket,
        &end,
        &end,
        "editor.virtual_paragraph.separator",
    )?;
    let allocated = ticket.native("virtual.paragraph_alloc", || {
        SyntheticVirtualPhysicalParagraph::alloc(mtm).set_ivars(VirtualParagraphIvars {
            binding,
            value: value.clone(),
            content: content.clone(),
            separator,
        })
    })?;
    // SAFETY: The attributed value is the initialized immutable public subclass above. Super
    // still invokes its designated initializer once; framework behavior may issue actual bulk reads.
    let paragraph: Retained<SyntheticVirtualPhysicalParagraph> = ticket
        .native("virtual.paragraph_init", || unsafe {
            msg_send![super(allocated), initWithAttributedString: &*value]
        })?;
    if !std::ptr::eq(
        paragraph.class(),
        SyntheticVirtualPhysicalParagraph::class(),
    ) {
        return Err(ProviderFailure::Graph); // Access no Rust ivars on an unexpectedly substituted object.
    }
    provider.include_owned(ticket, "editor.virtual_paragraph.paragraph", &paragraph)?;
    ticket.native("virtual.paragraph_range", || {
        paragraph.setElementRange(Some(&content))
    })?;
    ticket.native("virtual.paragraph_manager", || {
        paragraph.setTextContentManager(Some(provider.as_super().as_super()))
    })?;
    eprintln!(
        "content_storage_probe stage=virtual_physical_paragraph source_start=0 source_end={} synthetic_only=true read_only=true parity=false diagnostic_formal=false whole_bulk_forwarded=true",
        ticket.key.length,
    );
    Ok(paragraph.into_super())
}

fn original_range(
    provider: &NativeMarkdownContentStorageProbe,
    ticket: &ProviderTicket,
    start: &ProtocolObject<dyn NSTextLocation>,
    end: &ProtocolObject<dyn NSTextLocation>,
    role: &'static str,
) -> Result<Retained<NSTextRange>, ProviderFailure> {
    // Preserve actual original-provider endpoints, then register this concrete Native range
    // before any containing paragraph is published to the system.
    let allocated = ticket.native("virtual.range_alloc", NSTextRange::alloc)?;
    let range = ticket
        .native("virtual.range_init", || {
            NSTextRange::initWithLocation_endLocation(allocated, start, Some(end))
        })?
        .ok_or(ProviderFailure::Location)?;
    provider.include_owned(ticket, role, &range)?;
    Ok(range)
}

fn throw_virtual_contract() -> ! {
    let class = objc2::runtime::AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: Public NSException factory with fixed text-free name/reason and nil userInfo,
    // preserving explicit read-only experiment failure instead of successful fallback.
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class,
            exceptionWithName: ns_string!("NSInternalInconsistencyException"),
            reason: ns_string!("Synthetic virtual paragraph read-only ticket is no longer current"),
            userInfo: None::<&AnyObject>]
    };
    objc2::exception::throw(exception) // Revoked original authority fails this experiment; no fabricated empty string or successor data is returned.
}

fn throw_virtual_range() -> ! {
    let class = objc2::runtime::AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: Public NSException factory only. Invalid NSString/NSAttributedString coordinates
    // raise the original range exception rather than being shortened into a valid fragment.
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class,
            exceptionWithName: ns_string!("NSRangeException"),
            reason: ns_string!("Synthetic virtual UTF16 slice request is outside the original value"),
            userInfo: None::<&AnyObject>]
    };
    objc2::exception::throw(exception)
}

/// Inspect no data getter and trigger no class registration; NAME and registered class name
/// come from the same ClassType contract.
pub(super) fn virtual_attributed_receiver_is_ours(value: &NSAttributedString) -> bool {
    value.class().name().to_bytes() == SyntheticVirtualAttributedString::NAME.as_bytes()
}
