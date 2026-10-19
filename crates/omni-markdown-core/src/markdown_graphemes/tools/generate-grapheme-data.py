#!/usr/bin/env python3
"""从指定的官方18文件生成静态属性；只处理属性，不读取产品或用户正文。"""
import hashlib
import sys
from pathlib import Path

root = Path(sys.argv[1])
out = Path(sys.argv[2])
specs = [
    ('18.0.0-auxiliary-GraphemeBreakProperty.txt', '0839dcb79e4ac639ecd538b1abf7c9d22e3f9dd265b7e182d33627aa4d75b45a', 'GCB'),
    ('18.0.0-DerivedCoreProperties.txt', '09c928886a178fcafd93c29e4bd59073a058e5a100b716d425cb563ab50f68c9', 'INCB'),
    ('18.0.0-emoji-emoji-data.txt', '80d00f8e616a0ef27fd6b8de3b758c06383b5d917e2977709578e68baf733bf1', 'EP'),
]
gcb = {'Other': 0, 'CR': 1, 'LF': 2, 'Control': 3, 'Extend': 4, 'ZWJ': 5, 'Regional_Indicator': 6, 'Prepend': 7, 'SpacingMark': 8, 'L': 9, 'V': 10, 'T': 11, 'LV': 12, 'LVT': 13}
incb = {'None': 0, 'Consonant': 1, 'Extend': 2, 'Linker': 3}
lines = ['//! 自动生成的Unicode18静态属性；生成器逐文件核SHA256，未包含正文缓存。', '//! 上游属性遵守LICENSE-UNICODE；生成过程不采用宿主Rust版本的Unicode属性。', '']
for filename, expected, table in specs:
    data = (root / filename).read_bytes()
    actual = hashlib.sha256(data).hexdigest()
    if actual != expected:
        raise RuntimeError(f'{filename}: checksum mismatch')
    rows = []
    for line in data.decode().splitlines():
        text = line.split('#', 1)[0].strip()
        if not text:
            continue
        fields = [f.strip() for f in text.split(';')]
        if table == 'INCB':
            if fields[1] != 'InCB':
                continue
            value = incb[fields[2]]
        elif table == 'EP':
            if fields[1] != 'Extended_Pictographic':
                continue
            value = 1
        else:
            value = gcb[fields[1]]
        span = fields[0].split('..')
        rows.append((int(span[0], 16), int(span[-1], 16), value))
    rows.sort()
    merged = []
    for first, last, value in rows:
        if merged and first <= merged[-1][1]:
            raise RuntimeError('overlapping property ranges')
        if merged and first == merged[-1][1] + 1 and value == merged[-1][2]:
            merged[-1] = (merged[-1][0], last, value)
        else:
            merged.append((first, last, value))
    lines.append(f'// {filename} SHA256 {actual}')
    lines.append(f'pub(super) static {table}: &[(u32, u32, u8)] = &[')
    lines.extend(f'    (0x{first:x}, 0x{last:x}, {value}),' for first, last, value in merged)
    lines.extend(['];', ''])
out.write_text('\n'.join(lines))
