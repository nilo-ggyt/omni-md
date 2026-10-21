//! Bounded owner of ordered system font candidates, receiving no text and performing no missing-glyph
//! decision or shaping.
//!
//! Core Text supplies descriptor order and best-match construction only. Every actual CTFont still
//! passes the original
//! PagedFont file, face, collection, and axis checks; candidate existence does not certify correct
//! grapheme display.
use std::{
    cell::Cell,
    rc::{Rc, Weak},
};

use objc2_core_foundation::{CFArray, CFRetained, CFString, CFType, Type};
use objc2_core_text::{CTFont, CTFontDescriptor, CTFontOptions};
use objc2_foundation::MainThreadMarker;
use omni_markdown_core::MarkdownLayoutFontAuthority;
use thiserror::Error;

use super::paged_fonts::{
    NativeMarkdownFontCapture, NativeMarkdownPagedFont, NativeMarkdownPagedFontError,
};

/// Reject a system list above this hard bound instead of silently dropping its final fallback fonts.
pub const NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES: usize = 256;
const MAX_LANGUAGES: usize = 8;
const MAX_LANGUAGE_BYTES: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownFontCascadeBudget {
    pub maximum_candidates: usize,
}
impl Default for NativeMarkdownFontCascadeBudget {
    fn default() -> Self {
        Self {
            maximum_candidates: NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES,
        }
    }
}

#[derive(Debug, Error)]
pub enum NativeMarkdownFontCascadeError {
    #[error("字体候选配额无效")]
    InvalidBudget,
    #[error("字体语言偏好不是有限有效语言标识符列表")]
    InvalidLanguages,
    #[error("原字体payload权限不匹配")]
    FontAuthorityMismatch,
    #[error("原字体候选权限已撤销")]
    Revoked,
    #[error("系统字体候选列表不可用")]
    Unavailable,
    #[error("系统字体候选数{actual}超过明确配额{maximum}")]
    CandidateBudgetExceeded { actual: usize, maximum: usize },
    #[error("系统候选不是真实字体描述符")]
    InvalidDescriptor,
    #[error("无法捕获候选字体的精确来源")]
    Font(#[from] NativeMarkdownPagedFontError),
}
type Result<T> = std::result::Result<T, NativeMarkdownFontCascadeError>;

/// Retain weak Core identity and sticky revocation only, without the base, font bytes, View, document,
/// or caller closure.
struct CascadePermit {
    original: MarkdownLayoutFontAuthority,
    revoked: Cell<bool>,
}
impl CascadePermit {
    /// Check sticky revocation and the same actual base-payload identity before consulting caller
    /// authority.
    fn check(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        if self.revoked.get() {
            return Err(NativeMarkdownFontCascadeError::Revoked);
        }
        if !base.core_font().permits(&self.original) || !base.core_font().permits(authority) {
            self.revoked.set(true);
            return Err(NativeMarkdownFontCascadeError::FontAuthorityMismatch);
        } // A new same-name, same-CTFont, or same-axis capture still lacks the original Core allocation authority.
        if !current() {
            self.revoked.set(true);
            return Err(NativeMarkdownFontCascadeError::Revoked);
        }
        Ok(())
    }

