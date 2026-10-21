//! Bounded CF metadata reads without document strings, CTLines, or unbounded dictionary collections.
use std::{ffi::c_void, os::unix::ffi::OsStrExt, path::PathBuf, ptr::NonNull};

use objc2_core_foundation::{
    CFData, CFNumber, CFRetained, CFString, CFStringBuiltInEncodings, CFType, CFURL, CGSize,
};
use objc2_core_graphics::CGFont;
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontOrientation, CTFontSymbolicTraits, kCTFontURLAttribute,
};
use omni_markdown_core::{MarkdownLayoutFont, MarkdownLayoutFontAxis};

use super::{NativeMarkdownPagedFontError as Error, Result, step};

/// Own the actual graphics/name table and bounded scalar facts until capture transfers or rejects them.
pub(super) struct Metadata {
    pub graphics: CFRetained<CGFont>,
    pub path: PathBuf,
    pub name_table: CFRetained<CFData>,
    pub axes: Vec<MarkdownLayoutFontAxis>,
    design_coordinates: Vec<([u8; 4], f64)>,
    pub point_size: f64,
    units_per_em: u32,
    pub(super) glyph_count: usize,
}
impl Metadata {
    pub(super) fn name_bytes(&self) -> Result<&[u8]> {
        name_bytes(&self.name_table)
    }
    /// Require actual CTFont CFEqual before verifying the original file, face, size, axes, and matrix;
    /// a PostScript name is insufficient.
    pub(super) fn matches_capture(
        &self,
        font: &CTFont,
        captured: &super::NativeMarkdownPagedFont,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<bool> {
        // CTFont PartialEq uses public CFEqual; a negative result rejects reuse and leaves the actual
        // capture path available.
        if !step(current, || font == captured.native.as_ref())?
            || self.path != captured.source_path
            || self.point_size.to_bits() != captured.point_size.to_bits()
            || self.name_bytes()? != name_bytes(&captured.name_table)?
            || !step(current, || *self.graphics == *captured.constructor_graphics)?
        {
            return Ok(false);
        }
        // SAFETY: Both immutable CTFonts remain strongly held on their original main thread; read Copy
        // scalars without document layout.
        let matrix = step(current, || unsafe { font.matrix() })?;
        // SAFETY: The capture retains its original CTFont throughout comparison; this getter lends no
        // external pointer.
        let original_matrix = step(current, || unsafe { captured.native.matrix() })?;
        // SAFETY: Read the actual candidate traits so a plain font cannot satisfy a bold, italic, or
        // monospaced request.
        let traits = step(current, || unsafe { font.symbolic_traits() })?;
        // SAFETY: The original owner retains the Native face; compare all trait bits rather than names
        // or expected style.
        let original_traits = step(current, || unsafe { captured.native.symbolic_traits() })?;
        if matrix != original_matrix
            || traits != original_traits
            || self.design_coordinates != read_axes(&captured.native, current)?
        {
            return Ok(false);
        }
        // Require original f64 axes to reproduce the captured Core normalized coordinates, after
        // checking complete names and glyph metadata.
        self.verify_core(&captured.core)?;
        step(current, || true)
    }

    /// Match parsed Core face metrics and normalized axes to the actual Native font, without name-based
    /// identity.
    pub(super) fn verify_core(&self, core: &MarkdownLayoutFont) -> Result<()> {
        let metadata = core.metadata();
        if self.units_per_em != u32::from(metadata.units_per_em)
            || self.glyph_count != metadata.glyph_count as usize
        {
            return Err(Error::CoreFaceMismatch); // Actual Native scalar checks cannot be replaced by a matching name or a self-reported face identity.
        }
        core.verify_design_coordinates(&self.design_coordinates)?;
        Ok(())
    }
}

/// The original platform instance determines actual advances for color fonts and outline fonts with
/// default tracking.
/// A document instance must not inherit file-level UI tracking defaults or substitute scaled hmtx
/// advances alone.
/// Capture bounded glyph-domain metadata with the original font lifetime, accepting no text, Source, or
/// layout results.
pub(super) fn horizontal_advances(
    font: &CTFont,
    graphics: &CGFont,
    glyph_count: usize,
    current: &(impl Fn() -> bool + ?Sized),
) -> Result<Option<Box<[f64]>>> {
    // SAFETY: The caller retains this immutable CTFont on its original main thread; read actual traits
    // without guessing by name.
    let traits = step(current, || unsafe { font.symbolic_traits() })?;
    if !traits.contains(CTFontSymbolicTraits::TraitColorGlyphs) {
        // Build an outline-face platform table only when the actual CGFont supplies both STAT and trak,
        // matching the original
        // HarfRust default-tracking admission. Ordinary fonts lacking either table keep their
        // file/variation metrics path,
        // without an unnecessary whole-glyph table for every font or guesses based on name, UI role, or
        // document characters.
        for tag in [*b"STAT", *b"trak"] {
            let table = step(current, || {
                CGFont::table_for_tag(Some(graphics), u32::from_be_bytes(tag))
            })?;
            let present = table.is_some();
            step(current, || drop(table))?; // Return each temporary table claim before reading glyph advances.
            if !present {
                return Ok(None);
            }
        }
    }
    let count = glyph_count;
    if !(1..=usize::from(u16::MAX) + 1).contains(&count) {
        return Err(Error::MetadataUnavailable); // CoreGraphics accepts actual u16 glyph IDs; never truncate a larger glyph domain.
    }
    // The font-domain table is bounded by u16 glyph IDs; reserve once and use fixed-size Native query
    // batches.
    let mut result = Vec::new();
    result
        .try_reserve_exact(count)
        .map_err(|_| Error::HorizontalMetricsAllocation)?;
    let mut glyphs = [0_u16; 256];
    let mut advances = [CGSize::ZERO; 256];
    for start in (0..count).step_by(glyphs.len()) {
        let used = glyphs.len().min(count - start);
        for (offset, glyph) in glyphs[..used].iter_mut().enumerate() {
            *glyph = u16::try_from(start + offset).map_err(|_| Error::MetadataUnavailable)?;
        }
        // SAFETY: Both fixed stack arrays provide used complete slots, and every glyph ID belongs to
        // this actual font domain.
        // Each batch holds at most 256 glyphs; post-call current checks discard the entire candidate
        // table on revocation.
        step(current, || unsafe {
            font.advances_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::new(glyphs.as_mut_ptr()).expect("固定glyph数组非空"),
                advances.as_mut_ptr(),
                used as _,
            )
        })?;
        for advance in &advances[..used] {
            if !advance.width.is_finite() || !advance.height.is_finite() || advance.height != 0.0 {
                return Err(Error::MetadataUnavailable); // Reject nonhorizontal or invalid platform metrics without substituting file defaults.
            }
            result.push(advance.width);
        }
    }
    step(current, || Some(result.into_boxed_slice())) // Revocation drops the complete table on this stack instead of publishing it to a later capture.
}

