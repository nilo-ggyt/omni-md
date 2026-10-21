//! Immutable attributed values for the explicit diagnostic experiment. Share the frozen NSString
//! and captured run metadata without copying document text through a mutable attributed container.

use std::ops::Range;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSZone};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSAttributedString, NSAttributedStringKey, NSDictionary, NSIntersectionRange, NSObjectProtocol,
    NSRange, NSString, NSUInteger, ns_string,
};

use super::frozen_native_string::FrozenMarkdownString;
use super::frozen_value_family::{FrozenValueClaim, FrozenValueFamilyError};

pub(super) type FrozenAnnotationAttributes = NSDictionary<NSAttributedStringKey, AnyObject>;

/// Caller supplies actual source-local UTF-16 ranges. Construction copies dictionary containers;
/// their values retain Cocoa's shallow attribute semantics.
pub(super) struct FrozenAnnotationRun {
    pub(super) range: Range<usize>,
    pub(super) attributes: Retained<FrozenAnnotationAttributes>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum FrozenAnnotationError {
    #[error("不可变属性段范围无效或越过原值")]
    InvalidRunRange,
    #[error("不可变属性段未按原坐标排序或相互重叠")]
    RunsNotOrdered,
    #[error("系统没有保留不可变属性值的实际子类")]
    NativeClassChanged,
    #[error("不可变属性值无法登记真实生命周期：{0}")]
    Family(#[from] FrozenValueFamilyError),
}

struct FrozenAnnotationSegment {
    range: Range<usize>,
    attributes: Retained<FrozenAnnotationAttributes>,
}

/// Arc supplies atomic metadata ownership for native background copy/substring; it does not make
/// arbitrary attribute objects thread-safe.
///
/// Dictionary mappings are frozen while their values are not deep-copied. External mutation and
/// release threads must satisfy each Cocoa attribute object's contract. Neither this type nor its
/// native subclass implements Rust Send/Sync, and neither retains a live document, main-thread graph,
/// or revocable ticket.
struct FrozenAnnotationData {
    source: Retained<FrozenMarkdownString>,
    length: usize,
    segments: Box<[FrozenAnnotationSegment]>,
    // The original string and dictionary payloads actually drop before the shared AnnotationPayload claim settles.
    claim: FrozenValueClaim,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AnnotationView {
    start: usize,
    length: usize,
}

impl AnnotationView {
    fn subrange(self, range: NSRange) -> Option<Self> {
        let end = range.location.checked_add(range.length)?;
        if end > self.length {
            // Receiver EOF permits an empty substring, but the shared Source cannot authorize
            // reading attributes beyond the actual receiver's range.
            return None;
        }
        Some(Self {
            start: self.start.checked_add(range.location)?,
            length: range.length,
        })
    }

    fn index(self, index: usize) -> Option<usize> {
        if index >= self.length {
            // Empty values and EOF contain no attribute-bearing character at this index;
            // reject explicitly, just as the NSString primitives reject invalid character positions.
            return None;
        }
        self.start.checked_add(index)
    }

    fn end(self) -> usize {
        self.start + self.length // Constructed only from a validated actual Source range, never independently unchecked endpoints.
    }
}

pub(super) struct FrozenAnnotationIvars {
    data: Arc<FrozenAnnotationData>,
    view: AnnotationView,
    string: Retained<NSString>,
    // Data and the exact string view drop first; each actual native attributed value balances its own claim afterward.
    _claim: FrozenValueClaim,
}

define_class!(
    // SAFETY: Implements public NSAttributedString string/attributes primitives with actual UTF-16 ranges.
    // AnyThread permits construction only, not unsafe Send/Sync. Metadata uses Arc; attribute values retain shallow semantics.
    #[unsafe(super = NSAttributedString)]
    #[thread_kind = AnyThread]
    #[ivars = FrozenAnnotationIvars]
    pub(super) struct FrozenAnnotation;

    // SAFETY: Preserves inherited NSObject behavior. Last-value destruction releases resources without live-document or AppKit callbacks.
    unsafe impl NSObjectProtocol for FrozenAnnotation {}

    impl FrozenAnnotation {
        #[unsafe(method_id(string))]
        #[unsafe(method_family = none)]
        fn annotation_string(&self) -> Retained<NSString> {
            self.ivars().string.clone() // Retain the exact range value without copying or materializing characters.
        }

        #[unsafe(method(length))]
        fn annotation_length(&self) -> NSUInteger {
            self.ivars().view.length
        }

        #[unsafe(method_id(attributesAtIndex:effectiveRange:))]
        #[unsafe(method_family = none)]
        /// # Safety
        /// effective_range must be null or writable for one NSRange throughout this synchronous call.
        unsafe fn annotation_attributes(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
        ) -> Retained<FrozenAnnotationAttributes> {
            let segment_index = self.segment_at(index);
            let segment = &self.ivars().data.segments[segment_index];
            if !effective_range.is_null() {
                let local = self.local_segment_range(segment);
                // SAFETY: Caller supplies writable output; the actual segment intersects this view
                // and contains the already validated index.
                unsafe { effective_range.write(local) };
            }
            segment.attributes.clone() // The noncopy return convention transfers one valid dictionary retain.
        }

        #[unsafe(method_id(attributesAtIndex:longestEffectiveRange:inRange:))]
        #[unsafe(method_family = none)]
        /// # Safety
        /// effective_range must be null or writable for one NSRange throughout this synchronous call.
        unsafe fn annotation_longest_attributes(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
            range_limit: NSRange,
        ) -> Retained<FrozenAnnotationAttributes> {
            // A null output skips longest-range calculation only; it does not exempt index or
            // rangeLimit from their public receiver-bound checks.
            let segment_index = self.segment_at(index);
            self.ivars().view.subrange(range_limit).unwrap_or_else(|| throw_range());
            let attributes = &self.ivars().data.segments[segment_index].attributes;
            if !effective_range.is_null() {
                let full = self.longest_attributes_range(segment_index);
                let clipped = NSIntersectionRange(full, range_limit);
                // SAFETY: Clip the same view's actual longest equal-dictionary range to the valid
                // limit; caller owns this writable output.
                unsafe { effective_range.write(clipped) };
            }
            attributes.clone()
        }

        #[unsafe(method_id(copyWithZone:))]
        #[unsafe(method_family = copy)]
        fn annotation_copy(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain() // Immutable copy returns this one +1, without recapturing runs, dictionaries, or characters.
        }

        #[unsafe(method_id(attributedSubstringFromRange:))]
        #[unsafe(method_family = none)]
        fn annotation_substring(&self, range: NSRange) -> Retained<NSAttributedString> {
            let view = self.ivars().view.subrange(range).unwrap_or_else(|| throw_range());
            if view == self.ivars().view {
                self.retain().into_super() // The full-range substring shares this value; noncopy method_id handles return ownership.
            } else {
                Self::from_parts(Arc::clone(&self.ivars().data), view)
                    .unwrap_or_else(|_| throw_storage())
                    .into_super() // The new value shares original Data and exact offsets, without retaining a parent substring.
            }
        }
    }
);

impl FrozenAnnotation {
    /// Construct an immutable value from ordered actual runs. Uncovered intervals have empty
    /// dictionaries; no document text or separator characters are synthesized.
    pub(super) fn new(
        source: Retained<FrozenMarkdownString>,
        runs: Vec<FrozenAnnotationRun>,
    ) -> Result<Retained<Self>, FrozenAnnotationError> {
        let length = source.length();
        let mut previous_end = 0;
        for run in &runs {
            if run.range.start >= run.range.end || run.range.end > length {
                // Reject empty, reversed, and out-of-bounds runs before copying any dictionary;
                // never silently discard the caller's original attribute intent.
                return Err(FrozenAnnotationError::InvalidRunRange);
            }
            if run.range.start < previous_end {
                // Do not sort or clip original runs to conceal overlap. Their original coordinate
                // order is the sole attribute-range authority.
                return Err(FrozenAnnotationError::RunsNotOrdered);
            }
            previous_end = run.range.end;
        }

        // Only the original String claim derives this shared payload. Argument failure creates
        // no claim; subsequent native failure balances installed ownership through RAII.
        let claim = source.claim_annotation_payload()?;
        let empty = FrozenAnnotationAttributes::new();
        let mut segments = Vec::new();
        let mut next = 0;
        for run in runs {
            if next < run.range.start {
                // Fill only range metadata and a shared empty dictionary. A gap reads no characters
                // and adds no implicit style.
                segments.push(FrozenAnnotationSegment {
                    range: next..run.range.start,
                    attributes: empty.clone(),
                });
            }
            // SAFETY: Public NSDictionary copy freezes the container mapping. Keys remain NSString;
            // values are retained, not deep-copied or converted from NSNumber/NSString/NSNull into another type.
            let attributes: Retained<FrozenAnnotationAttributes> =
                unsafe { msg_send![&*run.attributes, copy] };
            next = run.range.end;
            segments.push(FrozenAnnotationSegment {
                range: run.range,
                attributes,
            });
        }
        if next < length {
            // The uncovered tail also uses an actual empty dictionary. An empty Source receives
            // no fabricated character segment.
            segments.push(FrozenAnnotationSegment {
                range: next..length,
                attributes: empty,
            });
        }
        // Native background callbacks may derive substrings from the same immutable ObjC value,
        // so shared payload clone/drop needs Arc rather than Rc. Containers are copied, Source/ranges
        // are immutable, and claims synchronize internally. General attribute values still obey Cocoa's
        // own thread contracts, so Data/native values gain no Rust Send/Sync. The independent control
        // retains the original on main until worker join and uses immutable Foundation attributes;
        // that control does not generalize to arbitrary AnyObject payloads.
        #[allow(
            clippy::arc_with_non_send_sync,
            reason = "Native AnyThread回调共享不可变payload需要原子计数；通用Cocoa属性不具Rust Send/Sync"
        )]
        let data = Arc::new(FrozenAnnotationData {
            source,
            length,
            segments: segments.into_boxed_slice(),
            claim,
        });
        Self::from_parts(data, AnnotationView { start: 0, length })
    }

    fn from_parts(
        data: Arc<FrozenAnnotationData>,
        view: AnnotationView,
    ) -> Result<Retained<Self>, FrozenAnnotationError> {
        // Every genuinely new attributed object registers its original payload lineage before
        // native allocation. copy(self) retains the existing value and does not enter this factory.
        let claim = data.claim.claim_annotation_value()?;
        let string = if view.start == 0 && view.length == data.length {
            data.source.clone().into_super() // The full range shares the original frozen NSString directly, without a mutable container.
        } else {
            data.source
                .substringWithRange(NSRange::new(view.start, view.length))
            // FrozenMarkdownString's public override creates an actual shared-snapshot substring,
            // including valid NSString slices containing a single surrogate code unit.
        };
        let allocated = Self::alloc().set_ivars(FrozenAnnotationIvars {
            data,
            view,
            string,
            _claim: claim,
        });
        // SAFETY: Required public primitives and all ivars are ready. Superclass init receives
        // and copies no document-text argument.
        let value: Retained<Self> = unsafe { msg_send![super(allocated), init] };
        if !std::ptr::eq(value.class(), Self::class()) {
            // If class-cluster initialization replaces the object, do not access Rust ivars;
            // ordinary Retained release balances it while construction reports failure.
            return Err(FrozenAnnotationError::NativeClassChanged);
        }
        Ok(value)
    }

    fn segment_at(&self, index: usize) -> usize {
        // Resolve receiver-local coordinates to the original segment directory. Binary search
        // must find one actual run or gap containing this character, not invent missing coverage.
        let global = self
            .ivars()
            .view
            .index(index)
            .unwrap_or_else(|| throw_range());
        let segments = &self.ivars().data.segments;
        let position = segments.partition_point(|segment| segment.range.end <= global);
        if segments.get(position).is_none_or(|segment| {
            // Every actual character belongs to one explicit run or gap; missing coverage is a
            // storage inconsistency, not successful empty attributes.
            global < segment.range.start || global >= segment.range.end
        }) {
            throw_storage();
        }
        position
    }

    fn local_segment_range(&self, segment: &FrozenAnnotationSegment) -> NSRange {
        // Return the segment/view intersection in receiver-local UTF-16 units, preserving the
        // substring's own start rather than leaking a shared Source's global coordinates.
        let view = self.ivars().view;
        let start = segment.range.start.max(view.start);
        let end = segment.range.end.min(view.end());
        NSRange::new(start - view.start, end - start)
    }

    fn longest_attributes_range(&self, index: usize) -> NSRange {
        // Extend only through adjacent equal dictionaries inside this view. This compares actual
        // attribute values; it neither reads characters nor assembles another attributed string.
        let data = &self.ivars().data;
        let view = self.ivars().view;
        let attributes = &data.segments[index].attributes;
        let mut first = index;
        let mut last = index;
        while first > 0 {
            let previous = &data.segments[first - 1];
            if previous.range.end <= view.start
                || !attributes.isEqualToDictionary(&previous.attributes)
            {
                // Dictionary value equality, not pointer identity, determines continuity; a view
                // boundary or unequal dictionary ends the actual longest interval.
                break;
            }
            first -= 1;
        }
        while last + 1 < data.segments.len() {
            let next = &data.segments[last + 1];
            if next.range.start >= view.end() || !attributes.isEqualToDictionary(&next.attributes) {
                // Different original dictionary objects may still be equal in adjacent segments;
                // no character reads or intermediate attributed strings are needed.
                break;
            }
            last += 1;
        }
        let start = data.segments[first].range.start.max(view.start);
        let end = data.segments[last].range.end.min(view.end());
        NSRange::new(start - view.start, end - start)
    }
}

fn throw_range() -> ! {
    // Public receiver-bound violations remain Foundation range exceptions.
    throw_named(
        ns_string!("NSRangeException"),
        ns_string!("不可变属性值的UTF-16请求越过接收者范围"),
    )
}

fn throw_storage() -> ! {
    // Inconsistent validated coverage/resource facts use a distinct internal consistency exception.
    throw_named(
        ns_string!("NSInternalInconsistencyException"),
        ns_string!("不可变属性值的已验证范围或资源不一致"),
    )
}

fn throw_named(name: &NSString, reason: &NSString) -> ! {
    let class = AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: Correct public NSException factory arguments, with a fixed text-free reason and nil userInfo.
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class, exceptionWithName: name, reason: reason, userInfo: None::<&AnyObject>]
    };
    // define_class supplies C-unwind entries; use the Objective-C throw/catch contract rather than a Rust panic substitute.
    objc2::exception::throw(exception)
}
