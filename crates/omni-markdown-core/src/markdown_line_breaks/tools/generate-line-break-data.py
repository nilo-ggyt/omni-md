#!/usr/bin/env python3
"""从固定Unicode18官方文件产生只读区间；运行时不加载UCD或保存正文。"""
from pathlib import Path
import hashlib
import sys

ROOT = Path(__file__).resolve().parent.parent
if len(sys.argv) != 2:
    raise SystemExit("usage: generate-line-break-data.py <Unicode18-official-data-directory>")
DATA = Path(sys.argv[1])
EXPECTED = {
    "LineBreak.txt": "ae8cf1970c73f3f1a12d77852df96c4b2b1723ca8b388e36bb5739250c6849de",
    "LineBreakTest.txt": "fd9ff9eca411080085b41471bba79ae19d9db2e16924f9770b8509f1affd3f95",
    "EastAsianWidth.txt": "a0cf29eacd00cfcaec4381c6b7c281685f18dbb4e7ff82b4076ccb342ca839aa",
    "emoji-data.txt": "80d00f8e616a0ef27fd6b8de3b758c06383b5d917e2977709578e68baf733bf1",
    "UnicodeData.txt": "0736451de439ae7baf1425136617da495e09ee5afbe6e394374db7009ea08950",
}
for filename, expected in EXPECTED.items():
    actual = hashlib.sha256((DATA / filename).read_bytes()).hexdigest()
    if actual != expected:
        raise SystemExit(f"Unicode18构建输入身份不符: {filename}: {actual}")

def records(filename):
    for line in (DATA / filename).read_text().splitlines():
        value = line.split("#", 1)[0].strip()
        if not value:
            continue
        span, prop = [part.strip() for part in value.split(";")[:2]]
        endpoints = span.split("..")
        yield int(endpoints[0], 16), int(endpoints[-1], 16), prop

limit = 0x110000
classes = ["XX"] * limit
flags = bytearray([8]) * limit  # 默认Cn；UnicodeData中的实际记录逐项撤销。
for start, end, prop in records("LineBreak.txt"):
    classes[start:end + 1] = [prop] * (end + 1 - start)
for start, end, prop in records("EastAsianWidth.txt"):
    if prop in {"F", "W", "H"}:
        for value in range(start, end + 1):
            flags[value] |= 16
for start, end, prop in records("emoji-data.txt"):
    if prop == "Extended_Pictographic":
        for value in range(start, end + 1):
            flags[value] |= 32
first = None
for line in (DATA / "UnicodeData.txt").read_text().splitlines():
    fields = line.split(";")
    value = int(fields[0], 16)
    if fields[1].endswith(", First>"):
        first = (value, fields[2])
        continue
    if fields[1].endswith(", Last>"):
        if first is None or first[1] != fields[2]:
            raise SystemExit("UnicodeData First/Last范围不成对")
        start, category = first
        first = None
    else:
        start, category = value, fields[2]
    category_flags = {"Pi": 1, "Pf": 2, "Mn": 4, "Mc": 4}.get(category, 0)
    for point in range(start, value + 1):
        flags[point] = (flags[point] & ~8) | category_flags
flags[0x25CC] |= 64

names = sorted(set(classes))
out = ["// Unicode18官方数据生成；来源与精确SHA由tools/generate-line-break-data.py固定，许可见LICENSE-UNICODE。",
       "#[derive(Clone, Copy, Debug, Eq, PartialEq)]", "pub(crate) enum Class {"]
out.extend(f"    {name.title()}," for name in names)
out.extend(["}", "use Class::*;", "pub(crate) static RANGES: &[(u32, u32, Class, u8)] = &["])
start = 0
for end in range(1, limit + 1):
    if end == limit or (classes[end], flags[end]) != (classes[start], flags[start]):
        out.append(f"    (0x{start:X}, 0x{end - 1:X}, {classes[start].title()}, {flags[start]}),")
        start = end
out.extend([
    "];",
    "pub(crate) fn properties(value: char) -> (Class, u8) {",
    "    let value = u32::from(value);",
    "    let index = RANGES.partition_point(|(_, end, _, _)| *end < value);",
    "    let (_, _, class, flags) = RANGES[index];",
    "    (class, flags)", "}",
])
(ROOT / "unicode_data.rs").write_text("\n".join(out) + "\n")
print(f"unicode18_tables ranges={len(out) - len(names) - 14} source_identity_verified=true")