/// Borrow the immutable table from its retained CFData owner, without copying the complete font.
pub(super) fn name_bytes(data: &CFData) -> Result<&[u8]> {
    let count = usize::try_from(data.length()).map_err(|_| Error::MetadataUnavailable)?;
    let pointer = data.byte_ptr();
    if !(6..=1_048_576).contains(&count) || pointer.is_null() {
        return Err(Error::MetadataUnavailable);
    }
    // SAFETY: Immutable CopyTable CFData retains all count bytes; the bounded length and nonnull
    // pointer were checked above.
    // The slice lifetime follows the data borrow; its raw address must not survive the actual
    // CFRelease.
    Ok(unsafe { std::slice::from_raw_parts(pointer, count) })
}

/// Use the actual system font CG instance for document text, without carrying UI usage descriptors into
/// the document cascade.
/// Create an unpublished immutable font only, without Source, input authority, document contents, or
/// global registration.
pub(super) fn document_instance(font: &CTFont) -> Result<CFRetained<CTFont>> {
    // SAFETY: The caller retains the original NSFont/CTFont; this stack takes ownership of retained
    // getter results.
    let (graphics, points, matrix, traits) = unsafe {
        (
            font.graphics_font(std::ptr::null_mut()),
            font.size(),
            font.matrix(),
            font.symbolic_traits(),
        )
    };
    if !points.is_finite() || points <= 0.0 {
        return Err(Error::MetadataUnavailable);
    }
    // SAFETY: The original CGFont claim spans construction and matrix is a complete stack Copy; None
    // omits the UI descriptor.
    // AppKit factories determine actual size/weight first; do not match by name or inherit another size
    // optical axis here.
    let document = unsafe { CTFont::with_graphics_font(&graphics, points, &matrix, None) };
    // SAFETY: The new font is the retained construction result; its CG instance serves bounded source
    // verification only.
    let document_graphics = unsafe { document.graphics_font(std::ptr::null_mut()) };
    let name = CGFont::table_for_tag(Some(&graphics), u32::from_be_bytes(*b"name"))
        .ok_or(Error::MetadataUnavailable)?;
    let document_name =
        CGFont::table_for_tag(Some(&document_graphics), u32::from_be_bytes(*b"name"))
            .ok_or(Error::MetadataUnavailable)?;
    // CFEqual may differ between the UI wrapper and document CGFont; complete names, the actual glyph
    // domain,
    // units, original axes, and matrix certify the outline facts here without claiming identical
    // wrapper objects.
    if name_bytes(&name)? != name_bytes(&document_name)?
        || CGFont::number_of_glyphs(Some(&graphics))
            != CGFont::number_of_glyphs(Some(&document_graphics))
        || CGFont::units_per_em(Some(&graphics)) != CGFont::units_per_em(Some(&document_graphics))
    {
        return Err(Error::GraphicsSourceMismatch);
    }
    // This font-only construction has no Source/event ticket; reuse bounded complete-axis and file-URL
    // checks,
    // releasing temporary dictionaries/URLs in this call without synthetic requests or reads/freezes of
    // document files.
    if read_axes(font, &|| true)? != read_axes(&document, &|| true)? {
        return Err(Error::GraphicsSourceMismatch);
    }
    let original_path = file_path(font, &|| true)?;
    let document_path = file_path(&document, &|| true)?.ok_or(Error::FileSourceUnavailable)?;
    if original_path.is_some_and(|original| original != document_path) {
        return Err(Error::GraphicsSourceMismatch);
    }
    // SAFETY: This stack retains both immutable objects; read public scalars without layout or document
    // access.
    let (actual_points, actual_matrix, actual_traits, units, actual_units) = unsafe {
        (
            document.size(),
            document.matrix(),
            document.symbolic_traits(),
            font.units_per_em(),
            document.units_per_em(),
        )
    };
    let semantics = CTFontSymbolicTraits::TraitBold
        | CTFontSymbolicTraits::TraitItalic
        | CTFontSymbolicTraits::TraitMonoSpace;
    if actual_points != points
        || actual_matrix != matrix
        || units != actual_units
        || (traits & semantics) != (actual_traits & semantics)
    {
        return Err(Error::GraphicsSourceMismatch);
    }
    Ok(document) // Transfer the new font claim only; local graphics, name tables, axis dictionaries, and URLs are released on return.
}

