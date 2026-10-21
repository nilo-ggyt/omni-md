//! Actual UTF-16 subrange views over public NSString primitives in the synthetic reference
//! experiment. Forward each complete bulk request to the original string and its diagnostics.
//! The original live adapter may fill failed reads with 0xfffd. Ticket checks here validate
//! identity/state, not a post-read Core Source-health receipt; this is no default lazy-render proof.

use super::*;

pub(super) struct VirtualStringSliceIvars {
    binding: Rc<VirtualBinding>,
    source: Retained<NSString>,
    slice: Utf16Slice,
}

define_class!(
    // SAFETY: Implements required NSString length/characterAtIndex: while preserving UTF-16
    // bulk/substring contracts. Stores no text; every character uses the original string's actual global index.
    #[unsafe(super = NSString)]
    #[thread_kind = MainThreadOnly]
    #[ivars = VirtualStringSliceIvars]
    pub(super) struct SyntheticVirtualStringSlice;

    // SAFETY: No additional NSObject obligations; Source access and release stay on the original main-thread graph.
    unsafe impl NSObjectProtocol for SyntheticVirtualStringSlice {}

    impl SyntheticVirtualStringSlice {
        #[unsafe(method(length))]
        fn slice_length(&self) -> NSUInteger {
            let _ticket = self.ivars().binding.ticket();
            self.ivars().slice.length // Actual slice length, neither shortened to a viewport nor relabeled as the entire original Source.
        }

        #[unsafe(method(characterAtIndex:))]
        fn slice_character(&self, index: NSUInteger) -> unichar {
            let global = self.ivars().slice.index(index).unwrap_or_else(|| throw_virtual_range());
            self.ivars().binding.checked("virtual.string_character", || {
                self.ivars().source.characterAtIndex(global) // Preserve the actual unit, including a lone surrogate; do not convert it to Rust char.
            })
        }

        #[unsafe(method(getCharacters:range:))]
        /// # Safety
        /// destination must be writable for range.length UTF-16 units during this synchronous call.
        unsafe fn slice_characters_range(&self, destination: NonNull<unichar>, range: NSRange) {
            let global = self.ivars().slice.subrange(range.location, range.length)
                .unwrap_or_else(|| throw_virtual_range());
            self.ivars().binding.checked("virtual.string_bulk", || {
                // SAFETY: Local/global ranges have equal validated lengths inside the original
                // Source; its primitive fills the caller buffer. Forward the entire request so
                // original read diagnostics still observe real whole-bulk calls, without truncation.
                unsafe { self.ivars().source.getCharacters_range(destination, NSRange::new(global.start, global.length)) };
            });
        }

        #[unsafe(method(getCharacters:))]
        /// # Safety
        /// destination must be writable for this slice's actual UTF-16 length during this synchronous call.
        unsafe fn slice_all_characters(&self, destination: NonNull<unichar>) {
            let slice = self.ivars().slice;
            self.ivars().binding.checked("virtual.string_bulk", || {
                // SAFETY: Caller requests the whole actual slice, whose global range has the same
                // complete length. This reference path does not impose a viewport read budget.
                unsafe { self.ivars().source.getCharacters_range(destination, NSRange::new(slice.start, slice.length)) };
            });
        }

        #[unsafe(method_id(copyWithZone:))]
        #[unsafe(method_family = copy)]
        fn immutable_copy(&self, _zone: *mut NSZone) -> Retained<Self> {
            let count = self.ivars().binding.counts.string_copy_entries.get();
            if count < 4 && super::super::receiver_observation_requested() {
                self.ivars().binding.counts.string_copy_entries.set(count + 1);
                // Observe the actual copy entry before its first ticket load. All slices share
                // four log entries, with no additional text access or Native getter.
                let slice = self.ivars().slice;
                eprintln!(
                    "content_storage_probe stage=virtual_string_copy_entry call={} global_location={} global_length={} synthetic_only=true read_only=true parity=false diagnostic_formal=false",
                    count + 1, slice.start, slice.length,
                );
            }
            self.ivars().binding.checked("virtual.string_copy", || self.retain())
            // Copy convention transfers this one +1; this read-only value creates neither a full
            // character copy nor another retain. It does not prove framework-wide absence of copies.
        }

        #[unsafe(method_id(substringWithRange:))]
        #[unsafe(method_family = none)]
        fn slice_substring(&self, range: NSRange) -> Retained<NSString> {
            self.requested_substring(range)
        }

        #[unsafe(method_id(substringFromIndex:))]
        #[unsafe(method_family = none)]
        fn slice_substring_from(&self, index: NSUInteger) -> Retained<NSString> {
            let length = self.ivars().slice.length.checked_sub(index).unwrap_or_else(|| throw_virtual_range());
            self.requested_substring(NSRange::new(index, length)) // Do not ask super to copy the complete value before slicing it.
        }

        #[unsafe(method_id(substringToIndex:))]
        #[unsafe(method_family = none)]
        fn slice_substring_to(&self, index: NSUInteger) -> Retained<NSString> {
            self.requested_substring(NSRange::new(0, index))
        }
    }
);

impl SyntheticVirtualStringSlice {
    pub(super) fn create(
        binding: Rc<VirtualBinding>,
        source: Retained<NSString>,
        slice: Utf16Slice,
        mtm: MainThreadMarker,
    ) -> Result<Retained<NSString>, ProviderFailure> {
        // Validate this actual global view under the original read-only ticket before publishing
        // any native object. The source string itself retains no live canonical document owner.
        let ticket = binding.ticket();
        if Utf16Slice::new(slice.start, slice.length, ticket.key.length) != Some(slice) {
            return Err(ProviderFailure::Range); // Never publish an overflowing or out-of-original-document virtual string.
        }
        let allocated = ticket.native("virtual.string_alloc", || {
            Self::alloc(mtm).set_ivars(VirtualStringSliceIvars {
                binding: Rc::clone(&binding),
                source,
                slice,
            })
        })?;
        // SAFETY: Required primitives and ivars are initialized. The public NSString subclass
        // calls superclass init without supplying a complete text argument.
        let value: Retained<Self> = ticket.native("virtual.string_init", || unsafe {
            msg_send![super(allocated), init]
        })?;
        if !std::ptr::eq(value.class(), Self::class()) {
            return Err(ProviderFailure::Graph); // Do not access Rust ivars after unexpected class-cluster object replacement.
        }
        binding.include_owned(&ticket, "editor.virtual_paragraph.string_slice", &value)?;
        Ok(value.into_super())
    }

    fn requested_substring(&self, range: NSRange) -> Retained<NSString> {
        // Receiver bounds remain independent of the shared original Source. Exact full-range
        // requests retain this value; partial requests create one directly rooted Source view.
        let ticket = self.ivars().binding.ticket();
        let slice = self
            .ivars()
            .slice
            .subrange(range.location, range.length)
            .unwrap_or_else(|| throw_virtual_range());
        self.ivars().binding.log_substring(false, range, slice);
        if slice == self.ivars().slice {
            self.ivars()
                .binding
                .checked("virtual.string_full_substring", || {
                    self.retain().into_super()
                })
        } else {
            let mtm = MainThreadMarker::new().unwrap_or_else(|| throw_virtual_contract());
            let source = ticket
                .native("virtual.string_source_retain", || {
                    self.ivars().source.clone()
                })
                .unwrap_or_else(|_| throw_virtual_contract());
            Self::create(Rc::clone(&self.ivars().binding), source, slice, mtm)
                .unwrap_or_else(|_| throw_virtual_contract()) // The new object refers directly to the original Source, without a chain of parent substring owners.
        }
    }
}
