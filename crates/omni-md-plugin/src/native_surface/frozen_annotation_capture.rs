//! Capture checking annotations for one original native action in the explicit frozen-value
//! diagnostic graph. Read actual rendering metadata while sharing text through the opaque Source.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, msg_send};
use objc2_app_kit::{
    NSTextCheckingController, NSTextContentManager, NSTextContentStorage, NSTextElementProvider,
    NSTextLayoutManager, NSTextLocation, NSTextRange, NSTextStorageObserving, NSTextView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSComparisonResult, NSNotFound, NSRange, NSString, ns_string,
};

use super::frozen_annotation::{FrozenAnnotation, FrozenAnnotationAttributes, FrozenAnnotationRun};
use super::frozen_native_source::FrozenNativeSource;
use super::frozen_native_string::FrozenMarkdownString;
use super::{
    MarkdownPresentationMode, NativeMarkdownSelectionCapture, PieceBackedMarkdownTextStorage,
    PieceBackedMarkdownTextView, WeakMarkdownDocument,
};

pub(super) enum FrozenAnnotationCapture {
    PassThrough,
    Outside,
    Cancelled,
    Value(Retained<NSAttributedString>, NSRange),
}

static PRE_OWNER_PASSTHROUGH: AtomicUsize = AtomicUsize::new(0);
static RANGE_REQUEST_DIAGNOSTICS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn pre_owner_passthrough_count() -> usize {
    PRE_OWNER_PASSTHROUGH.load(Ordering::Relaxed) // Scalar observation only; superclass fallback's actual text reads remain separate facts.
}

#[derive(Clone, Copy)]
enum CaptureFailure {
    Cancelled,
    NativeGraph,
    NativeRange,
    Source,
    Construction,
}

/// Original-action weak identities and scalars only. No document/slot borrow or borrowed native
/// object crosses a getter; later observations cannot recapture another action's authority.
struct CaptureOrigin {
    selection: NativeMarkdownSelectionCapture,
    identity: WeakMarkdownDocument,
    generation: u64,
    length: usize,
    semantic_epoch: u64,
    table_epoch: u64,
    fold_epoch: u64,
    input_epoch: u64,
    mode: MarkdownPresentationMode,
}

impl CaptureOrigin {
    fn current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> bool {
        if view.ivars().textkit_graph_teardown.get()
            || !self.selection.current()
            || !storage.ivars().accepts_edits.get()
            || storage.ivars().font_run_scratch.epoch() != self.semantic_epoch
            || storage.ivars().table_geometry_epoch.get() != self.table_epoch
            || view.ivars().preview_block_fold_epoch.get() != self.fold_epoch
            || view.ivars().table_input_intent_epoch.get() != self.input_epoch
            || storage.presentation_mode() != self.mode
            || !self.identity.ptr_eq(&storage.ivars().document.borrow())
        {
            return false; // A revoked action cannot adopt current generation or another manager to renew this checking request.
        }
        let Some(document) = self.identity.upgrade() else {
            return false; // Original Weak preserves allocation identity, but actual document ownership already ended.
        };
        let Ok(document) = document.try_borrow() else {
            return false; // Do not reenter an exclusive document commit or treat its transient state as the original frozen Source.
        };
        document.generation() == self.generation && document.utf16_len() == self.length
    }
}

/// The temporary controller serves only public validAnnotations. Normal return, revocation, and
/// foreign exception unwind all invalidate this exact locally owned controller.
struct TemporaryCheckingController(Retained<NSTextCheckingController>);

impl Drop for TemporaryCheckingController {
    fn drop(&mut self) {
        // Even revoked business authority may clean the same controller created on this stack.
        // Do not initiate checking, replace the view's controller, or swallow exceptions from
        // invalidate or an earlier native operation.
        self.0.invalidate();
    }
}