/// Style derivation may select a real italic face in the same family while preserving weight,
/// monospace, and matrix.
/// Resizing allows the platform to recompute the optical axis; do not copy hidden old UI axes into the
/// new document font.
pub(super) fn require_derived_style(
    original: &CTFont,
    derived: &CTFont,
    points: f64,
    italic: bool,
) -> Result<()> {
    // SAFETY: The caller retains both immutable fonts on their original main thread; read bounded
    // public metadata only.
    let (
        family,
        derived_family,
        before_points,
        actual_points,
        matrix,
        actual_matrix,
        before,
        after,
    ) = unsafe {
        (
            original.family_name(),
            derived.family_name(),
            original.size(),
            derived.size(),
            original.matrix(),
            derived.matrix(),
            original.symbolic_traits(),
            derived.symbolic_traits(),
        )
    };
    let unchanged = CTFontSymbolicTraits::TraitBold
        | CTFontSymbolicTraits::TraitMonoSpace
        | CTFontSymbolicTraits::TraitExpanded
        | CTFontSymbolicTraits::TraitCondensed;
    let expected_italic = italic || before.contains(CTFontSymbolicTraits::TraitItalic);
    if *family != *derived_family
        || !points.is_finite()
        || points <= 0.0
        || actual_points != points
        || matrix != actual_matrix
        || (before & unchanged) != (after & unchanged)
        || after.contains(CTFontSymbolicTraits::TraitItalic) != expected_italic
    {
        return Err(Error::GraphicsSourceMismatch);
    }
    // A new italic face may add axes such as YAXS; preserve each original specified weight, width, and
    // other axis.
    // Only requested italic/slant axes and optical size during resizing may differ from their original
    // values.
    let original_axes = read_axes(original, &|| true)?;
    let actual_axes = read_axes(derived, &|| true)?;
    for (tag, value) in original_axes {
        if (italic && matches!(&tag, b"ital" | b"slnt"))
            || (points != before_points && tag == *b"opsz")
        {
            continue;
        }
        if !actual_axes
            .iter()
            .any(|(actual_tag, actual)| *actual_tag == tag && *actual == value)
        {
            return Err(Error::GraphicsSourceMismatch);
        }
    }
    Ok(()) // Family and axis copies remain local verification data, without palette, Source, or global cache ownership.
}

