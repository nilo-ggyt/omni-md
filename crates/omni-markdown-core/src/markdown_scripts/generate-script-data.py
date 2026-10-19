#!/usr/bin/env python3
"""从固定Unicode18发布文件生成脚本属性；原始数据只由显式输入目录提供。"""
import argparse
import hashlib
from pathlib import Path

VERSION = "18.0.0"
EXPECTED = {
    "Scripts.txt": "0071fd81b6aeae25f6e8bce8efec3066a6476a91b49bdb2f52dc76e817862a6a",
    "ScriptExtensions.txt": "5c9d34a922f687726f2a8bcf57d49f905987e51f1b21b58c95a00fbe255cec23",
    "PropertyValueAliases.txt": "06c4c8eaf7b0bf34abe73b113da1215bd784ac254d4c223600b90267caa4bbbd",
    "BidiBrackets.txt": "4b3b62e4a14b84ee752808c810c602534921c09a4a1bf78cfbee566d66c125b3",
    "UnicodeData.txt": "0736451de439ae7baf1425136617da495e09ee5afbe6e394374db7009ea08950",
}


def records(path):
    # 先核固定发布内容，不让同名dev文件或较新版本悄悄替换属性基线。
    raw = path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != EXPECTED[path.name]:
        raise ValueError(f"wrong Unicode {VERSION} input: {path.name}")
    return [
        [field.strip() for field in line.split("#", 1)[0].split(";")]
        for line in raw.decode("utf-8").splitlines()
        if line.split("#", 1)[0].strip()
    ]


def extent(value):
    pair = value.split("..")
    return int(pair[0], 16), int(pair[-1], 16)


def merge(rows):
    result = []
    for first, last, value in sorted(rows):
        if result and result[-1][1] + 1 == first and result[-1][2] == value:
            result[-1] = (result[-1][0], last, value)
        else:
            result.append((first, last, value))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ucd", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    inputs = {name: records(args.ucd / name) for name in EXPECTED}
    aliases = {row[2]: row[1] for row in inputs["PropertyValueAliases.txt"] if row[0] == "sc"}
    tags = sorted(set(aliases.values()))
    ids = {tag: slot for slot, tag in enumerate(tags)}
    assert len(tags) == 179 and len(tags) <= 192
    scripts = merge([(*extent(row[0]), ids[aliases[row[1]]]) for row in inputs["Scripts.txt"]])
    extension_sets = sorted({tuple(row[1].split()) for row in inputs["ScriptExtensions.txt"]})
    assert len(extension_sets) == 120 and max(map(len, extension_sets)) == 23
    set_ids = {value: slot for slot, value in enumerate(extension_sets)}
    extensions = merge([
        (*extent(row[0]), set_ids[tuple(row[1].split())])
        for row in inputs["ScriptExtensions.txt"]
    ])
    marks, separators = [], []
    first_range = None
    for row in inputs["UnicodeData.txt"]:
        point = int(row[0], 16)
        if row[1].endswith(", First>"):
            first_range = point
            continue
        first = point if first_range is None else first_range
        first_range = None
        if row[2] in {"Mn", "Mc", "Me"}:
            marks.append((first, point, 1))
        if row[4] == "B":
            separators.append((first, point, 1))
    assert first_range is None
    marks, separators = merge(marks), merge(separators)
    openings = sorted(int(row[0], 16) for row in inputs["BidiBrackets.txt"] if row[2] == "o")
    assert len(openings) == 65
    bracket_ids = {point: slot for slot, point in enumerate(openings)}
    brackets = []
    for row in inputs["BidiBrackets.txt"]:
        point, pair = int(row[0], 16), int(row[1], 16)
        opening = point if row[2] == "o" else pair
        # U+2329/232A与3008/3009属于同一规范等价配对；沿现有UBA的BD16身份。
        if opening == 0x2329:
            opening = 0x3008
        brackets.append((point, bracket_ids[opening], row[2] == "o"))
    header = Path(__file__).parents[1] / "markdown_bidi" / "unicode_data.rs"
    license_lines = header.read_text().splitlines()
    start = next(i for i, line in enumerate(license_lines) if line.startswith("// SPDX-License-Identifier:"))
    end = next(i for i in range(start + 1, len(license_lines)) if license_lines[i].startswith("// ") and "SHA256" in license_lines[i])
    output = [
        "//! 由固定Unicode18脚本数据生成；generate-script-data.py核每个输入SHA后重建。",
        *license_lines[start:end],
        *[f"// {name} SHA256 {digest}" for name, digest in EXPECTED.items()],
        "pub(super) const UNICODE_VERSION: (u8, u8, u8) = (18, 0, 0);",
        f"pub(super) const COMMON: u8 = {ids['Zyyy']};",
        f"pub(super) const INHERITED: u8 = {ids['Zinh']};",
        f"pub(super) const UNKNOWN: u8 = {ids['Zzzz']};",
        f"pub(super) const TAGS: [[u8; 4]; {len(tags)}] = [",
        *[f'    *b"{tag}",' for tag in tags], "];",
    ]
    for name, rows in [("SCRIPTS", scripts), ("EXTENSIONS", extensions), ("MARKS", marks), ("PARAGRAPHS", separators)]:
        output += [f"pub(super) const {name}: &[(u32, u32, u8)] = &["]
        output += [f"    (0x{first:X}, 0x{last:X}, {value})," for first, last, value in rows]
        output += ["]; "]
    output += [f"pub(super) const SETS: [[u64; 3]; {len(extension_sets)}] = ["]
    for group in extension_sets:
        words = [0, 0, 0]
        for tag in group:
            words[ids[tag] // 64] |= 1 << (ids[tag] % 64)
        output += ["    [" + ", ".join(f"0x{word:016X}" for word in words) + "],"]
    output += ["];", "pub(super) const BRACKETS: &[(u32, u8, bool)] = &["]
    output += [f"    (0x{point:X}, {kind}, {str(opening).lower()})," for point, kind, opening in sorted(brackets)]
    output += ["];", ""]
    args.output.write_text("\n".join(output))
    print(f"scripts={len(scripts)} scx_ranges={len(extensions)} scx_sets={len(extension_sets)} marks={len(marks)} bracket_records={len(brackets)}")


if __name__ == "__main__":
    main()