pub(super) fn capture(
    view: &PieceBackedMarkdownTextView,
    proposed: NSRange,
) -> FrozenAnnotationCapture {
    // Disabled sessions preserve the original superclass operation. This experiment must not
    // alter ordinary checking merely because its two diagnostic features were compiled.
    if !view.ivars().frozen_values_enabled {
        return FrozenAnnotationCapture::PassThrough; // A closed constructor gate touches neither folds nor native getters.
    }
    let Ok(selection) = view.ivars().folds.capture_selection() else {
        // Initial construction may have no original owner ticket. Preserve the same system entry
        // rather than guessing an owner or presenting unverified work as Ready. Count only this
        // already-enabled session, without Native Weak upgrades; superclass reads remain visible
        // to the original global read diagnostic.
        let ordinal = PRE_OWNER_PASSTHROUGH
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_add(1)
            })
            .map_or(usize::MAX, |count| count + 1);
        if ordinal <= 4 || (ordinal <= 262_144 && ordinal.is_power_of_two()) {
            eprintln!(
                "frozen_annotation_capture event=pre_owner_passthrough count={ordinal} formal=false"
            );
        } // Bound log emission only; unprinted superclass fallbacks still remain in the full scalar count.
        return FrozenAnnotationCapture::PassThrough;
    };
    let input_epoch = view.ivars().table_input_intent_epoch.get();
    let fold_epoch = view.ivars().preview_block_fold_epoch.get();
    let storage = view.ivars().text_storage.load();
    if !selection.current()
        || view.ivars().textkit_graph_teardown.get()
        || view.ivars().table_input_intent_epoch.get() != input_epoch
        || view.ivars().preview_block_fold_epoch.get() != fold_epoch
    {
        return FrozenAnnotationCapture::Cancelled; // Even the first Native Weak load must preserve the original pre-call ticket.
    }
    let Some(storage) = storage else {
        return fail(CaptureFailure::NativeGraph); // An enabled, ticketed action cannot silently switch to an unmeasured superclass fallback.
    };
    let Some(values) = &storage.ivars().string.ivars().frozen_values else {
        return fail(CaptureFailure::Source); // View/storage share one constructor decision; rereading the environment cannot repair admission.
    };
    let identity = storage.ivars().document.borrow().clone();
    let Some(document) = identity.upgrade() else {
        return FrozenAnnotationCapture::Cancelled;
    };
    if !view.ivars().folds.matches_document(&document) {
        return FrozenAnnotationCapture::Cancelled; // Another storage at equal generation still does not belong to this original action.
    }
    let Some(source) = values.source_for_document(&document) else {
        drop(document);
        return fail(CaptureFailure::Source);
    };
    drop(document); // From here to return, no live-document strong claim or RefCell borrow crosses Native work.
    let origin = CaptureOrigin {
        selection,
        identity,
        generation: source.generation(),
        length: source.utf16_len(),
        semantic_epoch: storage.ivars().font_run_scratch.epoch(),
        table_epoch: storage.ivars().table_geometry_epoch.get(),
        fold_epoch,
        input_epoch,
        mode: storage.presentation_mode(),
    };
    let current = || origin.current(view, &storage);
    if !current() {
        return FrozenAnnotationCapture::Cancelled;
    }
    let Some(intersection) = intersect_proposed_range(proposed, source.utf16_len()) else {
        observe_request_range(proposed, source.utf16_len(), None, None);
        return FrozenAnnotationCapture::Outside; // An entirely disjoint request writes no actualRange and creates no checking value.
    };
    let actual = match source.text_checking_paragraph_range(intersection.clone()) {
        Ok(range) => range,
        Err(_) => {
            observe_request_range(proposed, source.utf16_len(), Some(&intersection), None);
            return fail(CaptureFailure::Source); // Preserve actual range-adjustment failure without reading text for diagnostics or falling back to super.
        }
    };
    observe_request_range(
        proposed,
        source.utf16_len(),
        Some(&intersection),
        Some(&actual),
    );
    let result = capture_value(view, &storage, &source, actual.clone(), &current);
    // capture_value's local manager/controller/dictionary claims have dropped. Recheck original
    // authority once more after their destructors, which may themselves permit native reentry.
    if !current() {
        drop(result);
        return FrozenAnnotationCapture::Cancelled;
    }
    match result {
        Ok(value) => FrozenAnnotationCapture::Value(
            value,
            NSRange::new(actual.start, actual.end - actual.start),
        ),
        Err(error) => fail(error),
    }
}