// Convert bounded metadata only; reject incomplete UTF-8 and control characters before returning owned
// text.
fn string(value: &CFString, current: &(impl Fn() -> bool + ?Sized)) -> Result<String> {
    let units = step(current, || value.length())?;
    if !(1..=1024).contains(&units) {
        return Err(Error::MetadataUnavailable);
    }
    let mut bytes = [0u8; 1025];
    // SAFETY: The fixed writable stack buffer includes the trailing NUL; a failed conversion cannot
    // supply a truncated string.
    let copied = step(current, || unsafe {
        value.c_string(
            bytes.as_mut_ptr().cast(),
            bytes.len() as _,
            CFStringBuiltInEncodings::EncodingUTF8.0,
        )
    })?;
    if !copied {
        return Err(Error::MetadataUnavailable);
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::MetadataUnavailable)?;
    let value = std::str::from_utf8(&bytes[..end]).map_err(|_| Error::MetadataUnavailable)?;
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Error::MetadataUnavailable);
    }
    Ok(value.to_owned())
}

// Accept only the actual absolute file URL; callers decide whether an absent UI proxy URL is
// admissible.
fn file_path(font: &CTFont, current: &(impl Fn() -> bool + ?Sized)) -> Result<Option<PathBuf>> {
    // SAFETY: The constant is the public CTFont file-URL key; the caller retains the original font
    // owner.
    let Some(raw_url) = step(current, || unsafe { font.attribute(kCTFontURLAttribute) })? else {
        return Ok(None);
    }; // A UI font proxy may lack a URL; only its actual CGFont round trip may supply the later source, without scans or guessed system paths.
    let url =
        step(current, || raw_url.downcast::<CFURL>())?.map_err(|_| Error::FileSourceUnavailable)?;
    let scheme = step(current, || url.scheme())?.ok_or(Error::FileSourceUnavailable)?;
    let file_scheme = string(&scheme, current)? == "file";
    step(current, || drop(scheme))?;
    if !file_scheme {
        return Err(Error::FileSourceUnavailable); // A nonfile URL path cannot certify a local font source.
    }
    let mut path = [0u8; 4096];
    // SAFETY: The output has 4096 writable stack bytes; the public function guarantees NUL termination
    // on success.
    if !step(current, || unsafe {
        url.file_system_representation(true, path.as_mut_ptr(), path.len() as _)
    })? {
        return Err(Error::FileSourceUnavailable);
    }
    let end = path
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::FileSourceUnavailable)?;
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&path[..end]));
    if !path.is_absolute() {
        return Err(Error::FileSourceUnavailable); // The working directory must not determine actual font-file identity.
    }
    step(current, || drop(url))?;
    Ok(Some(path))
}

