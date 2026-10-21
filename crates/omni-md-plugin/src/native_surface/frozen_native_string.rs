//! Independent immutable NSString values for the explicit frozen-value experiment. Actual UTF-16
//! subranges share Core's snapshot, without borrowing a live document or revocable adapter.

use std::ops::Range;
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::frozen_native_source::{FrozenNativeSource, FrozenReadKind};
use super::frozen_value_family::{FrozenValueClaim, FrozenValueFamilyError};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSZone};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSObjectProtocol, NSRange, NSString, NSUInteger, ns_string, unichar};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
/// Reject invalid ranges at the Rust constructor, before creating any Foundation value.
pub(super) enum FrozenMarkdownStringError {
    #[error("不可变文本范围越过快照边界")]
    RangeOutsideSnapshot,
    #[error("系统没有保留不可变文本的实际子类")]
    NativeClassChanged,
    #[error("不可变字符串无法登记真实生命周期：{0}")]
    Family(#[from] FrozenValueFamilyError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Actual snapshot coordinates permit endpoints inside surrogate pairs, preserving NSString's
/// UTF-16 code-unit slicing contract rather than imposing scalar-safe edit coordinates.
struct FrozenUtf16Range {
    start: usize,
    length: usize,
}

impl FrozenUtf16Range {
    fn new(range: Range<usize>, source_length: usize) -> Option<Self> {
        // Reject reversed/out-of-bounds intervals first; saturating arithmetic must not turn an
        // invalid request into a successfully constructed empty value.
        let length = range.end.checked_sub(range.start)?;
        (range.end <= source_length).then_some(Self {
            start: range.start,
            length,
        })
    }

    fn subrange(self, range: NSRange) -> Option<Self> {
        let local_end = range.location.checked_add(range.length)?;
        if local_end > self.length {
            // Receiver EOF permits an empty substring, but cannot authorize subsequent text
            // outside this substring merely because it belongs to the same snapshot.
            return None;
        }
        Some(Self {
            start: self.start.checked_add(range.location)?,
            length: range.length,
        })
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "独立Foundation控制消费观测；库单测只编译同源实现而不启动控制"
)]
/// Fixed scalar observations for the independent control. Counts describe actual primitive
/// requests only, without claiming that framework internals perform no copies.
pub(super) struct FrozenMarkdownStringReadObservation {
    pub(super) character_calls: usize,
    pub(super) bulk_calls: usize,
    pub(super) requested_units: usize,
    pub(super) maximum_bulk_units: usize,
    pub(super) full_snapshot_bulk_calls: usize,
    pub(super) copy_calls: usize,
    pub(super) substring_calls: usize,
}

#[cfg(test)]
#[derive(Default)]
/// Derived substrings share these scalar counters, without text, native objects, or lifetime callbacks.
struct FrozenReadCounters {
    character_calls: AtomicUsize,
    bulk_calls: AtomicUsize,
    requested_units: AtomicUsize,
    maximum_bulk_units: AtomicUsize,
    full_snapshot_bulk_calls: AtomicUsize,
    copy_calls: AtomicUsize,
    substring_calls: AtomicUsize,
}

#[cfg(test)]
impl FrozenReadCounters {
    #[allow(
        dead_code,
        reason = "独立Foundation控制分阶段采样；库单测不执行该原生入口"
    )]
    fn observation(&self) -> FrozenMarkdownStringReadObservation {
        // Read only atomics after each synchronous control phase, without a Native getter or
        // another text read merely to inspect diagnostic counts.
        FrozenMarkdownStringReadObservation {
            character_calls: self.character_calls.load(Ordering::Relaxed),
            bulk_calls: self.bulk_calls.load(Ordering::Relaxed),
            requested_units: self.requested_units.load(Ordering::Relaxed),
            maximum_bulk_units: self.maximum_bulk_units.load(Ordering::Relaxed),
            full_snapshot_bulk_calls: self.full_snapshot_bulk_calls.load(Ordering::Relaxed),
            copy_calls: self.copy_calls.load(Ordering::Relaxed),
            substring_calls: self.substring_calls.load(Ordering::Relaxed),
        }
    }
}