fn observe_request_range(
    proposed: NSRange,
    length: usize,
    intersection: Option<&Range<usize>>,
    actual: Option<&Range<usize>>,
) {
    // Emit at most eight process-wide scalar facts, reusing the completed range query. Request
    // no text, Native operation, or new authority merely to report diagnostic coordinates.
    let ordinal = RANGE_REQUEST_DIAGNOSTICS
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            count.checked_add(1)
        })
        .map_or(usize::MAX, |count| count + 1);
    if ordinal > 8 {
        return; // Past the logging budget, retain/format no request; actual checking and global primitive diagnostics still execute unchanged.
    }
    let (intersection_start, intersection_length) =
        intersection.map_or((0, 0), |range| (range.start, range.end - range.start));
    let (actual_start, actual_length) =
        actual.map_or((0, 0), |range| (range.start, range.end - range.start));
    eprintln!(
        "frozen_annotation_capture event=range_request ordinal={ordinal} proposed_start={} proposed_length={} intersection_present={} intersection_start={intersection_start} intersection_length={intersection_length} actual_present={} actual_start={actual_start} actual_length={actual_length} source_length={length} formal=false",
        proposed.location,
        proposed.length,
        intersection.is_some(),
        actual.is_some(),
    );
}

fn capture_value(
    view: &PieceBackedMarkdownTextView,
    storage: &PieceBackedMarkdownTextStorage,
    source: &FrozenNativeSource,
    actual: Range<usize>,
    current: &impl Fn() -> bool,
) -> Result<Retained<NSAttributedString>, CaptureFailure> {
    // Actual empty ranges still produce a real immutable empty value. Nonempty ranges capture
    // rendering metadata before constructing the Source-sharing string and annotation objects.
    let runs = if actual.is_empty() {
        Vec::new() // A valid empty document or EOF caret has no attribute character, but still returns an actual empty value rather than nil.
    } else {
        capture_rendering_runs(view, storage, source.utf16_len(), &actual, current)?
    };
    let string = native_step(current, || {
        FrozenMarkdownString::new(source.clone(), actual)
    })?
    .map_err(|_| CaptureFailure::Construction)?;
    let value = native_step(current, || FrozenAnnotation::new(string, runs))?
        .map_err(|_| CaptureFailure::Construction)?;
    Ok(value.into_super()) // No mutable attributed container, superclass checking call, or text copy is introduced.
}

