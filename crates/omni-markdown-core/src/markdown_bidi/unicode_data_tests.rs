//! 属性查表和官方UCD分别验证；测试可以保存有限属性区间，产品不引入整空间数组。

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::ops::RangeInclusive;
use std::path::Path;

use super::{BidiClass, BidiDataSource, UnicodeBidiData, unicode_data};
use unicode_bidi::HardcodedBidiData;

#[test]
fn unicode18_delta_boundaries_override_the_exact_published_baseline() {
    let data = UnicodeBidiData;
    let mut changed = 0;
    for &(first, last, previous, current) in unicode_data::CHANGED_BOUNDARIES {
        assert!(first <= last);
        assert_ne!(previous, current);
        // 同时覆盖包含端点和单点区间；发布表的真实返回值也须与生成时基线一致。
        for point in [first, last] {
            let scalar = char::from_u32(point).unwrap();
            assert_eq!(HardcodedBidiData.bidi_class(scalar), previous);
            assert_eq!(data.bidi_class(scalar), current, "U+{point:04X}");
        }
        changed += last - first + 1;
    }
    assert_eq!(changed, 4198);
    assert_eq!(unicode_data::CHANGED_BOUNDARIES.len(), 80);
    // 未受增量影响的ASCII、Hebrew及既有括号继续消费固定发布表，不能覆盖邻近空洞。
    for scalar in [
        'a', 'א', '(', ')', '\u{05c7}', '\u{05ca}', '\u{2e5f}', '\u{2e64}',
    ] {
        assert_eq!(
            data.bidi_class(scalar),
            HardcodedBidiData.bidi_class(scalar)
        );
    }
}

#[test]
fn unicode18_default_al_noncharacters_new_marks_and_bracket_equivalence_are_preserved() {
    use BidiClass::*;
    let data = UnicodeBidiData;
    for scalar in ['\u{086b}', '\u{088f}', '\u{0892}', '\u{10d28}', '\u{1ec70}'] {
        assert_eq!(data.bidi_class(scalar), AL); // 未分配RTL区的@missing不能被通用L或邻块R取代。
    }
    for scalar in [
        '\u{2065}',
        '\u{fdd0}',
        '\u{ffff}',
        '\u{e0fff}',
        '\u{10ffff}',
    ] {
        assert_eq!(data.bidi_class(scalar), BN); // 不可见/非字符仍有规范属性，不能因不显示而省略。
    }
    for scalar in ['\u{05c8}', '\u{0b53}', '\u{1acf}', '\u{10ecb}', '\u{1d127}'] {
        assert_eq!(data.bidi_class(scalar), NSM);
    }
    assert_eq!(data.bidi_class('\u{1b3a}'), L);
    for (scalar, opening, is_open) in [
        ('\u{2e62}', '\u{2e62}', true),
        ('\u{2e63}', '\u{2e62}', false),
        ('\u{2329}', '\u{3008}', true),
        ('\u{232a}', '\u{3008}', false),
        ('\u{3009}', '\u{3008}', false),
    ] {
        let bracket = data.bidi_matched_opening_bracket(scalar).unwrap();
        assert_eq!((bracket.opening, bracket.is_open), (opening, is_open));
    }
    assert!(data.bidi_matched_opening_bracket('\u{2e61}').is_none());
}

struct OfficialClassRange {
    points: RangeInclusive<u32>,
    class: BidiClass,
}

fn parse_extent(value: &str) -> RangeInclusive<u32> {
    let (first, last) = value
        .trim()
        .split_once("..")
        .unwrap_or((value.trim(), value.trim()));
    let first = u32::from_str_radix(first, 16).unwrap();
    let last = u32::from_str_radix(last, 16).unwrap();
    assert!(first <= last && last <= 0x10ffff);
    first..=last
}

fn parse_class(value: &str) -> BidiClass {
    use BidiClass::*;
    match value.trim() {
        "AL" | "Arabic_Letter" => AL,
        "AN" => AN,
        "B" => B,
        "BN" => BN,
        "CS" => CS,
        "EN" => EN,
        "ES" => ES,
        "ET" | "European_Terminator" => ET,
        "FSI" => FSI,
        "L" | "Left_To_Right" => L,
        "LRE" => LRE,
        "LRI" => LRI,
        "LRO" => LRO,
        "NSM" => NSM,
        "ON" => ON,
        "PDF" => PDF,
        "PDI" => PDI,
        "R" | "Right_To_Left" => R,
        "RLE" => RLE,
        "RLI" => RLI,
        "RLO" => RLO,
        "S" => S,
        "WS" => WS,
        other => panic!("官方属性值未识别: {other}"), // 数据格式变化必须失败，不能退成L掩盖属性缺口。
    }
}

