//! Verify exact faces, named instances, and ownership with the repository's real font and finite synthetic SFNT metadata.
use super::*;
use std::collections::BTreeMap;

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));
const NAME: &str = "Roboto-Regular";

fn bytes(value: &[u8]) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
    Arc::new(value.to_vec()) // Only finite test fixtures copy font bytes; the product resolver shares the caller's original Arc.
}
fn read_u16(value: &[u8], offset: usize) -> usize {
    usize::from(u16::from_be_bytes(
        value[offset..offset + 2].try_into().unwrap(),
    ))
}
fn read_u32(value: &[u8], offset: usize) -> usize {
    u32::from_be_bytes(value[offset..offset + 4].try_into().unwrap()) as usize
}
fn tables(value: &[u8]) -> BTreeMap<[u8; 4], Vec<u8>> {
    // Decode this known finite fixture's actual directory so modified inputs retain real table payloads rather than a replacement Font implementation.
    (0..read_u16(value, 4))
        .map(|index| {
            let record = 12 + index * 16;
            let tag = value[record..record + 4].try_into().unwrap();
            let offset = read_u32(value, record + 8);
            let size = read_u32(value, record + 12);
            (tag, value[offset..offset + size].to_vec())
        })
        .collect()
}
fn rebuild(tables: BTreeMap<[u8; 4], Vec<u8>>) -> Vec<u8> {
    // Rebuild only finite fixture metadata, preserving sorted tags and real absolute SFNT offsets for the production resolver.
    let count = tables.len();
    let mut result = vec![0; 12 + count * 16];
    result[..4].copy_from_slice(b"\0\x01\0\0");
    result[4..6].copy_from_slice(&(count as u16).to_be_bytes());
    let exponent = count.ilog2();
    let search = 16 * 2_usize.pow(exponent);
    result[6..8].copy_from_slice(&(search as u16).to_be_bytes());
    result[8..10].copy_from_slice(&(exponent as u16).to_be_bytes());
    result[10..12].copy_from_slice(&((count * 16 - search) as u16).to_be_bytes());
    for (index, (tag, data)) in tables.into_iter().enumerate() {
        let record = 12 + index * 16;
        let offset = result.len();
        result[record..record + 4].copy_from_slice(&tag);
        result[record + 8..record + 12].copy_from_slice(&(offset as u32).to_be_bytes());
        result[record + 12..record + 16].copy_from_slice(&(data.len() as u32).to_be_bytes());
        result.extend(data);
        while !result.len().is_multiple_of(4) {
            result.push(0);
        }
    }
    result
}
fn collection(fonts: &[Vec<u8>]) -> Vec<u8> {
    // Assemble a real finite TTC container, moving each embedded face's table offsets into the complete container coordinate space.
    let mut result = vec![0; 12 + fonts.len() * 4];
    result[..4].copy_from_slice(b"ttcf");
    result[4..8].copy_from_slice(&0x0001_0000_u32.to_be_bytes());
    result[8..12].copy_from_slice(&(fonts.len() as u32).to_be_bytes());
    for (index, font) in fonts.iter().enumerate() {
        let base = result.len();
        result[12 + index * 4..16 + index * 4].copy_from_slice(&(base as u32).to_be_bytes());
        let mut font = font.clone();
        for index in 0..read_u16(&font, 4) {
            let offset = 12 + index * 16 + 8;
            let shifted = read_u32(&font, offset) + base;
            font[offset..offset + 4].copy_from_slice(&(shifted as u32).to_be_bytes());
        }
        result.extend(font); // TTC table offsets refer to actual full-container bytes, not local offsets interpreted as face zero.
    }
    result
}
fn renamed() -> Vec<u8> {
    // Change existing PSName records in the real font, producing a genuinely distinct second face for identity/ambiguity cases.
    let mut all = tables(ROBOTO);
    let name = all.get_mut(b"name").unwrap();
    let start = read_u16(name, 4);
    for index in 0..read_u16(name, 2) {
        let record = 6 + index * 12;
        if read_u16(name, record + 6) != 6 {
            continue;
        }
        let offset = start + read_u16(name, record + 10);
        match read_u16(name, record) {
            0 | 3 => name[offset..offset + 2].copy_from_slice(&[0, b'X']),
            1 => name[offset] = b'X',
            _ => panic!("固定上游fixture的PS编码改变"),
        }
    }
    rebuild(all)
}
fn named_variable() -> Vec<u8> {
    named_variable_with("Roboto-Medium", 500)
}
fn named_variable_with(postscript: &str, coordinate: i32) -> Vec<u8> {
    // Add one finite real name/fvar instance record to the original face, so public resolution must read its actual encoded coordinates.
    let mut all = tables(ROBOTO);
    let name = all.get_mut(b"name").unwrap();
    assert_eq!(read_u16(name, 0), 0);
    let count = read_u16(name, 2);
    let old_start = read_u16(name, 4);
    let strings = name[old_start..].to_vec();
    let mut rebuilt = name[..6 + count * 12].to_vec();
    rebuilt[2..4].copy_from_slice(&((count + 1) as u16).to_be_bytes());
    rebuilt[4..6].copy_from_slice(&((6 + (count + 1) * 12) as u16).to_be_bytes());
    let encoded: Vec<u8> = postscript
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    for field in [
        3_u16,
        1,
        0x0409,
        256,
        encoded.len() as u16,
        strings.len() as u16,
    ] {
        rebuilt.extend(field.to_be_bytes());
    }
    rebuilt.extend(strings);
    rebuilt.extend(encoded);
    *name = rebuilt;
    let mut fvar = Vec::new();
    for field in [1_u16, 0, 16, 2, 1, 20, 1, 10] {
        fvar.extend(field.to_be_bytes());
    }
    fvar.extend(*b"wght");
    for value in [100_i32, 400, 900] {
        fvar.extend((value << 16).to_be_bytes());
    }
    for field in [0_u16, 257, 258, 0] {
        fvar.extend(field.to_be_bytes());
    }
    fvar.extend((coordinate << 16).to_be_bytes());
    fvar.extend(256_u16.to_be_bytes());
    all.insert(*b"fvar", fvar);
    rebuild(all)
}