fn capture_rendering_runs(
    view: &PieceBackedMarkdownTextView,
    storage: &PieceBackedMarkdownTextStorage,
    length: usize,
    actual: &Range<usize>,
    current: &impl Fn() -> bool,
) -> Result<Vec<FrozenAnnotationRun>, CaptureFailure> {
    // Capture only the original synchronous TextKit reference graph. Another manager or a
    // background layout queue cannot donate annotation metadata to this frozen Source.
    let manager =
        native_step(current, || view.textLayoutManager())?.ok_or(CaptureFailure::NativeGraph)?;
    let expected_manager = native_step(current, || {
        view.ivars()
            .text_layout_manager
            .get()
            .and_then(|weak| weak.load())
    })?
    .ok_or(CaptureFailure::NativeGraph)?;
    if !std::ptr::eq(&*manager, &*expected_manager)
        || native_step(current, || manager.layoutQueue())?.is_some()
    {
        return Err(CaptureFailure::NativeGraph); // An unverified graph or background layout cannot supply this action's annotation snapshot.
    }
    let content = native_step(current, || manager.textContentManager())?
        .ok_or(CaptureFailure::NativeGraph)?;
    verify_graph(view, storage, &manager, &content, current)?;
    let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
    let document_range = native_step(current, || provider.documentRange())?;
    let document_start = native_step(current, || document_range.location())?;
    let document_end = native_step(current, || document_range.endLocation())?;
    if location_offset(provider, &document_start, &document_end, length, current)? != length {
        return Err(CaptureFailure::NativeRange); // The native global coordinate domain must equal this frozen Source's complete length.
    }
    let keys = valid_annotation_keys(view, current)?;
    let runs = RefCell::new(Vec::new());
    let previous_end = Cell::new(0usize);
    let failure = Cell::new(None);
    let callback = RcBlock::new(
        |callback_manager: std::ptr::NonNull<NSTextLayoutManager>,
         attributes: std::ptr::NonNull<FrozenAnnotationAttributes>,
         range: std::ptr::NonNull<NSTextRange>|
         -> Bool {
            let result = (|| {
                if !current() {
                    return Err(CaptureFailure::Cancelled);
                }
                if !std::ptr::eq(callback_manager.as_ptr(), &*manager) {
                    return Err(CaptureFailure::NativeGraph); // The callback must belong to this original manager.
                }
                // SAFETY: Public synchronous enumeration keeps all three nonnull objects alive
                // throughout this block; no raw pointer escapes its invocation.
                let range = unsafe { range.as_ref() };
                let start_location = native_step(current, || range.location())?;
                let end_location = native_step(current, || range.endLocation())?;
                let start =
                    location_offset(provider, &document_start, &start_location, length, current)?;
                let end =
                    location_offset(provider, &document_start, &end_location, length, current)?;
                if start > end || start < previous_end.get() {
                    return Err(CaptureFailure::NativeRange); // Sorting/clipping cannot conceal a broken native enumeration order or overlap.
                }
                previous_end.set(end);
                if start >= actual.end {
                    return Ok(false); // Past the actual adjusted range, stop later metadata without truncating an intersecting run.
                }
                let intersection = start.max(actual.start)..end.min(actual.end);
                if intersection.start >= intersection.end {
                    return Ok(true); // Earlier/empty runs contain no target attribute character and must not fabricate empty coverage.
                }
                // SAFETY: This synchronous block keeps its original dictionary alive. Read only
                // values for actual valid-annotation keys, without text or object descriptions.
                let attributes = unsafe { attributes.as_ref() };
                let filtered = filtered_attributes(attributes, &keys, current)?;
                if let Some(attributes) = filtered {
                    runs.borrow_mut().push(FrozenAnnotationRun {
                        range: intersection.start - actual.start..intersection.end - actual.start,
                        attributes,
                    }); // Retain actual nonempty annotation runs only; FrozenAnnotation preserves empty-dictionary semantics in gaps.
                }
                Ok(true)
            })();
            match result {
                Ok(keep_going) => Bool::new(keep_going),
                Err(error) => {
                    failure.set(Some(error));
                    Bool::NO // Invalid original authority/coordinates stop immediately; no partial Ready result or automatic retry follows.
                }
            }
        },
    );
    // Enumerate actual metadata from the verified document origin, without assuming the API will
    // recover a run beginning before the target. Read annotation/rendering ranges only, not text,
    // and request no layout. Dense metadata has no run-count cap here; this diagnostic path is
    // therefore not evidence of the production viewport's work or memory bounds.
    native_step(current, || {
        manager.enumerateRenderingAttributesFromLocation_reverse_usingBlock(
            &document_start,
            false,
            &callback,
        )
    })?;
    drop(callback); // The block leaves no RefCell borrow; later native destructor reentry remains subject to original-current checks.
    if let Some(error) = failure.get() {
        return Err(error);
    }
    verify_graph(view, storage, &manager, &content, current)?;
    Ok(runs.into_inner())
}

fn verify_graph(
    view: &PieceBackedMarkdownTextView,
    storage: &PieceBackedMarkdownTextStorage,
    manager: &NSTextLayoutManager,
    content: &NSTextContentManager,
    current: &impl Fn() -> bool,
) -> Result<(), CaptureFailure> {
    // Each native getter may reenter independently. Compare the exact original storage,
    // manager, and content identities before accepting their combined metadata.
    let actual_storage = native_step(current, || view.ivars().text_storage.load())?;
    let actual_manager = native_step(current, || view.textLayoutManager())?;
    let actual_content = native_step(current, || manager.textContentManager())?;
    if actual_storage
        .as_deref()
        .is_none_or(|actual| !std::ptr::eq(actual, storage))
        || actual_manager
            .as_deref()
            .is_none_or(|actual| !std::ptr::eq(actual, manager))
        || actual_content
            .as_deref()
            .is_none_or(|actual| !std::ptr::eq(actual, content))
    {
        return Err(CaptureFailure::NativeGraph); // Synchronous graph replacement cannot combine the original Source with successor annotations.
    }
    let content_storage = content
        .downcast_ref::<NSTextContentStorage>()
        .ok_or(CaptureFailure::NativeGraph)?;
    let observer = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(content_storage);
    let canonical = native_step(current, || observer.textStorage())?;
    let expected: &objc2_app_kit::NSTextStorage = storage;
    if canonical
        .as_deref()
        .is_none_or(|actual| !std::ptr::eq(actual, expected))
    {
        return Err(CaptureFailure::NativeGraph); // The actual content provider must still observe this same canonical storage.
    }
    Ok(())
}