pub(super) struct FrozenMarkdownStringIvars {
    source: FrozenNativeSource,
    range: FrozenUtf16Range,
    // Local counters compile only for the independent test control. The enabled runtime experiment
    // uses the original Source hook and creates no additional counter Arc here.
    #[cfg(test)]
    reads: Arc<FrozenReadCounters>,
    // Actual Source and other resource fields drop before this concrete native string claim settles.
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "独立字符串控制保留真实claim析构；读取该字段的注解路径由独立注解控制覆盖"
        )
    )]
    claim: FrozenValueClaim,
}

define_class!(
    // SAFETY: Implements public NSString UTF-16 primitives, immutable copy, and actual substring.
    // Ivars are immutable or atomic; Core snapshots are Send+Sync, with no AppKit/thread-bound resources.
    #[unsafe(super = NSString)]
    #[thread_kind = AnyThread]
    #[ivars = FrozenMarkdownStringIvars]
    pub(super) struct FrozenMarkdownString;

    // SAFETY: Preserves inherited NSObject behavior, without mutable text or additional native resource callbacks.
    unsafe impl NSObjectProtocol for FrozenMarkdownString {}

    impl FrozenMarkdownString {
        #[unsafe(method(length))]
        fn frozen_length(&self) -> NSUInteger {
            self.ivars().range.length // Actual immutable length; editing or closing the live editor cannot turn this value into zero length.
        }

        #[unsafe(method(characterAtIndex:))]
        fn frozen_character(&self, index: NSUInteger) -> unichar {
            // Validate the original receiver-local request through its encapsulated Source before
            // reading a code unit; invalid coordinates raise Foundation's range exception.
            let read = self.ivars().source.request_read(
                FrozenReadKind::Character, self.ivars().range.start, self.ivars().range.length, index, 1,
            ).unwrap_or_else(|_| throw_range());
            #[cfg(test)]
            {
                // The independent control records this actual request. The runtime experiment's
                // original Source hook already received its facts inside request_read.
                self.ivars().reads.character_calls.fetch_add(1, Ordering::Relaxed);
                self.ivars().reads.requested_units.fetch_add(1, Ordering::Relaxed);
            }
            read.code_unit().unwrap_or_else(|_| throw_storage())
            // Failure after valid snapshot admission is an internal storage invariant failure;
            // a replacement character must not hide it as a successful read.
        }

        #[unsafe(method(getCharacters:range:))]
        /// # Safety
        /// destination must be writable for range.length UTF-16 units and u16-aligned for this synchronous call.
        unsafe fn frozen_characters_range(&self, destination: NonNull<unichar>, range: NSRange) {
            // SAFETY: Caller supplies actual capacity. The helper reports the hook and validates
            // the complete request before constructing a Rust slice over that destination.
            unsafe { self.copy_requested_units(destination, range) };
        }

        #[unsafe(method(getCharacters:))]
        /// # Safety
        /// destination must be writable for the receiver's complete UTF-16 length during this synchronous call.
        unsafe fn frozen_all_characters(&self, destination: NonNull<unichar>) {
            // A whole-value request executes its entire actual range, without truncation or
            // fabricated counts. This diagnostic NSString contract is not a bounded-viewport claim.
            // SAFETY: Caller supplies the receiver's actual writable length; construction validated the range.
            unsafe { self.copy_requested_units(destination, NSRange::new(0, self.ivars().range.length)) };
        }

        #[unsafe(method_id(copyWithZone:))]
        #[unsafe(method_family = copy)]
        fn frozen_copy(&self, _zone: *mut NSZone) -> Retained<Self> {
            #[cfg(test)]
            self.ivars().reads.copy_calls.fetch_add(1, Ordering::Relaxed);
            self.retain() // Immutable copy transfers this one +1, without copying characters, snapshot, or resource index.
        }

        #[unsafe(method_id(substringWithRange:))]
        #[unsafe(method_family = none)]
        fn frozen_substring(&self, range: NSRange) -> Retained<NSString> {
            self.substring_value(range)
        }

        #[unsafe(method_id(substringFromIndex:))]
        #[unsafe(method_family = none)]
        fn frozen_substring_from(&self, index: NSUInteger) -> Retained<NSString> {
            let length = self.ivars().range.length.checked_sub(index).unwrap_or_else(|| throw_range());
            self.substring_value(NSRange::new(index, length)) // EOF returns an actual empty substring; out-of-bounds raises explicitly.
        }

        #[unsafe(method_id(substringToIndex:))]
        #[unsafe(method_family = none)]
        fn frozen_substring_to(&self, index: NSUInteger) -> Retained<NSString> {
            self.substring_value(NSRange::new(0, index))
        }
    }
);

