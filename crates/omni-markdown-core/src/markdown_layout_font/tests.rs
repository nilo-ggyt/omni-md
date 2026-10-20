//! 使用仓库已有真实字体和有限合成SFNT元数据，验证精确face、命名实例与owner边界。
use super::*;
use std::collections::BTreeMap;

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));
const NAME: &str = "Roboto-Regular";

fn bytes(value: &[u8]) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
    Arc::new(value.to_vec()) // 只有有限测试字体复制；产品resolver只借用调用方原Arc。
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
        result.extend(font); // TTC表偏移按真实全容器字节重定位，不把局部offset误当face0。
    }
    result
}
fn renamed() -> Vec<u8> {
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
    assert!(observer.payload_released()); // 其他同字体字节owner仍在，收据只证明原payload/Font字段。
    assert_eq!(other.metadata().postscript_name, NAME);
}

#[test]
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
    assert!(!bound.permits(&generic.authority())); // 新构造有新权限，不能重签旧Font任务。

    // 0.1是合法Native字号，其双精度原值不能先舍入成6/64点；更大字号仍保原有效范围。
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
fn incomplete_or_unrepresentable_horizontal_metrics_reject_the_entire_font_capture() {
    let name = tables(ROBOTO).remove(b"name").unwrap();
    let original = MarkdownLayoutFont::resolve(bytes(ROBOTO), NAME, &[]).unwrap();
    let count = original.metadata().glyph_count as usize;
    // 这些都是公开捕获边界的真实非法输入，不伪造一份“平台成功”推进表作Native oracle。
    // 数量不足不能被默认hmtx补齐；非有限或超坐标推进不能饱和成另一个有效字宽。
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
        assert!(font.verify_design_coordinates(&axes).is_err()); // 越界/未知/重复不借fvar夹取或枚举顺序通过。
    }
}

#[test]
fn native_name_table_matches_complete_unique_face_without_resolving_ui_aliases() {
    let name = tables(ROBOTO).remove(b"name").unwrap();
    let font = MarkdownLayoutFont::resolve_name_table(bytes(ROBOTO), &name, &[]).unwrap();
    let named = MarkdownLayoutFont::resolve(bytes(ROBOTO), NAME, &[]).unwrap();
    assert_eq!(font.metadata(), named.metadata());
    assert!(!font.permits(&named.authority())); // 相同表只证明face，不能把新Font冒充原Font allocation。

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
    let mut altered = name.clone();
    let last = altered.len() - 1;
    altered[last] ^= 1;
    for candidate in [&altered[..], &name[..name.len() - 1]] {
        assert!(matches!(
            MarkdownLayoutFont::resolve_name_table(bytes(ROBOTO), candidate, &[]),
            Err(MarkdownLayoutFontError::PostScriptNotFound)
        ));
    } // 全表任意字节不同和截短表都不能借PSName前缀补签原face。
    let duplicate = collection(&[ROBOTO.to_vec(), ROBOTO.to_vec()]);
    assert!(matches!(
        MarkdownLayoutFont::resolve_name_table(bytes(&duplicate), &name, &[]),
        Err(MarkdownLayoutFontError::AmbiguousFace)
    ));
}

#[test]
fn native_name_table_keeps_axis_validation_and_uses_explicit_actual_coordinates() {
    let variable = named_variable();
    let name = tables(&variable).remove(b"name").unwrap();
    let axis = MarkdownLayoutFontAxis {
        tag: *b"wght",
        value: 550.0,
    };
    let font = MarkdownLayoutFont::resolve_name_table(bytes(&variable), &name, &[axis]).unwrap();
    assert_eq!(font.metadata().axes, vec![axis]);
    assert_eq!(font.metadata().named_instance, None); // 来源是base face加真实Native轴位，不假造命名实例索引。
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
        assert!(matches!(
            (expected, error),
            ("duplicate", MarkdownLayoutFontError::DuplicateAxis)
                | ("range", MarkdownLayoutFontError::InvalidAxisValue)
                | ("unknown", MarkdownLayoutFontError::UnknownAxis)
        ));
    } // 不按合成名称搜索实例，不等于放松真实轴数量、重复项、未知项或取值范围约束。
}

#[test]
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