#[test]
// A clone retains original authority until its last owner drops; resolving equal bytes again must create independent authority.
fn real_face_owns_exact_font_and_only_its_clones_share_authority() {
    let original = bytes(ROBOTO);
    let font = MarkdownLayoutFont::resolve(Arc::clone(&original), NAME, &[]).unwrap();
    let metadata = font.metadata();
    assert_eq!(metadata.face_index, 0);
    assert_eq!(metadata.face_count, 1);
    assert!(metadata.named_instance.is_none());
    assert!(metadata.axes.is_empty());
    assert_eq!(metadata.source_bytes, ROBOTO.len());
    assert_eq!(metadata.units_per_em, font.font().units_per_em());
    let observer = font.resource_observer();
    let authority = font.authority();
    let shared = font.clone();
    assert!(shared.permits(&authority));
    let other = MarkdownLayoutFont::resolve(original, NAME, &[]).unwrap();
    assert!(!other.permits(&authority));
    assert!(!font.permits(&other.authority()));
    drop(font);
    assert!(!observer.payload_released());
    drop(shared);
    assert!(observer.payload_released()); // Other font-byte owners still live; this receipt proves only the original payload/Font field dropped.
    assert_eq!(other.metadata().postscript_name, NAME);
}

#[test]
// Explicit point-size precision cannot alter the general font contract or allow another size to borrow this bound instance's authority.
fn bound_point_size_keeps_old_scale_contract_and_rejects_a_different_size() {
    let name = tables(ROBOTO).remove(b"name").unwrap();
    let generic = MarkdownLayoutFont::resolve_name_table(bytes(ROBOTO), &name, &[]).unwrap();
    assert_eq!(positioning::scale(&generic, 15.0, 960).unwrap(), (960, 1));
    let bound =
        MarkdownLayoutFont::resolve_name_table_at_size(bytes(ROBOTO), &name, &[], 15.0).unwrap();
    assert_eq!(bound.shaping_point_size(), Some(15.0));
    assert_eq!(
        positioning::scale(&bound, 15.0, 960).unwrap(),
        (15 * 64 * 16384, 16384)
    );
    assert!(matches!(
        positioning::scale(&bound, 16.0, 1024),
        Err(MarkdownLayoutFontError::InvalidPointSize)
    ));
    assert!(!bound.permits(&generic.authority())); // New construction creates new authority rather than reauthorizing an old Font job.

    // A valid 0.1-point Native size retains its original double precision instead of rounding to 6/64 point first; large sizes keep the valid range.
    for points in [0.1, 32.0, 1024.0, 1_000_000.0, 33_554_430.0] {
        let font =
            MarkdownLayoutFont::resolve_name_table_at_size(bytes(ROBOTO), &name, &[], points)
                .unwrap();
        let (scale, divisor) =
            positioning::scale(&font, points as f32, (points * 64.0).round() as i32).unwrap();
        assert!(scale > 0 && divisor > 0 && (divisor as u32).is_power_of_two());
        assert_eq!(
            f64::from(scale),
            (points * 64.0 * f64::from(divisor)).round()
        );
    }
    for points in [0.0, -1.0, f64::NAN, f64::INFINITY, 0.001, 33_554_432.0] {
        assert!(matches!(
            MarkdownLayoutFont::resolve_name_table_at_size(bytes(ROBOTO), &name, &[], points),
            Err(MarkdownLayoutFontError::InvalidPointSize)
        ));
    }
}

