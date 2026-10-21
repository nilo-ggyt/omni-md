#!/usr/bin/env python3
"""Compare explicit official UCD files with the released crate offline and emit a compact Rust property provider."""

# Inputs are versioned final files from https://www.unicode.org/Public/{version}/ucd/.
# DerivedBidiClass is under extracted/; BidiBrackets, UnicodeData, and ReadMe are at the UCD root.
# export-bidi-public-api.rs exports the fixed crate's actual API results. When supplied,
# --public-records verifies every scalar against them; neither tools nor raw UCD run in the product.

from __future__ import annotations

import argparse
import hashlib
import json
import re
import struct
from pathlib import Path


LIMIT = 0x110000
CLASSES = (
    "AL", "AN", "B", "BN", "CS", "EN", "ES", "ET", "FSI", "L", "LRE",
    "LRI", "LRO", "NSM", "ON", "PDF", "PDI", "R", "RLE", "RLI", "RLO", "S", "WS",
)
CLASS_IDS = {name: index for index, name in enumerate(CLASSES)}
LONG_NAMES = {
    "Left_To_Right": "L", "Right_To_Left": "R", "Arabic_Letter": "AL",
    "European_Terminator": "ET",
}


def scalar(point: int) -> bool:
    # Rust char excludes surrogates. Parse the full official data, excluding these points
    # only from scalar comparison and emitted deltas.
    return 0 <= point < LIMIT and not 0xD800 <= point <= 0xDFFF


def extent(value: str) -> tuple[int, int]:
    pieces = value.strip().split("..")
    start, end = int(pieces[0], 16), int(pieces[-1], 16)
    if not 0 <= start <= end < LIMIT or len(pieces) not in (1, 2):
        raise ValueError(f"非法UCD范围: {value}")
    return start, end


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def published_tables(path: Path) -> tuple[bytearray, dict[int, tuple[int, bool]], str]:
    # Parse the explicit released baseline and reject unrecognized table syntax instead
    # of allowing parser omissions to masquerade as legitimate Unicode-version changes.
    text = path.read_text(encoding="utf-8")
    version_match = re.search(r"UNICODE_VERSION[^=]*=\s*\((\d+),\s*(\d+),\s*(\d+)\)", text)
    if version_match is None:
        raise ValueError("发布表没有Unicode版本")
    version = ".".join(version_match.groups())
    class_text, pair_text = text.split("pub const bidi_class_table", 1)[1].split("pub const bidi_pairs_table", 1)
    classes = bytearray([CLASS_IDS["L"]]) * LIMIT
    previous = -1
    class_pattern = r"\('\\u\{([0-9a-fA-F]+)\}',\s*'\\u\{([0-9a-fA-F]+)\}',\s*(\w+)\)"
    records = re.findall(class_pattern, class_text)
    if len(records) < 1000:
        raise ValueError("发布表语法未完整识别")
    class_array = class_text.split("= &[", 1)[1].split("];", 1)[0]
    if re.sub(r"[\s,]", "", re.sub(class_pattern, "", class_array)):
        raise ValueError("发布属性表存在未识别的记录，禁止把漏解析误当成版本增量")
    for lo, hi, name in records:
        start, end = int(lo, 16), int(hi, 16)
        if not previous < start <= end < LIMIT:
            raise ValueError("发布属性区间有重叠或乱序")
        classes[start:end + 1] = bytes([CLASS_IDS[name]]) * (end - start + 1)
        previous = end
    brackets: dict[int, tuple[int, bool]] = {}
    pair_pattern = r"\('\\u\{([0-9a-fA-F]+)\}',\s*'\\u\{([0-9a-fA-F]+)\}',\s*(None|Some\('\\u\{[0-9a-fA-F]+\}'\))\)"
    pairs = re.findall(pair_pattern, pair_text)
    if len(pairs) < 50:
        raise ValueError("发布括号表语法未完整识别")
    pair_array = pair_text.split("= &[", 1)[1].split("];", 1)[0]
    if re.sub(r"[\s,]", "", re.sub(pair_pattern, "", pair_array)):
        raise ValueError("发布括号表存在未识别的记录")
    for opening, closing, normalization in pairs:
        op, close = int(opening, 16), int(closing, 16)
        normalized = op if normalization == "None" else int(re.search(r"\{([^}]+)\}", normalization).group(1), 16)
        if op in brackets or close in brackets:
            raise ValueError("发布括号键重复")
        brackets[op], brackets[close] = (normalized, True), (normalized, False)
    return classes, brackets, version


