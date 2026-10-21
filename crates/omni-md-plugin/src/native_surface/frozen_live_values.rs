//! The explicit diagnostic experiment's live-to-immutable boundary. Cache only a weak Source,
//! without retaining the live document or a full-text copy; this is absent from the default editor.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::Arc;

use objc2::Message;
use objc2::rc::Retained;
use objc2_foundation::{MainThreadMarker, NSRange, NSString, ns_string};

use super::frozen_native_source::{FrozenNativeSource, FrozenNativeSourceWeak};
use super::frozen_native_string::{FrozenMarkdownString, FrozenMarkdownStringError};
use super::frozen_value_family::FrozenValueFamily;
use super::{SharedMarkdownDocument, WeakMarkdownDocument};

struct FrozenLiveSourceCache {
    identity: WeakMarkdownDocument,
    generation: u64,
    length: usize,
    source: FrozenNativeSourceWeak,
}

pub(super) struct FrozenLiveValues {
    family: Arc<FrozenValueFamily>,
    source: RefCell<Option<FrozenLiveSourceCache>>,
    sealed: Cell<bool>,
}

impl FrozenLiveValues {
    pub(super) fn new_if_requested() -> Option<Self> {
        // Read the opt-in once at live-adapter construction. Later environment changes cannot
        // switch an existing session to different copy semantics.
        (std::env::var_os("OMNI_NATIVE_FROZEN_VALUES").as_deref()
            == Some(std::ffi::OsStr::new("1")))
        .then(Self::enabled)
    }

    fn enabled() -> Self {
        // One adapter owns one family and a weak-only cache; an idle cache captures no snapshot.
        Self {
            family: FrozenValueFamily::new(),
            source: RefCell::new(None),
            sealed: Cell::new(false),
        }
    }

    pub(super) fn family(&self) -> Arc<FrozenValueFamily> {
        Arc::clone(&self.family) // Share the original family, without another Source/value inventory or business owner.
    }

    pub(super) fn seal(&self) {
        // The accepted-release sequence already detached the native graph. Reject new live
        // captures before returning this weak cache. Existing immutable values keep their original
        // Source claims, so later valid copy/substring does not revoke their contents.
        self.sealed.set(true);
        let obsolete = self.source.borrow_mut().take();
        drop(obsolete);
        self.family.seal_sources();
    }

    pub(super) fn live_cache_is_released(&self) -> bool {
        // This proves only closed live admission and actual removal of the weak cache. The
        // original family independently observes final destruction of outstanding derived values.
        self.sealed.get() && self.source.borrow().is_none()
    }

    pub(super) fn copy_value(
        &self,
        document: &RefCell<WeakMarkdownDocument>,
    ) -> Retained<NSString> {
        // Capture one immutable Source before entering Foundation. The full-range value shares
        // its snapshot; a later native bulk request still executes its actual declared length.
        let source = self.capture_live_source(document);
        match source {
            Some(source) => {
                let length = source.utf16_len();
                self.string_value(source, 0..length)
            }
            None => ns_string!("").retain(), // Only an actually expired original document makes the live shell an empty value.
        }
    }

    pub(super) fn substring_value(
        &self,
        document: &RefCell<WeakMarkdownDocument>,
        range: NSRange,
    ) -> Retained<NSString> {
        // Retain exact receiver coordinates, including UTF-16 positions inside surrogate pairs;
        // invalid requests do not fall back to another document or an empty successful value.
        let source = self.capture_live_source(document);
        match source {
            Some(source) => {
                let end = range
                    .location
                    .checked_add(range.length)
                    .unwrap_or_else(|| throw_range());
                self.string_value(source, range.location..end)
            }
            None if range.location == 0 && range.length == 0 => ns_string!("").retain(),
            None => throw_range(), // An empty live receiver cannot redirect an old nonempty request to another Source or claim success.
        }
    }

    fn capture_live_source(
        &self,
        document: &RefCell<WeakMarkdownDocument>,
    ) -> Option<FrozenNativeSource> {
        if MainThreadMarker::new().is_none() {
            // Live documents remain main-thread RefCells. Background consumers may use already
            // frozen values, but cannot upgrade the live document owner across threads.
            throw_storage();
        }
        let identity = document.borrow().clone();
        let document = identity.upgrade()?;
        if self.sealed.get() {
            // Sealing while the original DocWeak still upgrades is not an empty document. Report
            // the accepted-release ordering violation rather than silently returning empty text.
            throw_storage();
        }
        let source = self
            .source_for_document(&document)
            .unwrap_or_else(|| throw_storage());
        drop(document); // End the live document claim and all RefCell borrows before constructing any native value.
        Some(source)
    }

    pub(super) fn source_for_document(
        &self,
        document: &SharedMarkdownDocument,
    ) -> Option<FrozenNativeSource> {
        if self.sealed.get() {
            return None; // Reject new live captures only; reads of previously derived immutable values stay valid.
        }
        let document_ref = document.try_borrow().ok()?;
        let identity = std::rc::Rc::downgrade(document);
        let generation = document_ref.generation();
        let length = document_ref.utf16_len();
        let cached = {
            let slot = self.source.borrow();
            slot.as_ref().and_then(|cached| {
                // Equal numeric generations do not identify a document. Original weak allocation
                // identity prevents another note's same-generation/same-length ABA from reusing it.
                (cached.identity.ptr_eq(&identity)
                    && cached.generation == generation
                    && cached.length == length)
                    .then(|| cached.source.upgrade())
                    .flatten()
            })
        };
        if let Some(source) = cached {
            if source.generation() != generation || source.utf16_len() != length {
                // A cache hit still checks actual opaque Source metadata. Reject inconsistent
                // metadata instead of recapturing and masking the wrong Source association.
                return None;
            }
            return Some(source); // Share the same complete opaque Data and Source claim, without recapturing a snapshot.
        }
        let source = FrozenNativeSource::capture(
            &document_ref,
            &self.family,
            Some(super::string_read_diagnostic::observe_frozen_read),
        )
        .ok()?;
        drop(document_ref);
        let obsolete = self.source.replace(Some(FrozenLiveSourceCache {
            identity,
            generation,
            length,
            source: source.downgrade(),
        }));
        drop(obsolete); // This cache owns only Weak; the family observes actual release of old generations still held by system values.
        Some(source)
    }

