#!/usr/bin/env python3
"""Generate static properties from explicit Unicode 18 inputs, without reading product or user documents."""
import hashlib
import sys
from pathlib import Path

# This offline generator consumes explicitly supplied Unicode property files, never
# product documents. Keep exact input digests stable; emitted comments belong to
# this source and are published only by regenerating the authenticated tables.
root = Path(sys.argv[1])
out = Path(sys.argv[2])
specs = [
    ('18.0.0-auxiliary-GraphemeBreakProperty.txt', '0839dcb79e4ac639ecd538b1abf7c9d22e3f9dd265b7e182d33627aa4d75b45a', 'GCB'),
    ('18.0.0-DerivedCoreProperties.txt', '09c928886a178fcafd93c29e4bd59073a058e5a100b716d425cb563ab50f68c9', 'INCB'),
    ('18.0.0-emoji-emoji-data.txt', '80d00f8e616a0ef27fd6b8de3b758c06383b5d917e2977709578e68baf733bf1', 'EP'),
]
gcb = {'Other': 0, 'CR': 1, 'LF': 2, 'Control': 3, 'Extend': 4, 'ZWJ': 5, 'Regional_Indicator': 6, 'Prepend': 7, 'SpacingMark': 8, 'L': 9, 'V': 10, 'T': 11, 'LV': 12, 'LVT': 13}
incb = {'None': 0, 'Consonant': 1, 'Extend': 2, 'Linker': 3}
lines = ['//! Generated Unicode 18 static properties; the generator verifies each input SHA256.', '//! Upstream properties retain LICENSE-UNICODE. No document cache or host-Rust Unicode lookup is used.', '']
for filename, expected, table in specs:
    # Authenticate each complete upstream file before deriving any property ranges.
    # A checksum mismatch aborts before the output file is published.
    data = (root / filename).read_bytes()
    actual = hashlib.sha256(data).hexdigest()
    if actual != expected:
        raise RuntimeError(f'{filename}: checksum mismatch')
    rows = []
    for line in data.decode().splitlines():
        # Ignore upstream comments, then select only this table's authoritative property.
        # Unrelated DerivedCoreProperties and emoji rows do not acquire GCB semantics.
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
        # Disjoint sorted intervals support the runtime binary lookup. Merge only
        # adjacent equal values; overlapping upstream ranges remain a generation error.
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
# Publish only after all three verified tables have been assembled. This is build-time
# property data, with no host-Rust Unicode lookup and no runtime source cache.
out.write_text('\n'.join(lines))