fn valid_annotation_keys(
    view: &PieceBackedMarkdownTextView,
    current: &impl Fn() -> bool,
) -> Result<Vec<Retained<NSString>>, CaptureFailure> {
    // Obtain the system's actual valid keys from one temporary controller. Do not replace the
    // view's controller, perform checking, or maintain a second guessed annotation-key schema.
    let controller = native_step(current, || {
        let client: &NSTextView = view;
        // SAFETY: Public initWithClient: accepts id<NSTextCheckingClient>; the actual NSTextView
        // supplies those selectors. The binding omits Rust conformance, so do not fabricate a trait
        // implementation. This invokes the same public API used by the independent native control.
        let controller: Retained<NSTextCheckingController> =
            unsafe { msg_send![NSTextCheckingController::alloc(), initWithClient: client] };
        TemporaryCheckingController(controller) // Install RAII immediately after init; revocation still invalidates this exact object.
    })?;
    let original_keys = native_step(current, || controller.0.validAnnotations())?;
    // SAFETY: Public NSArray copy returns one immutable-key-array +1, without creating another
    // business controller or reading document text.
    let keys: Retained<NSArray<NSString>> =
        native_step(current, || unsafe { msg_send![&*original_keys, copy] })?;
    drop(original_keys);
    drop(controller); // Invalidate/drop before rendering enumeration, without replacing the view's default controller.
    if !current() {
        return Err(CaptureFailure::Cancelled);
    }
    let count = native_step(current, || keys.count())?;
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        result.push(native_step(current, || keys.objectAtIndex(index))?);
        // Retain actual system keys only; no guessed fixed string list discards future system annotations.
    }
    Ok(result)
}

fn filtered_attributes(
    attributes: &FrozenAnnotationAttributes,
    keys: &[Retained<NSString>],
    current: &impl Fn() -> bool,
) -> Result<Option<Retained<FrozenAnnotationAttributes>>, CaptureFailure> {
    // Keep only keys the real checking controller accepts, retaining the actual shallow Cocoa
    // values. Pure rendering styles must not leak into the checking annotation result.
    let mut kept_keys: Vec<&NSString> = Vec::new();
    let mut values = Vec::new();
    for key in keys {
        if let Some(value) = native_step(current, || attributes.objectForKey(key))? {
            kept_keys.push(key.as_ref());
            values.push(value); // Preserve actual NSNumber/NSNull/NSString/etc. shallow values without casting or rewriting them.
        }
    }
    if values.is_empty() {
        return Ok(None); // A visual-only run has no valid annotation; font/color storage attributes do not enter this checking result.
    }
    let value_refs: Vec<&AnyObject> = values.iter().map(|value| &**value).collect();
    Ok(Some(native_step(current, || {
        FrozenAnnotationAttributes::from_slices(&kept_keys, &value_refs)
    })?))
}

fn location_offset(
    provider: &ProtocolObject<dyn NSTextElementProvider>,
    origin: &ProtocolObject<dyn NSTextLocation>,
    location: &ProtocolObject<dyn NSTextLocation>,
    length: usize,
    current: &impl Fn() -> bool,
) -> Result<usize, CaptureFailure> {
    // Require a nonnegative bounded provider offset and an exact inverse location. Numeric
    // proximity alone cannot certify that a native range belongs to the frozen Source domain.
    let offset = native_step(current, || {
        provider.offsetFromLocation_toLocation(origin, location)
    })?;
    let offset = usize::try_from(offset).map_err(|_| CaptureFailure::NativeRange)?;
    if offset > length {
        return Err(CaptureFailure::NativeRange); // Only actual document coordinates are valid; do not clamp an invalid native location.
    }
    let signed = isize::try_from(offset).map_err(|_| CaptureFailure::NativeRange)?;
    let roundtrip = native_step(current, || {
        provider.locationFromLocation_withOffset(origin, signed)
    })?
    .ok_or(CaptureFailure::NativeRange)?;
    if native_step(current, || location.compare(&roundtrip))? != NSComparisonResult::Same {
        return Err(CaptureFailure::NativeRange); // Forward offset and inverse location must agree exactly in the original provider.
    }
    Ok(offset)
}