// SAFETY: AnyThread constrains construction only; arbitrary NSString may actually be NSMutableString,
// so that marker alone does not establish Send. This class's sole constructor checks exact Self class.
// Its opaque Source is immutable and Send+Sync, ranges are immutable, and test counters are atomic.
// The claim family owns synchronized metadata/atomic observers; its function hook captures no business owner.
// No live document, mutable NSString, AppKit object, main-thread Drop, or native callback is retained.
// Immutable NSString retain/release may cross threads; each thread still needs its own autorelease pool.
unsafe impl Send for FrozenMarkdownString {}

// SAFETY: This concrete immutable class has no writable text alias. Inherited NSString reads consume
// its stable primitives; copy shares the value, and substring creates an independent snapshot range.
// Test observations use atomics. Exceptions follow objc2's C-unwind/catch contract, without sharing an
// exception object or thread-local pool. This reasoning does not apply to live or mutable adapters.
unsafe impl Sync for FrozenMarkdownString {}

impl FrozenMarkdownString {
    /// Construct the exact global immutable range; the whole opaque Source is 0..source.utf16_len().
    pub(super) fn new(
        source: FrozenNativeSource,
        global_utf16_range: Range<usize>,
    ) -> Result<Retained<Self>, FrozenMarkdownStringError> {
        let range = FrozenUtf16Range::new(global_utf16_range, source.utf16_len())
            .ok_or(FrozenMarkdownStringError::RangeOutsideSnapshot)?;
        Self::from_parts(
            source,
            range,
            #[cfg(test)]
            Arc::new(FrozenReadCounters::default()),
        )
    }