/// Collect one guarded Native font source, releasing all round-trip objects before publishing retained
/// metadata.
pub(super) fn read(font: &CTFont, current: &(impl Fn() -> bool + ?Sized)) -> Result<Metadata> {
    // Claim both Copy results inside the same Native operation, including the out-parameter claim if
    // post-call authority is revoked.
    let (graphics, descriptor) = step(current, || {
        let mut raw_descriptor = std::ptr::null();
        // SAFETY: The out pointer addresses a valid stack slot; a nonnull descriptor carries its own
        // Copy-rule retained claim.
        let graphics = unsafe { font.graphics_font(&mut raw_descriptor) };
        let descriptor = NonNull::new(raw_descriptor.cast_mut()).map(|pointer| {
            // SAFETY: This nonnull CTFontDescriptor was just delivered by the public Copy out parameter
            // and has not been released.
            unsafe { CFRetained::<CTFontDescriptor>::from_raw(pointer) }
        });
        (graphics, descriptor)
    })?;
    let name_table = step(current, || {
        CGFont::table_for_tag(Some(&graphics), u32::from_be_bytes(*b"name"))
    })?
    .ok_or(Error::MetadataUnavailable)?;
    name_bytes(&name_table)?; // Bound the actual table length before matching, without inferring a face from a synthesized PostScript name.
    // SAFETY: Size is a public read-only scalar of this CTFont; it accesses neither layoutManager nor
    // document contents.
    let point_size = step(current, || unsafe { font.size() })?;
    if !point_size.is_finite() || point_size <= 0.0 {
        return Err(Error::MetadataUnavailable);
    }
    // SAFETY: Round-trip the original CGFont with its copied attributes; the null optional matrix
    // accesses no external pointer.
    let recreated = step(current, || unsafe {
        CTFont::with_graphics_font(
            &graphics,
            point_size,
            std::ptr::null(),
            descriptor.as_deref(),
        )
    })?;
    let original_path = file_path(font, current)?;
    let path = file_path(&recreated, current)?.ok_or(Error::FileSourceUnavailable)?;
    if original_path
        .as_ref()
        .is_some_and(|original| original != &path)
    {
        return Err(Error::GraphicsSourceMismatch);
    } // An existing original URL must match the actual CG round-trip source; an absent URL never licenses guessed filenames.
    // SAFETY: This stack retains the original font and actual CG round-trip result; the getter reads a
    // Copy matrix only.
    let original_matrix = step(current, || unsafe { font.matrix() })?;
    // SAFETY: Recreated comes from the original CGFont and its actual attributes, without external
    // pointers or substitute fonts.
    let recreated_matrix = step(current, || unsafe { recreated.matrix() })?;
    if original_matrix != recreated_matrix {
        return Err(Error::GraphicsSourceMismatch);
    }
    // SAFETY: The public API permits a null out parameter; this stack takes the returned retained
    // CGFont claim.
    let recreated_graphics = step(current, || unsafe {
        recreated.graphics_font(std::ptr::null_mut())
    })?;
    let recreated_name = step(current, || {
        CGFont::table_for_tag(Some(&recreated_graphics), u32::from_be_bytes(*b"name"))
    })?
    .ok_or(Error::MetadataUnavailable)?;
    if name_bytes(&name_table)? != name_bytes(&recreated_name)? {
        return Err(Error::GraphicsSourceMismatch);
    } // UI roles or synthesized descriptors may make CFEqual differ, but the complete actual name table must match.
    step(current, || drop(recreated_name))?;
    step(current, || drop(recreated_graphics))?;
    let design_coordinates = read_axes(font, current)?;
    let axes = design_coordinates
        .iter()
        .map(|(tag, value)| MarkdownLayoutFontAxis {
            tag: *tag,
            value: *value as f32,
        })
        .collect();
    // The round-trip object locates the original CGFont file only; CoreText may round its design
    // coordinates again.
    // Actual shaper axes come from the original retained font, never from this temporary object as
    // replacement authority.
    step(current, || drop(recreated))?;
    step(current, || drop(descriptor))?;
    // SAFETY: Both public scalars describe the original font; guarded calls create no character or
    // glyph arrays.
    let units_per_em = step(current, || unsafe { font.units_per_em() })?;
    let cg_units = step(current, || CGFont::units_per_em(Some(&graphics)))?;
    if u32::try_from(cg_units).ok() != Some(units_per_em) {
        return Err(Error::GraphicsSourceMismatch);
    }
    let glyph_count = step(current, || CGFont::number_of_glyphs(Some(&graphics)))?;
    step(current, || ())?;
    Ok(Metadata {
        graphics,
        path,
        name_table,
        axes,
        design_coordinates,
        point_size,
        units_per_em,
        glyph_count,
    })
}

