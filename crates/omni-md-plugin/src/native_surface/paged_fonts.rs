//! Captures actual Native font sources for paginated candidates without creating windows, document text, CTLines, or a global font cache.
use std::path::PathBuf;

use objc2_core_foundation::{CFData, CFRetained, Type};
use objc2_core_graphics::CGFont;
use objc2_core_text::CTFont;
use objc2_foundation::MainThreadMarker;
use omni_markdown_core::{
    MarkdownLayoutFont, MarkdownLayoutFontAxis, MarkdownLayoutFontBytes,
    MarkdownLayoutFontBytesError, MarkdownLayoutFontBytesResourceObserver, MarkdownLayoutFontError,
};
use thiserror::Error;

mod metadata;

#[derive(Debug, Error)]
pub enum NativeMarkdownPagedFontError {
    #[error("原字体构造权限已撤销")]
    Revoked,
    #[error("原生字体来源元数据不可用或超出固定预算")]
    MetadataUnavailable,
    #[error("原生字体没有可安全捕获的本地文件来源")]
    FileSourceUnavailable,
    #[error("原生字体与CGFont往返来源不一致")]
    GraphicsSourceMismatch,
    #[error("原生字体与Core精确face或轴位置不一致")]
    CoreFaceMismatch,
    #[error("原生字体轴请求重复、过多或没有被实际接受")]
    InvalidVariation,
    #[error("无法为原生字体的有界水平度量分配存储")]
    HorizontalMetricsAllocation,
    #[error("无法捕获原生字体的不可变字节")]
    Bytes(#[from] MarkdownLayoutFontBytesError),
    #[error("无法解析原生字体的精确Core face")]
    Core(#[from] MarkdownLayoutFontError),
}
type Result<T> = std::result::Result<T, NativeMarkdownPagedFontError>;

/// Guards every Native primitive with the original caller authority, without recapture, retry, or substitution by a newer Font.
fn step<T>(current: &(impl Fn() -> bool + ?Sized), operation: impl FnOnce() -> T) -> Result<T> {
    if !current() {
        return Err(NativeMarkdownPagedFontError::Revoked);
    }
    let value = operation();
    if !current() {
        drop(value); // Returns the temporary Native claim on this stack; revoked work cannot transfer it to a successor owner.
        return Err(NativeMarkdownPagedFontError::Revoked);
    }
    Ok(value)
}

/// Owns the original Native font and same-source Core font for an explicit run; derived axes require the actual construction edge.
///
/// CTFont/CGFont are framework-shared immutable values; this module does not prove destruction of their system caches.
/// The owned snapshot observer proves actual project close/munmap facts only; shared font caches and whole-product acceptance require independent verification.
#[derive(Clone)]
pub struct NativeMarkdownPagedFont {
    native: CFRetained<CTFont>,
    constructor_graphics: CFRetained<CGFont>,
    // Keeps the same CGFont's bounded immutable name table for axis derivation, without reinterpreting UIFont or synthesized PostScript names.
    name_table: CFRetained<CFData>,
    bytes: MarkdownLayoutFontBytes,
    core: MarkdownLayoutFont,
    constructor_postscript: String,
    source_path: PathBuf,
    point_size: f64,
    _main_thread: MainThreadMarker,
}

/// Fresh snapshots, complete reuse, and shared bytes have different release duties; a mapping still used by the original Body is not a leak.
pub(crate) enum NativeMarkdownFontCapture {
    Fresh(NativeMarkdownPagedFont),
    Reused(NativeMarkdownPagedFont),
    // A new Native/Core instance shares this Body's existing frozen bytes; rejecting it must not require destruction of a mapping still in use.
    SharedBytes(NativeMarkdownPagedFont),
}
impl NativeMarkdownFontCapture {
    /// Borrows this verified Native/Core instance; shared bytes do not bypass actual shaping of the original grapheme.
    pub(crate) fn font(&self) -> &NativeMarkdownPagedFont {
        match self {
            Self::Fresh(font) | Self::Reused(font) | Self::SharedBytes(font) => font,
        }
    }

    /// Selected transfers actual ownership to Body; full reuse preserves Core identity, while new instances follow normal registration.
    pub(crate) fn into_font(self) -> NativeMarkdownPagedFont {
        match self {
            Self::Fresh(font) | Self::Reused(font) | Self::SharedBytes(font) => font,
        }
    }

    /// Only this capture's fresh mapping must end immediately on rejection/failure; reused mappings remain the original Body's final-close obligation.
    pub(crate) fn fresh_bytes_observer(&self) -> Option<MarkdownLayoutFontBytesResourceObserver> {
        match self {
            Self::Fresh(font) => Some(font.bytes_observer()),
            Self::Reused(_) | Self::SharedBytes(_) => None,
        }
    }
}

impl NativeMarkdownPagedFont {
    /// Preserves size and weight chosen by the system factory, removing UI usage binding only through the same actual CGFont.
    /// Does not match by name, switch families, or grant Core Font or Source authority before actual capture.
    pub(crate) fn document_instance(font: &CTFont) -> Result<CFRetained<CTFont>> {
        metadata::document_instance(font)
    }

    /// Verifies actual family, semantics, axes, and geometry after style derivation, without resolving a private PostScript name to another default font.
    pub(crate) fn require_derived_style(
        original: &CTFont,
        derived: &CTFont,
        points: f64,
        italic: bool,
    ) -> Result<()> {
        metadata::require_derived_style(original, derived, points, italic)
    }

    /// Captures an existing CTFont's original CGFont name table and actual axes; the complete table resolves one file face without interpreting UIFont aliases.
    ///
    /// # Errors
    /// Rejects revoked authority, unavailable sources, unresolved faces, inconsistent round trips, or Core parsing failure without guessing face zero.
    pub fn capture(
        font: &CTFont,
        maximum_font_bytes: u64,
        mtm: MainThreadMarker,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Self> {
        let original = metadata::read(font, current)?;
        let bytes = step(current, || {
            MarkdownLayoutFontBytes::capture_file(&original.path, maximum_font_bytes)
        })??;
        Self::capture_metadata(font, original, bytes, None, mtm, current)
    }

    /// Reuses only an actually selected system candidate, preserving cascade order, exact identity, and the subsequent original grapheme trial.
    pub(crate) fn capture_reusing(
        font: &CTFont,
        palette: &[std::rc::Rc<super::paged_line_fonts::NativeMarkdownLineFont>],
        maximum_font_bytes: u64,
        mtm: MainThreadMarker,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontCapture> {
        // The system cascade may return different references to the same immutable font for later graphemes.
        // Initial palette capture already verified CGFont, complete names, axes, size, and matrix;
        // Core Foundation font-value equality permits reusing that capture without per-grapheme URLs, descriptors, or temporary fonts.
        // Searches only the current Body's bounded palette, without cross-document caching or name-based source inference.
        // CFEqual follows each CF type's value equality; the referenced CoreText reuse implementation also compares CTFont values.
        // https://skia.googlesource.com/skia/+/e24127ce05ed26a1651235f268cc9a39840f924a/src/ports/SkTypeface_mac_ct.cpp
        // The borrowed Body palette remains under the original caller intent; the actual grapheme-shaping trial still follows.
        for registered in palette {
            let captured = registered.native();
            if step(current, || font == captured.native_font())? {
                return step(current, || {
                    NativeMarkdownFontCapture::Reused(captured.clone())
                });
            }
        }
        let original = metadata::read(font, current)?;
        for registered in palette {
            let captured = registered.native();
            if original.matches_capture(font, captured, current)? {
                // Clone retains the same CTFont/Core payload/anonymous mapping without reopening, resolving, or consuming a registration slot.
                return step(current, || {
                    NativeMarkdownFontCapture::Reused(captured.clone())
                });
            }
        }
        // One source file may produce multiple sizes or axis instances; shares immutable bytes only, with full source checks for every new instance.
        // Traverses only this Body's existing bounded palette, without a global inventory or ownership beyond the Body.
        let mut shared_bytes = None;
        for registered in palette {
            let captured = registered.native();
            if captured.source_path != original.path {
                continue;
            }
            let shared = step(current, || {
                captured
                    .bytes
                    .share_unchanged_file(&original.path, maximum_font_bytes)
            })??;
            if let Some(bytes) = shared {
                // Matching file identity permits sharing these frozen bytes only; another face in the same TTC may have a different name table.
                // Derives from this peer's Core instance and shares parsed layout only when its complete original name table also matches.
                if step(current, || {
                    Ok::<_, NativeMarkdownPagedFontError>(
                        original.name_bytes()? == metadata::name_bytes(&captured.name_table)?,
                    )
                })?? {
                    return Self::capture_metadata(
                        font,
                        original,
                        bytes,
                        Some(&captured.core),
                        mtm,
                        current,
                    )
                    .map(NativeMarkdownFontCapture::SharedBytes);
                }
                // Continues seeking a same-face peer; otherwise the complete original resolver must determine the new face.
                // Retains at most one existing mapping claim, without a new font-file inventory or process-wide cache.
                if shared_bytes.is_none() {
                    shared_bytes = Some(bytes);
                }
            }
        }
        if let Some(bytes) = shared_bytes {
            return Self::capture_metadata(font, original, bytes, None, mtm, current)
                .map(NativeMarkdownFontCapture::SharedBytes);
        }
        let bytes = step(current, || {
            MarkdownLayoutFontBytes::capture_file(&original.path, maximum_font_bytes)
        })??;
        Self::capture_metadata(font, original, bytes, None, mtm, current)
            .map(NativeMarkdownFontCapture::Fresh)
    }

    /// Consumes frozen bytes delivered under the same intent, verifying exact face, axes, and size per instance rather than inferring Native identity from a shared file.
    fn capture_metadata(
        font: &CTFont,
        original: metadata::Metadata,
        bytes: MarkdownLayoutFontBytes,
        same_face: Option<&MarkdownLayoutFont>,
        mtm: MainThreadMarker,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Self> {
        let name_table = step(current, || original.name_bytes())??;
        let horizontal =
            metadata::horizontal_advances(font, &original.graphics, original.glyph_count, current)?;
        let core = step(current, || {
            // Complete advances belong to this actual Native size/axis instance. Core subtracts the same
            // Font's nominal tracking before positioning reapplies it per grapheme; UI defaults are not document metrics.
            match (same_face, horizontal) {
                (Some(previous), Some(advances)) => previous
                    .derive_name_table_at_size_with_horizontal_advances(
                        name_table,
                        &original.axes,
                        original.point_size,
                        advances,
                    ),
                (Some(previous), None) => previous.derive_name_table_at_size(
                    name_table,
                    &original.axes,
                    original.point_size,
                ),
                (None, Some(advances)) => {
                    MarkdownLayoutFont::resolve_name_table_at_size_with_horizontal_advances(
                        bytes.blob(),
                        name_table,
                        &original.axes,
                        original.point_size,
                        advances,
                    )
                }
                (None, None) => MarkdownLayoutFont::resolve_name_table_at_size(
                    bytes.blob(),
                    name_table,
                    &original.axes,
                    original.point_size,
                ),
            }
        })??;
        original.verify_core(&core)?;
        let native = step(current, || font.retain())?;
        step(current, || ())?; // Rechecks the same intent after Native temporaries return; publication cannot acquire newer authority.
        let constructor_postscript = core.metadata().postscript_name.clone();
        Ok(Self {
            native,
            constructor_graphics: original.graphics,
            name_table: original.name_table,
            bytes,
            core,
            constructor_postscript,
            source_path: original.path,
            point_size: original.point_size,
            _main_thread: mtm,
        })
    }

    /// Constructs new axis instances from this owner's original CGFont, without inferring a source from foreign synthesized PostScript names.
    ///
    /// # Errors
    /// Rejects duplicate, unknown, or out-of-range axes, unaccepted Native requests, changed sources, and revoked original authority.
    pub fn derive_axes(
        &self,
        overrides: &[MarkdownLayoutFontAxis],
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Self> {
        if overrides.len() > 32 {
            return Err(NativeMarkdownPagedFontError::InvalidVariation);
        }
        let mut requested = overrides.to_vec();
        requested.sort_unstable_by_key(|axis| axis.tag);
        if requested.windows(2).any(|pair| pair[0].tag == pair[1].tag) {
            return Err(NativeMarkdownPagedFontError::InvalidVariation);
        }
        let mut complete = self.core.metadata().axes.clone();
        for axis in requested {
            if let Some(existing) = complete
                .iter_mut()
                .find(|existing| existing.tag == axis.tag)
            {
                existing.value = axis.value;
            } else {
                complete.push(axis); // The original Core resolver explicitly rejects unknown axes rather than silently ignoring them.
            }
        }
        let name_table = step(current, || metadata::name_bytes(&self.name_table))??;
        let expected = step(current, || {
            MarkdownLayoutFont::resolve_name_table(self.bytes.blob(), name_table, &complete)
        })??;
        // SAFETY: The original main-thread owner retains self.native; the public getter returns an independent +1 descriptor.
        let mut descriptor = step(current, || unsafe { self.native.font_descriptor() })?;
        for axis in &expected.metadata().axes {
            let identifier = step(current, || {
                objc2_core_foundation::CFNumber::new_i64(i64::from(u32::from_be_bytes(axis.tag)))
            })?;
            // SAFETY: identifier is a public numeric OpenType tag, and Core validated the value against the actual face's finite range.
            let next = step(current, || unsafe {
                descriptor.copy_with_variation(&identifier, f64::from(axis.value))
            })?;
            step(current, || drop(std::mem::replace(&mut descriptor, next)))?;
            step(current, || drop(identifier))?; // CFRelease also completes under the original intent, without spanning a RefCell borrow.
        }
        // SAFETY: The original CGFont claim spans construction; the matrix pointer is null and the descriptor carries this owner's complete axes.
        let native = step(current, || unsafe {
            CTFont::with_graphics_font(
                &self.constructor_graphics,
                self.point_size,
                std::ptr::null(),
                Some(&descriptor),
            )
        })?;
        step(current, || drop(descriptor))?;
        let actual = metadata::read(&native, current)?;
        if actual.path != self.source_path {
            return Err(NativeMarkdownPagedFontError::GraphicsSourceMismatch); // Derivation cannot resolve to a different font file.
        }
        let name_table = step(current, || actual.name_bytes())??;
        let horizontal =
            metadata::horizontal_advances(&native, &actual.graphics, actual.glyph_count, current)?;
        let core = step(current, || {
            // Recaptures the axis instance's actual Native metrics instead of borrowing old-axis data or inheriting old glyph widths.
            match horizontal {
                Some(advances) => {
                    MarkdownLayoutFont::resolve_name_table_at_size_with_horizontal_advances(
                        self.bytes.blob(),
                        name_table,
                        &actual.axes,
                        actual.point_size,
                        advances,
                    )
                }
                None => MarkdownLayoutFont::resolve_name_table_at_size(
                    self.bytes.blob(),
                    name_table,
                    &actual.axes,
                    actual.point_size,
                ),
            }
        })??;
        actual.verify_core(&core)?;
        if core.metadata().face_index != self.core.metadata().face_index
            || core.metadata().axes != expected.metadata().axes
        {
            return Err(NativeMarkdownPagedFontError::InvalidVariation); // Requesting axes does not prove Native actually accepted them.
        }
        let constructor_graphics = step(current, || self.constructor_graphics.clone())?;
        let name_table = step(current, || actual.name_table.clone())?;
        step(current, || drop(actual))?;
        step(current, || drop(expected))?;
        Ok(Self {
            native,
            constructor_graphics,
            name_table,
            bytes: self.bytes.clone(),
            core,
            constructor_postscript: self.constructor_postscript.clone(),
            source_path: self.source_path.clone(),
            point_size: self.point_size,
            _main_thread: self._main_thread,
        })
    }

    #[must_use]
    pub fn core_font(&self) -> &MarkdownLayoutFont {
        &self.core
    }
    #[must_use]
    pub fn native_font(&self) -> &CTFont {
        &self.native
    }
    /// Returns the size checked against actual CTFont during capture, allowing same-face/different-size drawing to be rejected.
    #[must_use]
    pub fn point_size(&self) -> f64 {
        self.point_size
    }
    /// Public shaping options retain their f32 protocol; the same Core instance binds the original CTFont's f64 size for internal scaling.
    /// Requires its unique f32 encoding and the original binding, avoiding false rejection of sizes such as 0.1 or same-face size substitution.
    #[must_use]
    pub fn accepts_shaping_size(&self, points: f32) -> bool {
        self.core.shaping_point_size() == Some(self.point_size)
            && (self.point_size as f32).to_bits() == points.to_bits()
    }
    #[must_use]
    pub fn bytes_observer(&self) -> MarkdownLayoutFontBytesResourceObserver {
        self.bytes.resource_observer()
    }
    #[must_use]
    pub fn source_face_postscript(&self) -> &str {
        &self.constructor_postscript
    }

    /// Returns file identity verified against the same CGFont/name table at capture, for bounded source diagnostics and reproducible controls.
    /// This path grants no authority to reopen a file or replace the current Core/Native payload with a same-name font.
    #[must_use]
    pub fn source_path(&self) -> &std::path::Path {
        &self.source_path
    }
}