    fn string_value(&self, source: FrozenNativeSource, range: Range<usize>) -> Retained<NSString> {
        match FrozenMarkdownString::new(source, range) {
            Ok(value) => value.into_super(),
            Err(FrozenMarkdownStringError::RangeOutsideSnapshot) => throw_range(),
            Err(_) => throw_storage(), // Actual family/class invariant failure cannot be hidden by a whole-document-copy fallback.
        }
    }
}

fn throw_range() -> ! {
    // Receiver-bound errors keep the same public Foundation exception contract as immutable values.
    super::text_storage::throw_markdown_native_exception(
        ns_string!("NSRangeException"),
        ns_string!("live字符串的不可变子范围越过原接收者"),
    )
}

fn throw_storage() -> ! {
    // Reuse the existing public storage-unavailable exception boundary instead of another factory
    // solely for these experimental values.
    super::text_storage::throw_markdown_storage_unavailable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_markdown_core::{MarkdownDocumentBuffer, MarkdownTextSelection};

    fn fixture(text: &str) -> (tempfile::TempDir, SharedMarkdownDocument) {
        // This Rust-only control writes its own finite input. It neither constructs Foundation
        // values nor accesses user notes; its family observations do not prove native copy behavior.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("frozen-live-source.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        (directory, std::rc::Rc::new(RefCell::new(document)))
    }

    #[test]
    fn same_generation_reuses_live_source_without_extra_payload_claim() {
        // Inspect reuse of the actual Core snapshot allocation while values still own it, then
        // verify that the weak-only cache cannot extend the payload lifetime on its own.
        let (_directory, document) = fixture("前😀后");
        let values = FrozenLiveValues::enabled();
        let source = values.source_for_document(&document).unwrap();
        let weak = source.downgrade();
        let again = values.source_for_document(&document).unwrap();
        assert_eq!(values.family.snapshot().constructed[0], 1);
        drop(source);
        drop(again);
        assert!(weak.upgrade().is_none()); // The weak cache cannot keep the Source payload resident.
        assert_eq!(values.family.snapshot().active, [0; 4]);
    }

    #[test]
    fn new_generation_and_same_numeric_generation_other_document_keep_distinct_sources() {
        // Two independent real documents start at equal numeric generations. Editing one through
        // Core changes only its Source metadata; neither identity may be inferred from generation alone.
        let (_first_directory, first) = fixture("a😀b");
        let (_second_directory, second) = fixture("c😀d");
        let values = FrozenLiveValues::enabled();
        let first_source = values.source_for_document(&first).unwrap();
        let second_source = values.source_for_document(&second).unwrap();
        assert_eq!(values.family.snapshot().constructed[0], 2);
        first
            .borrow_mut()
            .replace_utf16_range(0..1, "新版", MarkdownTextSelection::caret(0))
            .unwrap();
        let changed = values.source_for_document(&first).unwrap();
        assert_eq!(values.family.snapshot().constructed[0], 3);
        assert_ne!(changed.generation(), first_source.generation());
        assert_eq!(first_source.utf16_len(), second_source.utf16_len());
        assert_ne!(changed.utf16_len(), first_source.utf16_len());
    }

    #[test]
    fn seal_clears_weak_cache_and_blocks_capture_without_revoking_existing_source() {
        // Source sealing ends live admission while the already captured immutable Source keeps
        // its content until its last actual claim is dropped.
        let (_directory, document) = fixture("旧😀值");
        let values = FrozenLiveValues::enabled();
        let source = values.source_for_document(&document).unwrap();
        let weak = source.downgrade();
        values.seal();
        assert!(values.live_cache_is_released());
        assert!(values.source_for_document(&document).is_none());
        assert_eq!(source.utf16_len(), 4);
        assert!(weak.upgrade().is_some());
        assert!(!values.family.snapshot().payload_value_destructors_complete);
        drop(source);
        assert!(weak.upgrade().is_none());
        assert!(values.family.snapshot().payload_value_destructors_complete);
    }

    #[test]
    fn upgraded_source_metadata_mismatch_rejects_without_recapture() {
        // This existing private-cache mutation tests rejection of an inconsistent association.
        // It is not a Native callback or an externally reachable way to install another Source.
        let (_directory, document) = fixture("a😀b");
        let (_other_directory, other_document) = fixture("other😀");
        let values = FrozenLiveValues::enabled();
        let source = values.source_for_document(&document).unwrap();
        let other =
            FrozenNativeSource::capture(&other_document.borrow(), &values.family, None).unwrap();
        values.source.borrow_mut().as_mut().unwrap().source = other.downgrade();
        let before = values.family.snapshot().constructed;
        assert!(values.source_for_document(&document).is_none());
        assert_eq!(values.family.snapshot().constructed, before);
        assert_eq!(source.utf16_len(), 4); // The inconsistent cache entry neither rewrites nor revokes the original immutable value.
    }
}
