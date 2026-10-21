//! Resolve through bounded table directories, names, and fvar metadata without copying font bytes or interpreting glyph outlines.
use std::sync::Arc;

use harfrust::Font;
use read_fonts::{
    TableProvider,
    types::{NameId, Tag},
};

use super::{
    MarkdownLayoutFontAxis as Axis, MarkdownLayoutFontError as Error,
    MarkdownLayoutFontMetadata as Metadata, Result,
};

const MAX_FACES: usize = 256;
const MAX_TABLES: usize = 256;
const MAX_AXES: usize = 32;
const MAX_INSTANCES: usize = 128;
const MAX_NAME_RECORDS: usize = 4096;
const MAX_POSTSCRIPT_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 1024 * 1024;
const MAX_NAME_RECORD_VISITS: usize = 1_048_576;
const MAX_DECODED_NAME_SCALARS: usize = 1_048_576;

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    let end = offset.checked_add(2).ok_or(Error::MetadataOverflow)?;
    let raw = bytes.get(offset..end).ok_or(Error::InvalidContainer)?;
    Ok(u16::from_be_bytes(raw.try_into().expect("已核固定2B")))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset.checked_add(4).ok_or(Error::MetadataOverflow)?;
    let raw = bytes.get(offset..end).ok_or(Error::InvalidContainer)?;
    Ok(u32::from_be_bytes(raw.try_into().expect("已核固定4B")))
}

/// Retain a few directory facts per face; bound actual record counts before iteration rather than hiding unknown tails with take.
struct Directory {
    name_records: usize,
    has_fvar: bool,
}
fn directory(bytes: &[u8], base: usize) -> Result<Directory> {
    let version_end = base.checked_add(4).ok_or(Error::MetadataOverflow)?;
    if !matches!(
        bytes.get(base..version_end),
        Some(b"\0\x01\0\0" | b"OTTO" | b"true")
    ) {
        return Err(Error::InvalidContainer); // A non-SFNT input cannot prove the same collection face and must not enter a permissive fallback.
    }
    let count = usize::from(u16_at(bytes, base + 4)?);
    if count == 0 || count > MAX_TABLES {
        return Err(Error::InvalidDirectory);
    }
    let start = base.checked_add(12).ok_or(Error::MetadataOverflow)?;
    let end = start
        .checked_add(count.checked_mul(16).ok_or(Error::MetadataOverflow)?)
        .ok_or(Error::MetadataOverflow)?;
    let records = bytes.get(start..end).ok_or(Error::InvalidDirectory)?;
    let mut previous = None;
    let mut name_records = None;
    let mut has_fvar = false;
    for record in records.as_chunks::<16>().0 {
        let tag: [u8; 4] = record[..4].try_into().expect("固定tag");
        if previous.is_some_and(|old| old >= tag) {
            return Err(Error::InvalidDirectory); // Duplicate/unordered tags must not let different parsers select different tables for the same name.
        }
        previous = Some(tag);
        let offset = usize::try_from(u32_at(record, 8)?).map_err(|_| Error::MetadataOverflow)?;
        let size = usize::try_from(u32_at(record, 12)?).map_err(|_| Error::MetadataOverflow)?;
        let end = offset.checked_add(size).ok_or(Error::MetadataOverflow)?;
        let table = bytes.get(offset..end).ok_or(Error::InvalidDirectory)?;
        if tag == *b"name" {
            // Validate the complete directory and string-region boundary before any decoded-name iteration.
            if size > MAX_NAME_BYTES || table.len() < 6 {
                return Err(Error::NameBudgetExceeded);
            }
            let version = u16_at(table, 0)?;
            let count = usize::from(u16_at(table, 2)?);
            let record_end = 6_usize
                .checked_add(count.checked_mul(12).ok_or(Error::MetadataOverflow)?)
                .ok_or(Error::MetadataOverflow)?;
            let string_start = usize::from(u16_at(table, 4)?);
            if count > MAX_NAME_RECORDS {
                return Err(Error::NameBudgetExceeded);
            }
            if version > 1 || record_end > table.len() || string_start < record_end {
                return Err(Error::InvalidNameTable);
            }
            if version == 1 {
                // Format-one language-tag records belong before the string region too; bound their actual count and extent.
                let languages = usize::from(u16_at(table, record_end)?);
                let language_end = record_end
                    .checked_add(2)
                    .and_then(|start| start.checked_add(languages.checked_mul(4)?))
                    .ok_or(Error::MetadataOverflow)?;
                if languages > MAX_NAME_RECORDS {
                    return Err(Error::NameBudgetExceeded);
                }
                if language_end > table.len() || string_start < language_end {
                    return Err(Error::InvalidNameTable);
                }
            }
            if string_start > table.len() {
                return Err(Error::InvalidNameTable);
            }
            name_records = Some(count);
        }
        has_fvar |= tag == *b"fvar";
    }
    Ok(Directory {
        name_records: name_records.ok_or(Error::InvalidNameTable)?,
        has_fvar,
    })
}