/// Read original font axes only; sorting normalizes dictionary order without ignoring duplicates or
/// substituting round-trip values.
fn read_axes(font: &CTFont, current: &(impl Fn() -> bool + ?Sized)) -> Result<Vec<([u8; 4], f64)>> {
    // SAFETY: CopyVariation returns an immutable dictionary with numeric tags and values from the
    // actual original font.
    let variation = step(current, || unsafe { font.variation() })?;
    let mut axes = Vec::new();
    if let Some(variation) = variation {
        let count = step(current, || variation.len())?;
        if count > 32 {
            return Err(Error::MetadataUnavailable);
        }
        let mut keys = [std::ptr::null::<c_void>(); 32];
        let mut values = [std::ptr::null::<c_void>(); 32];
        // SAFETY: The immutable Copy dictionary has at most 32 entries; both arrays provide 32 slots
        // and the dictionary spans all borrows.
        step(current, || unsafe {
            variation.keys_and_values(keys.as_mut_ptr(), values.as_mut_ptr())
        })?;
        for (&key, &value) in keys[..count].iter().zip(&values[..count]) {
            let (Some(key), Some(value)) =
                (NonNull::new(key.cast_mut()), NonNull::new(value.cast_mut()))
            else {
                return Err(Error::MetadataUnavailable);
            };
            // SAFETY: CTFontCopyVariation specifies CFType number keys/values; the retained dictionary
            // spans these stack borrows.
            let (key, value) = unsafe {
                (
                    &*key.as_ptr().cast::<CFType>(),
                    &*value.as_ptr().cast::<CFType>(),
                )
            };
            let key = step(current, || key.downcast_ref::<CFNumber>())?
                .ok_or(Error::MetadataUnavailable)?;
            let value = step(current, || value.downcast_ref::<CFNumber>())?
                .ok_or(Error::MetadataUnavailable)?;
            let tag = step(current, || key.as_i64())?.ok_or(Error::MetadataUnavailable)?;
            let native_value =
                step(current, || value.as_f64())?.ok_or(Error::MetadataUnavailable)?;
            // CFNumber Float32 conversion may fail on ordinary rounding; retain the original
            // double-precision design coordinate first.
            // Core must verify exact final F2Dot14 coordinates after parsing real fvar/avar tables,
            // without design-value epsilon guesses.
            if !native_value.is_finite() || !(native_value as f32).is_finite() {
                return Err(Error::MetadataUnavailable);
            }
            let tag = u32::try_from(tag)
                .map_err(|_| Error::MetadataUnavailable)?
                .to_be_bytes();
            if axes.iter().any(|(prior, _): &([u8; 4], f64)| *prior == tag) {
                return Err(Error::MetadataUnavailable); // Reject duplicate/nonfinite axes and never truncate a negative tag.
            }
            axes.push((tag, native_value));
        }
        step(current, || drop(variation))?;
    }
    axes.sort_unstable_by_key(|(tag, _)| *tag);
    Ok(axes)
}