fn native_step<T>(
    current: &impl Fn() -> bool,
    action: impl FnOnce() -> T,
) -> Result<T, CaptureFailure> {
    // Carry the original action through each reentrant native operation, without reacquiring
    // current selection or source authority after the call returns.
    if !current() {
        return Err(CaptureFailure::Cancelled);
    }
    let result = action();
    if !current() {
        return Err(CaptureFailure::Cancelled); // The returned object drops on this stack; it cannot renew another action's authority.
    }
    Ok(result)
}

fn intersect_proposed_range(proposed: NSRange, length: usize) -> Option<Range<usize>> {
    if proposed.location == NSNotFound as usize && proposed.length == 0 {
        return Some(0..length); // The protocol's explicit full-document sentinel stays full-document, never relabeled as a viewport request.
    }
    if proposed.location > length {
        return None; // Both document endpoints are valid carets; only a start strictly beyond EOF is entirely disjoint.
    }
    let start = proposed.location;
    let end = if proposed.length > length - start {
        length // Standard document intersection compares remaining length to avoid overflow; it does not shorten a valid paragraph for performance.
    } else {
        start + proposed.length
    };
    Some(start..end)
}

fn fail(error: CaptureFailure) -> FrozenAnnotationCapture {
    // Ordinary original-action revocation discards the candidate. Inconsistent graph/range/Source
    // or construction facts fail explicitly rather than silently taking the superclass path.
    if matches!(error, CaptureFailure::Cancelled) {
        return FrozenAnnotationCapture::Cancelled;
    }
    let class = AnyClass::get(c"NSException").expect("Foundation异常类已链接");
    // SAFETY: Explicit failure in this same experiment uses the public factory and a fixed
    // diagnostic, without text/path disclosure or swallowing the exception to continue.
    let exception: Retained<objc2::exception::Exception> = unsafe {
        msg_send![class, exceptionWithName: ns_string!("NSInternalInconsistencyException"),
            reason: ns_string!("不可变检查注解无法认证原Native来源或完整范围"), userInfo: None::<&AnyObject>]
    };
    objc2::exception::throw(exception)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposed_document_intersection_preserves_eof_empty_and_full_sentinel() {
        // Pure production range arithmetic preserves the full sentinel and real empty endpoints;
        // these checks do not execute a native checking controller or claim bounded text reads.
        assert_eq!(
            intersect_proposed_range(NSRange::new(NSNotFound as usize, 0), 19),
            Some(0..19)
        );
        assert_eq!(
            intersect_proposed_range(NSRange::new(19, 0), 19),
            Some(19..19)
        );
        assert_eq!(
            intersect_proposed_range(NSRange::new(19, 8), 19),
            Some(19..19)
        );
        assert_eq!(intersect_proposed_range(NSRange::new(20, 0), 19), None);
        assert_eq!(intersect_proposed_range(NSRange::new(0, 0), 0), Some(0..0));
    }

    #[test]
    fn proposed_document_intersection_clamps_only_standard_bounds_without_overflow() {
        // Intersect actual document bounds even for overflowing requested lengths, while a
        // non-sentinel invalid start remains outside instead of gaining full-document permission.
        assert_eq!(
            intersect_proposed_range(NSRange::new(4, 7), 19),
            Some(4..11)
        );
        assert_eq!(
            intersect_proposed_range(NSRange::new(4, usize::MAX), 19),
            Some(4..19)
        );
        assert_eq!(
            intersect_proposed_range(NSRange::new(NSNotFound as usize, 1), 19),
            None
        );
    }
}