struct NameBudget {
    records: usize,
    scalars: usize,
}
impl NameBudget {
    fn matches(
        &mut self,
        font: &Font,
        directory: &Directory,
        id: NameId,
        target: &str,
    ) -> Result<bool> {
        // Repeated instance-name lookups share one finite record/scalar budget across every collection face.
        self.records = self
            .records
            .checked_sub(directory.name_records)
            .ok_or(Error::NameBudgetExceeded)?;
        for name in font.names().get(id).iter() {
            let mut wanted = target.chars();
            let mut matches = true;
            for (index, scalar) in name.name().chars().enumerate() {
                self.scalars = self
                    .scalars
                    .checked_sub(1)
                    .ok_or(Error::NameBudgetExceeded)?;
                if index >= MAX_POSTSCRIPT_BYTES {
                    return Err(Error::NameBudgetExceeded); // Truncating an overlong equal prefix could manufacture an apparent exact match.
                }
                if wanted.next() != Some(scalar) {
                    matches = false;
                    break; // The name already differs; avoid decoding an unrelated long suffix or retaining its String.
                }
            }
            if matches && wanted.next().is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

struct Candidate {
    face: u32,
    font: Font,
    named: Option<(u16, Vec<Axis>)>,
    has_fvar: bool,
}

/// Decode one bounded base PSName from the selected face's original names, not a synthesized Native name or named-instance label.
fn face_postscript(font: &Font) -> Result<String> {
    for record in font.names().get(NameId::POSTSCRIPT_NAME).iter() {
        let mut candidate = String::new();
        for scalar in record.name().chars() {
            if !scalar.is_ascii()
                || scalar.is_ascii_control()
                || candidate.len() >= MAX_POSTSCRIPT_BYTES
            {
                return Err(Error::InvalidPostScriptName);
            }
            candidate.push(scalar);
        } // The complete table passed directory limits; decode only this bounded name, without glyph outlines or document text.
        if !candidate.is_empty() {
            return Ok(candidate);
        }
    }
    Err(Error::PostScriptNotFound)
}

/// Derive from the already authenticated immutable face, retaining its unique identity only when the complete original name table matches.
/// No replacement Blob is accepted, so equal paths/names or another TTC face cannot substitute for the original immutable source.
pub(super) fn derive_name_table(
    original: &Font,
    metadata: &Metadata,
    native_name_table: &[u8],
    axes: &[Axis],
) -> Result<(Font, Metadata)> {
    if native_name_table.is_empty() || native_name_table.len() > MAX_NAME_BYTES {
        return Err(Error::InvalidNameTable);
    }
    if axes.len() > MAX_AXES {
        return Err(Error::VariationBudgetExceeded);
    }
    // Original resolution already excluded ambiguity across the complete container. Check this same face's full table,
    // without creating another Font/cache or inferring the face index from a table prefix or PSName.
    if !original
        .tables()
        .name_data()
        .is_some_and(|table| table == native_name_table)
    {
        return Err(Error::PostScriptNotFound);
    }
    // General resolution may start from a named-instance PSName. Derivation returns to base face plus explicit axes,
    // so an old instance label must not follow new coordinates; name-table resolution already holds the actual base name.
    let base_postscript = metadata
        .named_instance
        .map(|_| face_postscript(original))
        .transpose()?;
    let postscript = base_postscript
        .as_deref()
        .unwrap_or(&metadata.postscript_name);
    finish(
        Candidate {
            face: metadata.face_index,
            font: original.clone(),
            named: None,
            has_fvar: original.tables().fvar_data().is_some(),
        },
        metadata.face_count as usize,
        metadata.source_bytes,
        postscript,
        axes,
    ) // Original finish validates actual fvar and unknown/duplicate/out-of-range axes before deriving from the same SharedFont.
}

/// Match the complete Native name table byte-for-byte to one real face; equal names or glyph counts cannot replace source verification.
pub(super) fn resolve_name_table(
    bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
    native_name_table: &[u8],
    axes: &[Axis],
) -> Result<(Font, Metadata)> {
    if native_name_table.is_empty() || native_name_table.len() > MAX_NAME_BYTES {
        return Err(Error::InvalidNameTable);
    }
    if axes.len() > MAX_AXES {
        return Err(Error::VariationBudgetExceeded);
    }
    let raw: &[u8] = bytes.as_ref().as_ref();
    let collection = raw.get(..4) == Some(b"ttcf");
    let count = if collection {
        if !matches!(u32_at(raw, 4)?, 0x0001_0000 | 0x0002_0000) {
            return Err(Error::InvalidContainer);
        }
        usize::try_from(u32_at(raw, 8)?).map_err(|_| Error::MetadataOverflow)?
    } else {
        1
    }; // A standalone candidate still requires strict SFNT directory validation, rather than assuming face zero matches.
    if count == 0 || count > MAX_FACES {
        return Err(Error::FaceBudgetExceeded);
    }
    let mut matched = None;
    for face in 0..count {
        let base = if collection {
            usize::try_from(u32_at(raw, 12 + face * 4)?).map_err(|_| Error::MetadataOverflow)?
        } else {
            0
        };
        let directory = directory(raw, base)?;
        let font = Font::new(Arc::clone(&bytes), face as u32).ok_or(Error::InvalidFace)?;
        if !read_fonts::FontRef::from_index(raw, face as u32)
            .map_err(|_| Error::InvalidFace)?
            .table_data(Tag::new(b"name"))
            .is_some_and(|table| table.as_bytes() == native_name_table)
        {
            continue;
        } // Only complete original-table equality matches, not a prefix, coincident hash, or Native display name.
        if matched.is_some() {
            return Err(Error::AmbiguousFace);
        } // Multiple faces with identical name tables lack unique identity; iteration order cannot select one.
        let postscript = face_postscript(&font)?;
        matched = Some((
            Candidate {
                face: face as u32,
                font,
                named: None,
                has_fvar: directory.has_fvar,
            },
            postscript,
        ));
    }
    let (matched, postscript) = matched.ok_or(Error::PostScriptNotFound)?;
    // Full-table equality already uniquely identified the face, with axes supplied by the actual Native instance.
    // Do not enumerate unrelated named instances or raise their budget merely to bypass this original-source proof.
    finish(matched, count, raw.len(), &postscript, axes)
}

pub(super) fn resolve(
    bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
    postscript: &str,
    axes: &[Axis],
) -> Result<(Font, Metadata)> {
    // Bound public identity/axis input before inspecting the immutable blob; no invalid request falls back to a generic face.
    if postscript.is_empty()
        || postscript.len() > MAX_POSTSCRIPT_BYTES
        || postscript.chars().any(char::is_control)
    {
        return Err(Error::InvalidPostScriptName);
    }
    if axes.len() > MAX_AXES {
        return Err(Error::VariationBudgetExceeded);
    }
    let raw: &[u8] = bytes.as_ref().as_ref();
    let count = if raw.get(..4) == Some(b"ttcf") {
        if !matches!(u32_at(raw, 4)?, 0x0001_0000 | 0x0002_0000) {
            return Err(Error::InvalidContainer);
        }
        usize::try_from(u32_at(raw, 8)?).map_err(|_| Error::MetadataOverflow)?
    } else {
        1 // One standalone SFNT candidate only; validate its actual signature and directory below.
    };
    if count == 0 || count > MAX_FACES {
        return Err(Error::FaceBudgetExceeded);
    }
    let mut budget = NameBudget {
        records: MAX_NAME_RECORD_VISITS,
        scalars: MAX_DECODED_NAME_SCALARS,
    };
    let mut matched = None;
    for face in 0..count {
        let base = if count == 1 && raw.get(..4) != Some(b"ttcf") {
            0
        } else {
            usize::try_from(u32_at(raw, 12 + face * 4)?).map_err(|_| Error::MetadataOverflow)?
        };
        let directory = directory(raw, base)?;
        let font = Font::new(Arc::clone(&bytes), face as u32).ok_or(Error::InvalidFace)?;
        let direct = budget.matches(&font, &directory, NameId::POSTSCRIPT_NAME, postscript)?;
        let mut named: Option<(u16, Vec<Axis>)> = None;
        if directory.has_fvar {
            // PSName resolution must also inspect actual named instances; share original name work limits and reject an oversized instance table.
            let fvar = font
                .tables()
                .fvar()
                .map_err(|_| Error::InvalidVariationTable)?;
            let records = fvar.axes().map_err(|_| Error::InvalidVariationTable)?;
            if records.len() > MAX_AXES || usize::from(fvar.instance_count()) > MAX_INSTANCES {
                return Err(Error::VariationBudgetExceeded);
            }
            for (index, instance) in fvar
                .instances()
                .map_err(|_| Error::InvalidVariationTable)?
                .iter()
                .enumerate()
            {
                let instance = instance.map_err(|_| Error::InvalidVariationTable)?;
                let Some(id) = instance.post_script_name_id else {
                    continue; // An instance lacking PSNameID cannot substitute a subfamily display name for an exact PSName.
                };
                if !budget.matches(&font, &directory, id, postscript)? {
                    continue;
                }
                if instance.coordinates.len() != records.len() {
                    return Err(Error::InvalidVariationTable);
                }
                if named.is_some() {
                    return Err(Error::AmbiguousNamedInstance);
                }
                let coordinates = records
                    .iter()
                    .zip(instance.coordinates.iter())
                    .map(|(record, value)| Axis {
                        tag: record.axis_tag().to_be_bytes(),
                        value: value.get().to_f32(),
                    })
                    .collect();
                named = Some((index as u16, coordinates));
            }
        }
        if direct || named.is_some() {
            if direct && let Some((_, coordinates)) = &named {
                let fvar = font
                    .tables()
                    .fvar()
                    .map_err(|_| Error::InvalidVariationTable)?;
                let records = fvar.axes().map_err(|_| Error::InvalidVariationTable)?;
                if records.iter().zip(coordinates).any(|(record, axis)| {
                    record.axis_tag().to_be_bytes() != axis.tag
                        || record.default_value().to_f32() != axis.value
                }) {
                    // nameID6 also identifies this face's default position. A same-name instance agrees only when every axis equals default;
                    // equal PSNames at different positions cannot be resolved by iteration order.
                    return Err(Error::AmbiguousNamedInstance);
                }
            }
            if matched.is_some() {
                return Err(Error::AmbiguousFace); // Never choose the first face by directory order or a previous-match cache.
            }
            matched = Some(Candidate {
                face: face as u32,
                font,
                named,
                has_fvar: directory.has_fvar,
            });
        }
    }
    finish(
        matched.ok_or(Error::PostScriptNotFound)?,
        count,
        raw.len(),
        postscript,
        axes,
    )
}

/// Share axis validation and actual instance construction only after face selection, preserving each entry's distinct matching authority.
fn finish(
    matched: Candidate,
    count: usize,
    source_bytes: usize,
    postscript: &str,
    axes: &[Axis],
) -> Result<(Font, Metadata)> {
    let named_instance = matched.named.as_ref().map(|(index, _)| *index);
    let mut actual = matched.named.map_or_else(Vec::new, |(_, axes)| axes);
    let mut explicit = axes.to_vec();
    // Sort finite explicit tags once so duplicates fail before a builder could silently replace one value with another.
    explicit.sort_unstable_by_key(|axis| axis.tag);
    if explicit.windows(2).any(|pair| pair[0].tag == pair[1].tag) {
        return Err(Error::DuplicateAxis);
    }
    if matched.has_fvar {
        let fvar = matched
            .font
            .tables()
            .fvar()
            .map_err(|_| Error::InvalidVariationTable)?;
        let records = fvar.axes().map_err(|_| Error::InvalidVariationTable)?;
        if records.len() > MAX_AXES {
            return Err(Error::VariationBudgetExceeded);
        } // Native table matching retains the actual axis-count limit without reading unrelated named instances.
        for (index, record) in records.iter().enumerate() {
            let tag = record.axis_tag().to_be_bytes();
            let min = record.min_value().to_f32();
            let default = record.default_value().to_f32();
            let max = record.max_value().to_f32();
            if !min.is_finite()
                || !default.is_finite()
                || !max.is_finite()
                || min > default
                || default > max
                || records[..index]
                    .iter()
                    .any(|earlier| earlier.axis_tag() == record.axis_tag())
            {
                return Err(Error::InvalidAxisValue);
            }
            if actual.is_empty() && index == 0 {
                actual = records
                    .iter()
                    .map(|axis| Axis {
                        tag: axis.axis_tag().to_be_bytes(),
                        value: axis.default_value().to_f32(),
                    })
                    .collect(); // A non-named instance starts at this face's actual defaults, not zero-filled requested axes.
            }
            let value = actual
                .iter_mut()
                .find(|axis| axis.tag == tag)
                .ok_or(Error::InvalidVariationTable)?;
            if let Some(requested) = explicit.iter().find(|axis| axis.tag == tag) {
                value.value = requested.value;
            }
            if !value.value.is_finite() || value.value < min || value.value > max {
                return Err(Error::InvalidAxisValue); // Builder clamping must not hide invalid coordinates from the instance table itself.
            }
        }
        if explicit
            .iter()
            .any(|axis| !actual.iter().any(|known| known.tag == axis.tag))
        {
            return Err(Error::UnknownAxis);
        }
    } else if !explicit.is_empty() {
        return Err(Error::UnknownAxis); // Static faces have no valid explicit variation axes; do not ignore the request.
    }
    actual.sort_unstable_by_key(|axis| axis.tag);
    // Build a new instance only from the complete validated axis set; retain actual face metrics in metadata rather than caller estimates.
    let font = matched
        .font
        .instance_builder()
        .variations(actual.iter().map(|axis| (Tag::new(&axis.tag), axis.value)))
        .build();
    let units_per_em = font.units_per_em();
    if units_per_em == 0 {
        return Err(Error::InvalidUnitsPerEm);
    }
    let glyph_count = font.num_glyphs();
    Ok((
        font,
        Metadata {
            face_index: matched.face,
            face_count: count as u32,
            named_instance,
            units_per_em,
            glyph_count,
            axes: actual,
            postscript_name: postscript.to_owned(),
            source_bytes,
        },
    ))
}