fn class_ranges(path: &Path) -> (Vec<OfficialClassRange>, Vec<OfficialClassRange>) {
    let mut defaults = Vec::new();
    let mut explicit = Vec::new();
    for (index, line) in std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .enumerate()
    {
        let line = line.unwrap();
        if index == 0 {
            assert_eq!(line, "# DerivedBidiClass-18.0.0.txt");
        }
        let (payload, missing) = if let Some(value) = line.strip_prefix("# @missing:") {
            (value.trim(), true)
        } else {
            (line.split('#').next().unwrap().trim(), false)
        };
        if payload.is_empty() {
            continue; // 普通注释和空行没有属性定义，不能影响默认区域优先级。
        }
        let (points, class) = payload.split_once(';').expect("官方属性必须有范围和值");
        let row = OfficialClassRange {
            points: parse_extent(points),
            class: parse_class(class),
        };
        if missing {
            defaults.push(row); // 顺序保留通用默认值与后续更具体的默认值，显式行最后覆盖两者。
        } else {
            explicit.push(row);
        }
    }
    assert_eq!(defaults.first().unwrap().points, 0..=0x10ffff);
    assert_eq!(defaults.first().unwrap().class, BidiClass::L);
    assert!(explicit.len() > 1000 && explicit.len() < 5000);
    explicit.sort_unstable_by_key(|row| *row.points.start());
    for pair in explicit.windows(2) {
        assert!(
            pair[0].points.end() < pair[1].points.start(),
            "官方显式行不能重复覆盖同一点"
        );
    }
    (defaults, explicit)
}

fn official_brackets(path: &Path, unicode_data: &Path) -> BTreeMap<u32, (u32, bool)> {
    let mut raw = BTreeMap::new();
    for (index, line) in std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .enumerate()
    {
        let line = line.unwrap();
        if index == 0 {
            assert_eq!(line, "# BidiBrackets-18.0.0.txt");
        }
        let payload = line.split('#').next().unwrap().trim();
        if payload.is_empty() {
            continue;
        }
        let mut fields = payload.split(';').map(str::trim);
        let point = u32::from_str_radix(fields.next().unwrap(), 16).unwrap();
        let pair = u32::from_str_radix(fields.next().unwrap(), 16).unwrap();
        let opening = match fields.next().unwrap() {
            "o" => true,
            "c" => false,
            other => panic!("官方括号类型未识别: {other}"),
        };
        assert!(fields.next().is_none());
        assert!(raw.insert(point, (pair, opening)).is_none());
    }
    let openings: BTreeSet<_> = raw
        .iter()
        .filter_map(|(&point, &(_, open))| open.then_some(point))
        .collect();
    let mut canonical = BTreeMap::new();
    // 只保存真实开括号的规范分解，避免使用不同Unicode版本的NFD库或复制UnicodeData全文。
    for line in std::io::BufReader::new(std::fs::File::open(unicode_data).unwrap()).lines() {
        let line = line.unwrap();
        let mut fields = line.split(';');
        let point = u32::from_str_radix(fields.next().unwrap(), 16).unwrap();
        if !openings.contains(&point) {
            continue;
        }
        let decomposition = fields.nth(4).expect("UnicodeData必须包含规范分解字段");
        if decomposition.is_empty() || decomposition.starts_with('<') {
            continue; // 兼容分解不属于BD16的规范等价关系。
        }
        let mut parts = decomposition.split_whitespace();
        let target = u32::from_str_radix(parts.next().unwrap(), 16).unwrap();
        assert!(
            parts.next().is_none(),
            "括号规范分解必须能表示为单scalar身份"
        );
        assert!(
            openings.contains(&target),
            "规范目标不在真实开括号集合，需审查新数据语义"
        );
        canonical.insert(point, target);
    }
    let mut normalized = BTreeMap::new();
    for (&point, &(pair, is_open)) in &raw {
        assert_eq!(raw.get(&pair), Some(&(point, !is_open)));
        let mut opening = if is_open { point } else { pair };
        let mut hops = 0;
        while let Some(&target) = canonical.get(&opening) {
            opening = target;
            hops += 1;
            assert!(hops <= canonical.len(), "规范分解不能发生环");
        }
        normalized.insert(point, (opening, is_open));
    }
    normalized
}

#[test]
#[ignore = "需OMNI_BIDI_UCD_ROOT指向官方18.0.0属性、括号和UnicodeData下载目录"]
fn official_ucd_properties_match_every_unicode_scalar() {
    let root = std::env::var_os("OMNI_BIDI_UCD_ROOT").expect("必须显式指定官方数据目录");
    let root = Path::new(&root);
    let (defaults, explicit) = class_ranges(&root.join("DerivedBidiClass-18.0.0.txt"));
    let brackets = official_brackets(
        &root.join("BidiBrackets-18.0.0.txt"),
        &root.join("UnicodeData-18.0.0.txt"),
    );
    let mut cursor = 0;
    let mut checked = 0;
    let data = UnicodeBidiData;
    for point in 0..0x110000 {
        let Some(scalar) = char::from_u32(point) else {
            continue; // 代理区不是合法Rust标量，但文件默认区域仍完整保留以防解析遗漏。
        };
        while cursor < explicit.len() && *explicit[cursor].points.end() < point {
            cursor += 1;
        }
        let expected = if cursor < explicit.len() && explicit[cursor].points.contains(&point) {
            explicit[cursor].class
        } else {
            defaults
                .iter()
                .rev()
                .find(|row| row.points.contains(&point))
                .unwrap()
                .class
        };
        assert_eq!(
            data.bidi_class(scalar),
            expected,
            "官方Bidi_Class U+{point:04X}"
        );
        let actual = data
            .bidi_matched_opening_bracket(scalar)
            .map(|value| (value.opening as u32, value.is_open));
        assert_eq!(
            actual,
            brackets.get(&point).copied(),
            "官方规范括号 U+{point:04X}"
        );
        checked += 1;
    }
    assert_eq!(checked, 1_112_064);
    assert_eq!(brackets.len(), 130);
    eprintln!(
        "bidi_official_properties unicode=18.0.0 scalars={checked} bracket_entries={} all_default_regions=true",
        brackets.len()
    );
}