    /// Guard each Native acquisition/release and return any acquired value on post-call revocation.
    fn native<T>(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
        operation: impl FnOnce() -> T,
    ) -> Result<T> {
        self.check(base, authority, current)?;
        let value = operation();
        if let Err(error) = self.check(base, authority, current) {
            drop(value); // Return the actual +1 temporary on revocation rather than publishing it under a successor ticket.
            return Err(error);
        }
        Ok(value)
    }
}

/// Session-local ordered CFArray holds metadata only, without creating/caching font-file mappings for
/// every candidate.
///
/// MainThreadMarker preserves PagedFont's Native thread boundary. Destruction returns this owner's CF
/// claims only,
/// without claiming shared framework descriptors/fonts were destroyed. Later composition owns text and
/// missing-glyph decisions.
pub struct NativeMarkdownFontCascade {
    descriptors: CFRetained<CFArray>,
    permit: Rc<CascadePermit>,
    count: usize,
    next: usize,
    point_size: f64,
    mtm: MainThreadMarker,
}
impl NativeMarkdownFontCascade {
    /// Capture actual system order using fixed configured languages, never probing fonts with note
    /// contents.
    ///
    /// # Errors
    /// Reject invalid quota/languages/original-payload authority, failed system copy, or excess
    /// candidates without a truncated list.
    pub fn capture(
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        languages: &[&'static str],
        budget: NativeMarkdownFontCascadeBudget,
        mtm: MainThreadMarker,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Self> {
        if !(1..=NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES).contains(&budget.maximum_candidates) {
            return Err(NativeMarkdownFontCascadeError::InvalidBudget);
        }
        if languages.len() > MAX_LANGUAGES
            || languages.iter().any(|language| {
                language.is_empty()
                    || language.len() > MAX_LANGUAGE_BYTES
                    || !language
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
        {
            return Err(NativeMarkdownFontCascadeError::InvalidLanguages);
        } // Validate bounded language-tag syntax only; arbitrary UTF-8 note text never enters the system font query.
        let permit = Rc::new(CascadePermit {
            original: authority.clone(),
            revoked: Cell::new(false),
        });
        permit.check(base, authority, current)?; // Validate the actual same Core payload before any CF construction.
        let mut values = Vec::with_capacity(languages.len());
        for language in languages {
            values.push(permit.native(base, authority, current, || CFString::from_str(language))?);
        } // This vector holds at most eight fixed languages, independently of document size or system font count.
        let language_array = permit.native(base, authority, current, || {
            CFArray::<CFString>::from_retained_objects(&values)
        })?;
        // SAFETY: The original base and bounded CFString array cover the Copy call; the returned array
        // owns its descriptors.
        let descriptors = permit
            .native(base, authority, current, || unsafe {
                base.native_font()
                    .default_cascade_list_for_languages(Some(language_array.as_opaque()))
            })?
            .ok_or(NativeMarkdownFontCascadeError::Unavailable)?;
        let count = permit.native(base, authority, current, || descriptors.len())?;
        if count > budget.maximum_candidates {
            return Err(NativeMarkdownFontCascadeError::CandidateBudgetExceeded {
                actual: count,
                maximum: budget.maximum_candidates,
            });
        } // Return the complete list on failure; a bounded prefix cannot masquerade as the full system cascade.
        permit.native(base, authority, current, || drop(language_array))?;
        for value in values {
            permit.native(base, authority, current, || drop(value))?;
        } // Recheck authority after returning language temporaries before publishing the externally visible list owner.
        permit.check(base, authority, current)?;
        Ok(Self {
            descriptors,
            permit,
            count,
            next: 0,
            point_size: base.point_size(),
            mtm,
        })
    }

    /// Return the actual system-list count including unconstructed fonts, never a count of supported
    /// graphemes.
    #[must_use]
    pub fn candidate_count(&self) -> usize {
        self.count
    }

    /// Deliver original list entries in order, without sorting, name-based deduplication, character
    /// scans, or implicit retries.
    ///
    /// # Errors
    /// Revocation permanently ends this cursor; invalid CF types fail instead of being skipped in a
    /// supposedly complete traversal.
    pub fn next(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownFontCandidate>> {
        self.permit.check(base, authority, current)?;
        if self.next == self.count {
            return Ok(None); // End only after delivering the actual count; an empty system list does not mean font support is Ready.
        }
        let ordinal = self.next;
        // SAFETY: The immutable list stays alive and ordinal<count<=256; Core Text specifies CFType
        // descriptor entries.
        let raw = self.permit.native(base, authority, current, || unsafe {
            self.descriptors.value_at_index(ordinal as _)
        })?;
        if raw.is_null() {
            self.permit.revoked.set(true);
            return Err(NativeMarkdownFontCascadeError::InvalidDescriptor);
        }
        // SAFETY: The original list retains this nonnull CFType; verify its dynamic type before
        // obtaining an independent +1 claim.
        let value = unsafe { &*raw.cast::<CFType>() };
        let descriptor = self.permit.native(base, authority, current, || {
            value.downcast_ref::<CTFontDescriptor>()
        })?;
        let Some(descriptor) = descriptor else {
            self.permit.revoked.set(true);
            return Err(NativeMarkdownFontCascadeError::InvalidDescriptor);
        };
        let descriptor = self
            .permit
            .native(base, authority, current, || descriptor.retain())?;
        self.next += 1; // Consume this system ordinal only after original-ticket type checking and retain both succeed.
        Ok(Some(NativeMarkdownFontCandidate {
            descriptor,
            permit: Rc::downgrade(&self.permit),
            ordinal,
            point_size: self.point_size,
            mtm: self.mtm,
        }))
    }

    /// Stop issuing candidates and return the original CFArray; existing immutable descriptors stay
    /// intact but cannot construct new fonts.
    pub fn close(self) {
        drop(self);
    }
}
impl Drop for NativeMarkdownFontCascade {
    fn drop(&mut self) {
        self.permit.revoked.set(true); // Revoke every weak candidate before fields return the original list and sole permit.
    }
}

/// One actual descriptor and weak issuer identity retain neither the cascade list nor base font.
pub struct NativeMarkdownFontCandidate {
    descriptor: CFRetained<CTFontDescriptor>,
    permit: Weak<CascadePermit>,
    ordinal: usize,
    point_size: f64,
    mtm: MainThreadMarker,
}
impl NativeMarkdownFontCandidate {
    /// Original system-array ordinal, not a font name, glyph ID, or script range.
    #[must_use]
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }
    /// Lend the immutable descriptor for finite metadata observation only, without new
    /// capture/text/paint authority.
    /// After cascade close it remains immutable; release verification must not force it into an empty
    /// object.
    #[must_use]
    pub fn descriptor(&self) -> &CTFontDescriptor {
        &self.descriptor
    }

    /// Construct one actual best-match font on demand, then reuse exact Native/Core Source capture.
    ///
    /// # Errors
    /// Report ended cascade, revoked original font, or failed Native capture without guessing missing
    /// glyphs or changing grapheme boundaries.
    pub fn capture(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        maximum_font_bytes: u64,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedFont> {
        let (permit, native) = self.construct(base, authority, current)?;
        let valid = || permit.check(base, authority, current).is_ok();
        let captured =
            NativeMarkdownPagedFont::capture(&native, maximum_font_bytes, self.mtm, &valid);
        permit.native(base, authority, current, || drop(native))?;
        permit.check(base, authority, current)?;
        captured.map_err(Into::into) // A candidate failure never silently substitutes the next font; the Composer explicitly owns the next decision.
    }
    /// The active Body reuses this actual candidate's existing capture while preserving cascade ordinal
    /// and original grapheme-trial order.
    pub(crate) fn capture_reusing(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        palette: &[Rc<super::paged_line_fonts::NativeMarkdownLineFont>],
        maximum_font_bytes: u64,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontCapture> {
        let (permit, native) = self.construct(base, authority, current)?;
        let valid = || permit.check(base, authority, current).is_ok();
        let captured = NativeMarkdownPagedFont::capture_reusing(
            &native,
            palette,
            maximum_font_bytes,
            self.mtm,
            &valid,
        );
        permit.native(base, authority, current, || drop(native))?;
        permit.check(base, authority, current)?;
        captured.map_err(Into::into)
    }

    /// Standalone capture and active-palette reuse share actual best-match construction; Native entry
    /// requires the original live weak permit.
    fn construct(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<(Rc<CascadePermit>, CFRetained<CTFont>)> {
        let permit = self
            .permit
            .upgrade()
            .ok_or(NativeMarkdownFontCascadeError::Revoked)?;
        permit.check(base, authority, current)?;
        // SAFETY: The original strong descriptor and finite positive size come from the base; the null
        // matrix references no external storage.
        // Preventing autoactivation/download only constrains candidate acquisition; original capture
        // still fully verifies actual best-match identity.
        let native = permit.native(base, authority, current, || unsafe {
            CTFont::with_font_descriptor_and_options(
                &self.descriptor,
                self.point_size,
                std::ptr::null(),
                CTFontOptions::PreventAutoActivation | CTFontOptions::PreventAutoDownload,
            )
        })?;
        Ok((permit, native))
    }
}