    /// Read scalar observations for the independent Foundation control's phase checks, without text access.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "仅独立Foundation控制调用；库单测不能据编译自行启动原生控制"
    )]
    pub(super) fn read_observation(&self) -> FrozenMarkdownStringReadObservation {
        self.ivars().reads.observation()
    }

    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "独立字符串控制不链接注解；真实注解派生由独立注解控制验证"
        )
    )]
    pub(super) fn claim_annotation_payload(
        &self,
    ) -> Result<FrozenValueClaim, FrozenValueFamilyError> {
        // Only this concrete string claim may derive an annotation payload in the original family.
        self.ivars().claim.claim_annotation_payload()
    }

    fn from_parts(
        source: FrozenNativeSource,
        range: FrozenUtf16Range,
        #[cfg(test)] reads: Arc<FrozenReadCounters>,
    ) -> Result<Retained<Self>, FrozenMarkdownStringError> {
        // Derive from the still-live Source claim first; sealing new sources does not prevent an
        // existing Source from constructing another actual immutable substring.
        let claim = source.claim_string_value()?;
        let allocated = Self::alloc().set_ivars(FrozenMarkdownStringIvars {
            source,
            range,
            #[cfg(test)]
            reads,
            claim,
        });
        // SAFETY: Required NSString primitives and ivars are ready; superclass init initializes this
        // same value. The returned type preserves objc2's PartialInit and init ownership contract.
        let value: Retained<Self> = unsafe { msg_send![super(allocated), init] };
        if !std::ptr::eq(value.class(), Self::class()) {
            // If class-cluster initialization replaced the object, do not access Rust ivars;
            // the original Retained follows ordinary release while this constructor reports failure.
            return Err(FrozenMarkdownStringError::NativeClassChanged);
        }
        Ok(value)
    }

    fn substring_value(&self, local_range: NSRange) -> Retained<NSString> {
        // Receiver-local bounds are checked before creating any derived native value or updating
        // observations. Sharing the same Source cannot extend this receiver's admitted range.
        let range = self
            .ivars()
            .range
            .subrange(local_range)
            .unwrap_or_else(|| throw_range());
        #[cfg(test)]
        self.ivars()
            .reads
            .substring_calls
            .fetch_add(1, Ordering::Relaxed);
        if range == self.ivars().range {
            self.retain().into_super() // An exact whole-receiver substring shares this value; noncopy method_id handles return ownership.
        } else {
            // Share only the opaque Source and actual global offset. Do not retain the parent
            // string or form a native substring chain back through it.
            Self::from_parts(
                self.ivars().source.clone(),
                range,
                #[cfg(test)]
                Arc::clone(&self.ivars().reads),
            )
            .unwrap_or_else(|_| throw_storage())
            .into_super()
        }
    }

    /// Initialize the complete validated caller request without allocating a UTF-16 staging
    /// buffer or truncating a native bulk request.
    ///
    /// # Safety
    /// destination covers local.length writable, aligned UTF-16 units during this synchronous call.
    unsafe fn copy_requested_units(&self, destination: NonNull<unichar>, local: NSRange) {
        // Invalid ranges still report original hook facts. Failure precedes destination-slice
        // construction, observation updates, and every character read.
        let read = self
            .ivars()
            .source
            .request_read(
                FrozenReadKind::Bulk,
                self.ivars().range.start,
                self.ivars().range.length,
                local.location,
                local.length,
            )
            .unwrap_or_else(|_| throw_range());
        let length = read.length();
        #[cfg(test)]
        {
            // Only the independent control accumulates local bulk counts. The runtime diagnostic
            // hook receives the same request_read facts for the full original request.
            let reads = &self.ivars().reads;
            reads.bulk_calls.fetch_add(1, Ordering::Relaxed);
            reads.requested_units.fetch_add(length, Ordering::Relaxed);
            reads
                .maximum_bulk_units
                .fetch_max(length, Ordering::Relaxed);
            if read.global_start() == 0 && length == self.ivars().source.utf16_len() {
                // Record an actual full-snapshot request honestly, even when Foundation issued
                // it; such a request cannot be relabeled as successful bounded loading.
                reads
                    .full_snapshot_bulk_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        if length == 0 {
            // Empty ranges, including actual EOF substrings, need no slice or destination dereference.
            return;
        }
        // SAFETY: Admission validated equal local/global length inside the snapshot. Caller supplies
        // one writable, aligned allocation of that length, without another writable alias or Native reentry here.
        let output = unsafe { std::slice::from_raw_parts_mut(destination.as_ptr(), length) };
        if read.copy_to(output).is_err() {
            // Core snapshots do not revoke; an actual storage invariant failure cannot become
            // short-read success or replacement characters.
            throw_storage();
        }
    }
}

fn throw_range() -> ! {
    // Public receiver-bound violations use Foundation's range exception, not a Rust panic.
    throw_named(
        ns_string!("NSRangeException"),
        ns_string!("不可变字符串的UTF-16请求越过接收者范围"),
    )
}

fn throw_storage() -> ! {
    // A failed already-validated immutable read is a distinct internal consistency exception.
    throw_named(
        ns_string!("NSInternalInconsistencyException"),
        ns_string!("不可变字符串的已验证快照资源不一致"),
    )
}

fn throw_named(name: &NSString, reason: &NSString) -> ! {
    let class = AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: Public NSException factory with fixed, text-free name/reason and nil userInfo follows its ABI.
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class, exceptionWithName: name, reason: reason, userInfo: None::<&AnyObject>]
    };
    // define_class supplies extern C-unwind entries. Use objc2's Objective-C exception helper;
    // callers must receive this with ObjC catch, not Rust panic/catch_unwind as a substitute.
    objc2::exception::throw(exception)
}