def public_api_records(path: Path, parsed_classes: bytearray, parsed_brackets: dict[int, tuple[int, bool]], parsed_version: str) -> tuple[bytearray, dict[int, tuple[int, bool]]]:
    # Require the complete fixed-width export, matching data version, and strict scalar
    # order before any actual public result can validate the parsed baseline.
    expected_records = LIMIT - 0x800
    if path.stat().st_size != 40 + 10 * expected_records:
        raise ValueError("公开API文件长度不完整，禁止使用部分输出")
    classes = bytearray([CLASS_IDS["L"]]) * LIMIT
    brackets = {}
    with path.open("rb") as source:
        magic, major, minor, patch, records, size = struct.unpack("<8sQQQII", source.read(40))
        if magic != b"OMUBIDI1" or records != expected_records or size != 10:
            raise ValueError("公开API文件schema不匹配")
        if f"{major}.{minor}.{patch}" != parsed_version:
            raise ValueError("运行的crate数据版本与解析的发布表不同")
        for point in range(LIMIT):
            if not scalar(point):
                continue
            actual_point, value, tag, opening = struct.unpack("<IBBI", source.read(10))
            if actual_point != point or value >= len(CLASSES) or tag not in (0, 1, 2):
                raise ValueError(f"公开API记录顺序/编码无效: {point:X}")
            if (tag == 0 and opening != 0) or (tag != 0 and not scalar(opening)):
                raise ValueError("公开API括号记录无效")
            bracket = None if tag == 0 else (opening, tag == 1)
            # Check actual public return values item by item so a faulty source parser
            # cannot generate an apparently plausible version delta.
            if value != parsed_classes[point] or bracket != parsed_brackets.get(point):
                raise ValueError(f"发布源码解析和实际公开API不一致: U+{point:04X}")
            classes[point] = value
            if bracket is not None:
                brackets[point] = bracket
        if source.read(1):
            raise ValueError("公开API文件含额外未校验记录")
    return classes, brackets


def official_classes(path: Path, version: str) -> bytearray:
    lines = path.read_text(encoding="utf-8").splitlines()
    if not lines or lines[0] != f"# DerivedBidiClass-{version}.txt":
        raise ValueError(f"属性版本不匹配: {path}")
    classes = bytearray([255]) * LIMIT
    missing = []
    explicit = []
    for line in lines:
        # Specific @missing ranges override general L defaults; explicit assignments
        # override both last, preserving unassigned RTL regions too.
        match = re.match(r"#\s*@missing:\s*([^;]+);\s*(\w+)", line)
        if match:
            start, end = extent(match.group(1))
            name = LONG_NAMES.get(match.group(2), match.group(2))
            missing.append((start, end, CLASS_IDS[name]))
            continue
        data = line.split("#", 1)[0].strip()
        if data:
            location, name = (field.strip() for field in data.split(";"))
            start, end = extent(location)
            explicit.append((start, end, CLASS_IDS[name]))
    if not missing or missing[0] != (0, LIMIT - 1, CLASS_IDS["L"]):
        raise ValueError("缺失完整Unicode默认值，禁止猜测空洞为L")
    for start, end, value in missing:
        classes[start:end + 1] = bytes([value]) * (end - start + 1)
    assigned = bytearray(LIMIT)
    for start, end, value in explicit:
        if any(assigned[start:end + 1]):
            raise ValueError(f"官方显式属性重叠: {start:X}..{end:X}")
        assigned[start:end + 1] = bytes([1]) * (end - start + 1)
        classes[start:end + 1] = bytes([value]) * (end - start + 1)
    if 255 in classes:
        raise ValueError("官方属性未覆盖全部Unicode范围")
    return classes