#[test]
// Derived faces must shape identically to fresh same-source instances and release their independent payloads before the shared bytes' final munmap.
fn derived_face_keeps_real_shaping_and_independent_payload_release() {
    use crate::{
        MarkdownDocumentBuffer, MarkdownLayoutTextSource, MarkdownShapingBudget,
        MarkdownShapingDirection, MarkdownShapingJob, MarkdownShapingOptions,
        MarkdownShapingStatus,
    };

    // Freeze the real upstream font and open independent text, writing only an isolated fixture directory without platform fonts or user notes.
    let directory = tempfile::tempdir().unwrap();
    let font_path = directory.path().join("font.ttf");
    let document_path = directory.path().join("source.md");
    std::fs::write(&font_path, ROBOTO).unwrap();
    std::fs::write(&document_path, "office AVA a\u{301} fi").unwrap();
    let frozen = MarkdownLayoutFontBytes::capture_file(&font_path, ROBOTO.len() as u64).unwrap();
    let mapping = frozen.resource_observer();
    let name = tables(ROBOTO).remove(b"name").unwrap();
    let original =
        MarkdownLayoutFont::resolve_name_table_at_size(frozen.blob(), &name, &[], 14.0).unwrap();
    let original_drop = original.resource_observer();
    let document = MarkdownDocumentBuffer::open(&document_path, 65_536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_authority = source.authority();

    // Drive the public Job through complete input and shaping, comparing actual size-bound glyphs/clusters/advances.
    // Do not inspect cache addresses, manufacture a platform advance oracle, or treat derivation counts as performance evidence.
    let shape = |font: &MarkdownLayoutFont, points: f32| {
        let authority = font.authority();
        let mut job = MarkdownShapingJob::new(
            source.clone(),
            &source_authority,
            font.clone(),
            &authority,
            0..source.utf16_len(),
            MarkdownShapingOptions {
                direction: MarkdownShapingDirection::LeftToRight,
                script: *b"Latn",
                language: Some("en".into()),
                points,
                beginning_of_text: true,
                end_of_text: true,
                features: Vec::new(),
            },
        )
        .unwrap();
        let resources = job.resource_observer();
        while job.status() == MarkdownShapingStatus::Ingesting {
            job.step(
                &source_authority,
                &authority,
                MarkdownShapingBudget::default(),
            )
            .unwrap();
        }
        job.shape(&source_authority, &authority).unwrap();
        let count = job.profile().unwrap().glyph_count;
        let result: Vec<_> = (0..count)
            .map(|index| {
                job.glyph(&source_authority, &authority, index)
                    .unwrap()
                    .unwrap()
            })
            .collect();
        assert!(!result.is_empty());
        drop(job);
        // The case still legitimately owns Source and Font; these facts settle only the completed Job's actual storage resources.
        let closed = resources.snapshot();
        assert_eq!(
            (
                closed.work_bytes,
                closed.files_open,
                closed.mappings_open,
                closed.mapped_bytes
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(closed.files_created, closed.files_closed);
        assert_eq!(closed.mappings_created, closed.mappings_released);
        assert!(!closed.cleanup_failed && closed.first_error.is_none());
        result
    };
    let _ = shape(&original, 14.0); // Shape with the original face before deriving, rather than testing only an unused Font.
    for points in [0.1, 14.0, 32.0] {
        let derived = original
            .derive_name_table_at_size(&name, &[], points)
            .unwrap();
        let full =
            MarkdownLayoutFont::resolve_name_table_at_size(frozen.blob(), &name, &[], points)
                .unwrap();
        assert!(!derived.permits(&original.authority()));
        assert_eq!(shape(&derived, points as f32), shape(&full, points as f32));
        let released = derived.resource_observer();
        drop(derived);
        assert!(released.payload_released()); // Shared derived caches must not keep the independent authority payload alive.
    }

    // Drop the original payload first; the surviving derived face still shapes, and only the last strong byte owner triggers actual munmap.
    let surviving = original
        .derive_name_table_at_size(&name, &[], 32.0)
        .unwrap();
    let expected = shape(&surviving, 32.0);
    drop(original);
    assert!(original_drop.payload_released());
    drop(frozen);
    assert!(!mapping.mapping_drop_completed());
    assert_eq!(shape(&surviving, 32.0), expected);
    drop(surviving);
    assert!(mapping.mapping_drop_completed());
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(!mapping.failed());
}

#[test]
// Public capture rejects invalid complete-domain metric inputs as a whole, without claiming that synthetic values are successful Native measurements.
fn incomplete_or_unrepresentable_horizontal_metrics_reject_the_entire_font_capture() {
    let name = tables(ROBOTO).remove(b"name").unwrap();
    let original = MarkdownLayoutFont::resolve(bytes(ROBOTO), NAME, &[]).unwrap();
    let count = original.metadata().glyph_count as usize;
    // These are actual invalid inputs at the public capture boundary, not a fabricated successful platform-advance oracle.
    // Missing glyphs cannot fall back to hmtx; nonfinite/overflowing advances cannot saturate into another valid width.
    for (case, advances) in [
        ("missing_glyph_domain", Box::<[f64]>::default()),
        ("not_finite", vec![f64::NAN; count].into_boxed_slice()),
        ("infinite", vec![f64::INFINITY; count].into_boxed_slice()),
        (
            "coordinate_overflow",
            vec![f64::MAX; count].into_boxed_slice(),
        ),
    ] {
        assert!(
            matches!(
                MarkdownLayoutFont::resolve_name_table_at_size_with_horizontal_advances(
                    bytes(ROBOTO),
                    &name,
                    &[],
                    14.0,
                    advances,
                ),
                Err(MarkdownLayoutFontError::InvalidHorizontalAdvances)
            ),
            "非法水平推进不得发布部分字体: {case}"
        );
    }
}

#[test]
// Original f64 design coordinates must normalize to the actual Font position; nearby unequal positions and invalid tags cannot pass approximately.
fn original_double_design_axes_require_exact_normalized_equivalence() {
    let variable = named_variable();
    let name = tables(&variable).remove(b"name").unwrap();
    let native = 500.123456_f64;
    let font = MarkdownLayoutFont::resolve_name_table_at_size(
        bytes(&variable),
        &name,
        &[MarkdownLayoutFontAxis {
            tag: *b"wght",
            value: native as f32,
        }],
        15.0,
    )
    .unwrap();
    font.verify_design_coordinates(&[(*b"wght", native)])
        .unwrap();
    assert!(matches!(
        font.verify_design_coordinates(&[(*b"wght", 501.0)]),
        Err(MarkdownLayoutFontError::DesignCoordinatesMismatch)
    ));
    for axes in [
        vec![(*b"wght", f64::NAN)],
        vec![(*b"wght", 99.0)],
        vec![(*b"nope", 1.0)],
        vec![(*b"wght", native), (*b"wght", native)],
    ] {
        assert!(font.verify_design_coordinates(&axes).is_err()); // Out-of-range/unknown/duplicate axes cannot pass through fvar clamping or iteration order.
    }
}

#[test]
// Complete original name-table bytes identify one actual face; neither a display alias, a prefix, nor equal font metadata confers allocation authority.
fn native_name_table_matches_complete_unique_face_without_resolving_ui_aliases() {
    let name = tables(ROBOTO).remove(b"name").unwrap();
    let font = MarkdownLayoutFont::resolve_name_table(bytes(ROBOTO), &name, &[]).unwrap();
    let named = MarkdownLayoutFont::resolve(bytes(ROBOTO), NAME, &[]).unwrap();
    assert_eq!(font.metadata(), named.metadata());
    assert!(!font.permits(&named.authority())); // Equal tables identify the face but cannot impersonate the original Font allocation.

    let renamed = renamed();
    let container = collection(&[ROBOTO.to_vec(), renamed.clone()]);
    let second_name = tables(&renamed).remove(b"name").unwrap();
    let second =
        MarkdownLayoutFont::resolve_name_table(bytes(&container), &second_name, &[]).unwrap();
    assert_eq!(
        (second.metadata().face_index, second.metadata().face_count),
        (1, 2)
    );
    assert!(second.metadata().postscript_name.starts_with('X'));
    // Another complete name table within the same TTC cannot use this face's derivation entry; the correct face can still derive an independent size.
    assert!(matches!(
        second.derive_name_table_at_size(&name, &[], 18.0),
        Err(MarkdownLayoutFontError::PostScriptNotFound)
    ));
    let derived = second
        .derive_name_table_at_size(&second_name, &[], 18.0)
        .unwrap();
    assert_eq!(derived.metadata(), second.metadata());
    assert!(!derived.permits(&second.authority()));
    let mut altered = name.clone();
    let last = altered.len() - 1;
    altered[last] ^= 1;
    for candidate in [&altered[..], &name[..name.len() - 1]] {
        assert!(matches!(
            MarkdownLayoutFont::resolve_name_table(bytes(ROBOTO), candidate, &[]),
            Err(MarkdownLayoutFontError::PostScriptNotFound)
        ));
        assert!(matches!(
            font.derive_name_table_at_size(candidate, &[], 18.0),
            Err(MarkdownLayoutFontError::PostScriptNotFound)
        ));
    } // Any differing byte or truncated table cannot reauthorize the original face through an equal PSName prefix.
    let duplicate = collection(&[ROBOTO.to_vec(), ROBOTO.to_vec()]);
    assert!(matches!(
        MarkdownLayoutFont::resolve_name_table(bytes(&duplicate), &name, &[]),
        Err(MarkdownLayoutFontError::AmbiguousFace)
    ));
}

#[test]
// Native table matching retains actual axis validation even though it does not enumerate named instances by synthesized PSName.
fn native_name_table_keeps_axis_validation_and_uses_explicit_actual_coordinates() {
    let variable = named_variable();
    let name = tables(&variable).remove(b"name").unwrap();
    let axis = MarkdownLayoutFontAxis {
        tag: *b"wght",
        value: 550.0,
    };
    let font = MarkdownLayoutFont::resolve_name_table(bytes(&variable), &name, &[axis]).unwrap();
    assert_eq!(font.metadata().axes, vec![axis]);
    assert_eq!(font.metadata().named_instance, None); // Base face plus actual Native axes does not manufacture a named-instance index.
    let derived = font
        .derive_name_table_at_size(
            &name,
            &[MarkdownLayoutFontAxis {
                value: 700.0,
                ..axis
            }],
            18.0,
        )
        .unwrap();
    assert_eq!(derived.metadata().axes[0].value, 700.0);
    derived
        .verify_design_coordinates(&[(*b"wght", 700.0)])
        .unwrap();
    for (axes, expected) in [
        (vec![axis, axis], "duplicate"),
        (
            vec![MarkdownLayoutFontAxis {
                value: 901.0,
                ..axis
            }],
            "range",
        ),
        (
            vec![MarkdownLayoutFontAxis {
                tag: *b"zzzz",
                ..axis
            }],
            "unknown",
        ),
    ] {
        let error = MarkdownLayoutFont::resolve_name_table(bytes(&variable), &name, &axes)
            .err()
            .unwrap();
        let derived_error = font
            .derive_name_table_at_size(&name, &axes, 18.0)
            .err()
            .unwrap();
        assert!(matches!(
            (expected, error),
            ("duplicate", MarkdownLayoutFontError::DuplicateAxis)
                | ("range", MarkdownLayoutFontError::InvalidAxisValue)
                | ("unknown", MarkdownLayoutFontError::UnknownAxis)
        ));
        assert!(matches!(
            (expected, derived_error),
            ("duplicate", MarkdownLayoutFontError::DuplicateAxis)
                | ("range", MarkdownLayoutFontError::InvalidAxisValue)
                | ("unknown", MarkdownLayoutFontError::UnknownAxis)
        ));
    } // Avoiding synthesized-name instance search does not relax actual axis-count, duplicate, unknown-tag, or range constraints.
}

#[test]
// Resolve the actual second face and reject ambiguous or missing PSNames without selecting face zero as a fallback.
fn ttc_exact_second_face_and_ambiguous_names_never_fall_back_to_zero() {
    let value = collection(&[renamed(), ROBOTO.to_vec()]);
    let font = MarkdownLayoutFont::resolve(bytes(&value), NAME, &[]).unwrap();
    assert_eq!(font.metadata().face_index, 1);
    assert_eq!(font.metadata().face_count, 2);
    let duplicate = collection(&[ROBOTO.to_vec(), ROBOTO.to_vec()]);
    assert!(matches!(
        MarkdownLayoutFont::resolve(bytes(&duplicate), NAME, &[]),
        Err(MarkdownLayoutFontError::AmbiguousFace)
    ));
    assert!(matches!(
        MarkdownLayoutFont::resolve(bytes(&value), "missing-postscript", &[]),
        Err(MarkdownLayoutFontError::PostScriptNotFound)
    ));
}

#[test]
// Actual instance coordinates seed public resolution; explicit overrides stay within range and derivation returns to base-face identity.
fn named_instance_coordinates_are_real_and_explicit_axes_are_not_clamped() {
    let value = bytes(&named_variable());
    let named = MarkdownLayoutFont::resolve(Arc::clone(&value), "Roboto-Medium", &[]).unwrap();
    assert_eq!(named.metadata().named_instance, Some(0));
    assert_eq!(
        named.metadata().axes,
        [MarkdownLayoutFontAxis {
            tag: *b"wght",
            value: 500.0
        }]
    );
    let axis = MarkdownLayoutFontAxis {
        tag: *b"wght",
        value: 550.0,
    };
    let explicit =
        MarkdownLayoutFont::resolve(Arc::clone(&value), "Roboto-Medium", &[axis]).unwrap();
    assert_eq!(explicit.metadata().axes, [axis]);
    // Deriving new axes from a named instance retains its original face and restores the base PSName from the same complete name table.
    // Do not keep the old Roboto-Medium label for new axes or implicitly restore its old 500 coordinate from named_instance metadata.
    let name = tables(&named_variable()).remove(b"name").unwrap();
    let derived = named
        .derive_name_table_at_size(&name, &[axis], 18.0)
        .unwrap();
    let resolved =
        MarkdownLayoutFont::resolve_name_table_at_size(Arc::clone(&value), &name, &[axis], 18.0)
            .unwrap();
    assert_eq!(derived.metadata(), resolved.metadata());
    assert_eq!(derived.metadata().postscript_name, NAME);
    assert!(!derived.permits(&named.authority()));
    derived
        .verify_design_coordinates(&[(*b"wght", f64::from(axis.value))])
        .unwrap();
    assert_ne!(
        named.font().normalized_coords(),
        explicit.font().normalized_coords()
    );
    for value in [f32::NAN, f32::INFINITY, 99.0, 901.0] {
        assert!(matches!(
            MarkdownLayoutFont::resolve(
                bytes(&named_variable()),
                "Roboto-Medium",
                &[MarkdownLayoutFontAxis {
                    tag: *b"wght",
                    value
                }]
            ),
            Err(MarkdownLayoutFontError::InvalidAxisValue)
        ));
    }
    assert!(matches!(
        MarkdownLayoutFont::resolve(Arc::clone(&value), "Roboto-Medium", &[axis, axis]),
        Err(MarkdownLayoutFontError::DuplicateAxis)
    ));
    assert!(matches!(
        MarkdownLayoutFont::resolve(
            value,
            "Roboto-Medium",
            &[MarkdownLayoutFontAxis {
                tag: *b"wdth",
                value: 100.0
            }]
        ),
        Err(MarkdownLayoutFontError::UnknownAxis)
    ));
}

#[test]
// Malformed/oversized public inputs fail before unbounded directory/name iteration, with static-face axis requests rejected explicitly.
fn actual_container_and_name_budgets_fail_before_unbounded_metadata_iteration() {
    let mut header = b"ttcf\0\x01\0\0".to_vec();
    header.extend(257_u32.to_be_bytes());
    assert!(matches!(
        MarkdownLayoutFont::resolve(bytes(&header), NAME, &[]),
        Err(MarkdownLayoutFontError::FaceBudgetExceeded)
    ));
    let mut all = tables(ROBOTO);
    all.get_mut(b"name").unwrap()[2..4].copy_from_slice(&4097_u16.to_be_bytes());
    assert!(matches!(
        MarkdownLayoutFont::resolve(bytes(&rebuild(all)), NAME, &[]),
        Err(MarkdownLayoutFontError::NameBudgetExceeded)
    ));
    assert!(matches!(
        MarkdownLayoutFont::resolve(
            bytes(ROBOTO),
            NAME,
            &[MarkdownLayoutFontAxis {
                tag: *b"wght",
                value: 400.0
            }]
        ),
        Err(MarkdownLayoutFontError::UnknownAxis)
    ));
}

#[test]
// A direct base PSName and named-instance PSName can agree only at the actual default coordinate, never by choosing one match first.
fn direct_and_named_postscript_only_agree_at_the_actual_default_position() {
    let conflicting = named_variable_with(NAME, 500);
    assert!(matches!(
        MarkdownLayoutFont::resolve(bytes(&conflicting), NAME, &[]),
        Err(MarkdownLayoutFontError::AmbiguousNamedInstance)
    ));
    let same = named_variable_with(NAME, 400);
    let font = MarkdownLayoutFont::resolve(bytes(&same), NAME, &[]).unwrap();
    assert_eq!(font.metadata().named_instance, Some(0));
    assert_eq!(font.metadata().axes[0].value, 400.0);
}