def canonical_decompositions(path: Path) -> dict[int, tuple[int, ...]]:
    # Only canonical decompositions contribute to BD16 equivalence. Compatibility
    # mappings remain outside this bracket-identity contract.
    result = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        fields = line.split(";")
        if len(fields) != 15:
            raise ValueError("UnicodeData字段数不符")
        decomposition = fields[5]
        if decomposition and not decomposition.startswith("<"):
            result[int(fields[0], 16)] = tuple(int(value, 16) for value in decomposition.split())
    return result


def normalized_opening(point: int, decompositions: dict[int, tuple[int, ...]], seen: frozenset[int] = frozenset()) -> int:
    if point in seen:
        raise ValueError("规范分解发生环")
    decomposition = decompositions.get(point)
    if decomposition is None:
        return point
    # BD16 groups canonically equivalent opening brackets. BidiDataSource keys are
    # single scalars, so a decomposition tail cannot be silently discarded.
    if len(decomposition) != 1:
        raise ValueError(f"括号规范分解不是单标量: {point:X}")
    return normalized_opening(decomposition[0], decompositions, seen | {point})


def official_brackets(path: Path, unicode_data: Path, version: str, classes: bytearray) -> dict[int, tuple[int, bool]]:
    # Verify version, actual ON classes, reciprocal pairs, and canonical opening identity
    # directly from the supplied official files rather than assuming the old bracket set.
    lines = path.read_text(encoding="utf-8").splitlines()
    if not lines or lines[0] != f"# BidiBrackets-{version}.txt":
        raise ValueError("括号版本不匹配")
    raw = {}
    for line in lines:
        data = line.split("#", 1)[0].strip()
        if not data:
            continue
        point, pair, kind = (field.strip() for field in data.split(";"))
        code, matching = int(point, 16), int(pair, 16)
        if not scalar(code) or not scalar(matching) or kind not in ("o", "c") or code in raw:
            raise ValueError("括号键/类型无效")
        if classes[code] != CLASS_IDS["ON"]:
            raise ValueError("真实括号不是ON，不能使用旧稳定假设")
        raw[code] = matching, kind
    decompositions = canonical_decompositions(unicode_data)
    result = {}
    for point, (matching, kind) in raw.items():
        if raw.get(matching) != (point, "c" if kind == "o" else "o"):
            raise ValueError("括号配对不对称")
        opening = point if kind == "o" else matching
        result[point] = normalized_opening(opening, decompositions), kind == "o"
    return result


def deltas(old: bytearray, new: bytearray) -> list[tuple[int, int, int, int]]:
    # Preserve both original and target classes in each contiguous changed interval;
    # unchanged points and changed-value transitions close the current interval.
    if len(old) != LIMIT or len(new) != LIMIT:
        raise ValueError("属性比较范围不是完整Unicode码点空间")
    result = []
    start = previous = None
    for point, (before, after) in enumerate(zip(old, new)):
        changed = scalar(point) and before != after
        identity = before, after
        if start is not None and (not changed or identity != previous):
            result.append((start, point - 1, *previous))
            start = None
        if changed and start is None:
            start, previous = point, identity
    if start is not None:
        result.append((start, LIMIT - 1, *previous))
    return result


def compact(changes: list[tuple[int, int, int, int]]) -> list[tuple[int, int, int]]:
    # Runtime lookup needs only target values. Merge adjacent equal targets without
    # bridging unchanged holes between independently proven delta intervals.
    result = []
    for start, end, _old, value in changes:
        if result and result[-1][1] + 1 == start and result[-1][2] == value:
            result[-1] = result[-1][0], end, value
        else:
            result.append((start, end, value))
    return result


def emit(path: Path, version: str, crate_version: str, changes: list[tuple[int, int, int, int]], bracket_changes: dict[int, tuple[int, bool] | None], sources: list[Path], license_path: Path) -> None:
    # Maintain output explanations here, then regenerate from the original authenticated
    # inputs and released public API. Preserve license text, provenance hashes, property
    # records, and baseline-version assertions independently of these comment templates.
    version_tuple = ", ".join(version.split("."))
    base_parts = crate_version.split(".")
    license_lines = "\n".join(f"// {line}" if line else "//" for line in license_path.read_text(encoding="utf-8").splitlines())
    lines = [
        "//! Official UCD property deltas for unicode-bidi 0.3.18; UBA rules remain unchanged.",
        f"//! Data version {version}; use the exact released hardcoded-data baseline used during generation.",
        "//! Generated by generate-bidi-data-delta.py; UnicodeData supplies only BD16 canonical opening identities.",
        "// SPDX-License-Identifier: Unicode-3.0",
        license_lines,
        "",
    ]
    for source in sources:
        lines.append(f"// {source.name} SHA256 {sha(source)}")
    lines.extend([
        "",
        "use unicode_bidi::data_source::BidiMatchedOpeningBracket;",
        "use unicode_bidi::{BidiClass, BidiDataSource, HardcodedBidiData};",
        "",
        f"pub const UNICODE_VERSION: (u64, u64, u64) = ({version_tuple});",
        "// Regenerate after updating the crate's data; deltas cannot be applied to a different baseline.",
        "const _: () = assert!(",
        f"    unicode_bidi::UNICODE_VERSION.0 == {base_parts[0]}",
        f"        && unicode_bidi::UNICODE_VERSION.1 == {base_parts[1]}",
        f"        && unicode_bidi::UNICODE_VERSION.2 == {base_parts[2]}",
        ");",
        "",
        "/// Static target-version properties over the independently verified released baseline.",
        "pub struct UnicodeBidiData;",
        "",
        "impl BidiDataSource for UnicodeBidiData {",
        "    fn bidi_class(&self, scalar: char) -> BidiClass {",
        "        let point = scalar as u32;",
        "        // Delta intervals cover every actual change. Unchanged scalars retain the pinned released table.",
        "        let slot = CLASS_DELTAS.partition_point(|&(_, end, _)| end < point);",
        "        match CLASS_DELTAS.get(slot) {",
        "            Some(&(start, _, value)) if start <= point => value,",
        "            _ => HardcodedBidiData.bidi_class(scalar),",
        "        }",
        "    }",
        "",
        "    fn bidi_matched_opening_bracket(&self, scalar: char) -> Option<BidiMatchedOpeningBracket> {",
        "        // Explicit None removes a target-version bracket; absence of a delta alone uses the old table.",
        "        match BRACKET_DELTAS.binary_search_by_key(&scalar, |&(point, _)| point) {",
        "            Ok(slot) => BRACKET_DELTAS[slot].1.map(|(opening, is_open)| BidiMatchedOpeningBracket { opening, is_open }),",
        "            Err(_) => HardcodedBidiData.bidi_matched_opening_bracket(scalar),",
        "        }",
        "    }",
        "}",
        "",
        "const CLASS_DELTAS: &[(u32, u32, BidiClass)] = &[",
    ])
    for start, end, value in compact(changes):
        lines.append(f"    (0x{start:X}, 0x{end:X}, BidiClass::{CLASSES[value]}),")
    lines.extend([
        "];", "", "const BRACKET_DELTAS: &[(char, Option<(char, bool)>)] = &[",
    ])
    for point, value in sorted(bracket_changes.items()):
        entry = "None" if value is None else f"Some(('\\u{{{value[0]:X}}}', {str(value[1]).lower()}))"
        lines.append(f"    ('\\u{{{point:X}}}', {entry}),")
    lines.extend([
        "];", "",
        "// Original per-interval facts support offline controls and boundary tests, without production allocation.",
        "#[cfg(test)]",
        "pub const CHANGED_BOUNDARIES: &[(u32, u32, BidiClass, BidiClass)] = &[",
    ])
    for start, end, before, after in changes:
        lines.append(f"    (0x{start:X}, 0x{end:X}, BidiClass::{CLASSES[before]}, BidiClass::{CLASSES[after]}),")
    lines.extend(["];"])
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--crate-tables", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--baseline-version", default="16.0.0")
    parser.add_argument("--public-records", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    old, old_brackets, crate_version = published_tables(args.crate_tables)
    if args.public_records is not None:
        old, old_brackets = public_api_records(args.public_records, old, old_brackets, crate_version)
    root = args.root
    versions = tuple(dict.fromkeys((args.baseline_version, args.version)))
    official = {}
    for version in versions:
        readme = (root / f"ReadMe-{version}.txt").read_text(encoding="utf-8")
        release = " ".join(readme.split()).lower()
        if "contains final data files" not in release or f"version {version}" not in release:
            raise ValueError(f"官方ReadMe未明确final: {version}")
        classes = official_classes(root / f"DerivedBidiClass-{version}.txt", version)
        brackets = official_brackets(root / f"BidiBrackets-{version}.txt", root / f"UnicodeData-{version}.txt", version, classes)
        official[version] = classes, brackets
    target, target_brackets = official[args.version]
    changes = deltas(old, target)
    bracket_changes = {point: target_brackets.get(point) for point in old_brackets.keys() | target_brackets.keys() if old_brackets.get(point) != target_brackets.get(point)}
    # Reconstruct the generated provider offline to prove compaction preserved holes
    # and all explicit/default property differences across the complete scalar space.
    projected = bytearray(old)
    for start, end, value in compact(changes):
        projected[start:end + 1] = bytes([value]) * (end - start + 1)
    if any(scalar(point) and left != right for point, (left, right) in enumerate(zip(projected, target))):
        raise ValueError("压缩后的属性provider不能精确还原官方数据")
    projected_brackets = dict(old_brackets)
    for point, value in bracket_changes.items():
        if value is None:
            projected_brackets.pop(point, None)
        else:
            projected_brackets[point] = value
    if projected_brackets != target_brackets:
        raise ValueError("括号增量不能精确还原规范等价关系")
    inputs = [root / f"{leaf}-{args.version}.txt" for leaf in ("DerivedBidiClass", "BidiBrackets", "UnicodeData", "ReadMe")]
    inputs.append(args.crate_tables)
    if args.public_records is not None:
        inputs.append(args.public_records)
    emit(args.output, args.version, crate_version, changes, bracket_changes, inputs, root / "UNICODE-LICENSE.txt")
    # Print finite statistics and actual changed intervals without writing an additional
    # report file or a Unicode-text mirror.
    baseline_changes = deltas(old, official[args.baseline_version][0])
    version_changes = deltas(official[args.baseline_version][0], target)
    print(json.dumps({
        "target": args.version, "published_data": crate_version,
        "public_api_verified": args.public_records is not None,
        "scalars_compared": LIMIT - 0x800,
        "baseline_official_gap_points": sum(end - start + 1 for start, end, _, _ in baseline_changes),
        "baseline_official_gap_ranges": len(baseline_changes),
        "official_version_change_points": sum(end - start + 1 for start, end, _, _ in version_changes),
        "published_to_target_points": sum(end - start + 1 for start, end, _, _ in changes),
        "published_to_target_ranges": len(changes), "compact_ranges": len(compact(changes)),
        "bracket_changes": len(bracket_changes), "target_bracket_entries": len(target_brackets),
        "baseline_bracket_changes": sum(old_brackets.get(point) != official[args.baseline_version][1].get(point) for point in old_brackets.keys() | official[args.baseline_version][1].keys()),
        "changes": [[f"{start:04X}", f"{end:04X}", CLASSES[before], CLASSES[after]] for start, end, before, after in changes],
        "output": str(args.output), "output_sha256": sha(args.output),
    }, ensure_ascii=False))


if __name__ == "__main__":
    main()
