//! Accumulate exact Unicode facts during original leaf construction, without deriving layout permission.

use crate::unicode_bidi_data::UnicodeBidiData;
use unicode_bidi::{BidiClass, BidiDataSource};

// The complete ASCII domain matches the pinned Unicode 18 provider in a constant
// 512-byte table. Construction needs no heap or lazy state and avoids repeated
// range-table searches for each ASCII scalar in the original leaf traversal.
static ASCII_BIDI_MASKS: [u32; 128] = ascii_bidi_masks();

const fn ascii_bidi_class(byte: u8) -> BidiClass {
    // These exhaustive ASCII intervals encode actual Bidi_Class data, not a
    // character-category heuristic or the third-party enum's memory representation.
    match byte {
        0x00..=0x08 | 0x0e..=0x1b | 0x7f => BidiClass::BN,
        0x09 | 0x0b | 0x1f => BidiClass::S,
        0x0a | 0x0d | 0x1c..=0x1e => BidiClass::B,
        0x0c | b' ' => BidiClass::WS,
        b'#'..=b'%' => BidiClass::ET,
        b'+' | b'-' => BidiClass::ES,
        b',' | b'.' | b'/' | b':' => BidiClass::CS,
        b'0'..=b'9' => BidiClass::EN,
        b'A'..=b'Z' | b'a'..=b'z' => BidiClass::L,
        _ => BidiClass::ON, // Remaining ASCII punctuation; only 0..127 is passed by table construction.
    }
}

const fn ascii_bidi_masks() -> [u32; 128] {
    let mut masks = [0; 128];
    let mut index = 0;
    while index < masks.len() {
        // ASCII and non-ASCII use the same explicit class-to-bit mapping; the
        // fast path must not introduce another ordering for the aggregate set.
        masks[index] = MarkdownUnicodeProperties::class_mask(ascii_bidi_class(index as u8));
        index += 1;
    }
    masks
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MarkdownUnicodeProperties {
    pub(super) bidi_classes: u32,
    // Six bits identify TAB/CR/LF/NEL/LS/PS exactly. Empty sets are the identity
    // for union and do not manufacture a separator for empty text.
    pub(super) separators: u8,
    // Printable-ASCII envelope with zero as the empty-set sentinel. Two bytes
    // use existing alignment space without retaining a per-character bitset.
    pub(super) printable_ascii_min: u8,
    pub(super) printable_ascii_max: u8,
}

impl MarkdownUnicodeProperties {
    pub(super) fn observe(&mut self, scalar: char) {
        // Record existence facts from this actual scalar. These sets do not claim
        // a resolved bidi level, shaping safety, or permission for a later algorithm.
        self.bidi_classes |= if scalar.is_ascii() {
            // Index only the exact ASCII domain; its entries use the same pinned
            // Unicode data as the non-ASCII provider.
            ASCII_BIDI_MASKS[scalar as usize]
        } else {
            // Use the real UBA provider for non-ASCII, including unassigned/new
            // characters, rather than guessing direction from Rust character classes.
            Self::class_mask(UnicodeBidiData.bidi_class(scalar))
        };
        self.separators |= Self::separator_mask(scalar);
        // Only printable ASCII contributes to this envelope. Whether a complete
        // range contains exclusively printable ASCII is a separate original fact.
        if (' '..='~').contains(&scalar) {
            let byte = scalar as u8;
            self.printable_ascii_min = if self.printable_ascii_min == 0 {
                byte
            } else {
                self.printable_ascii_min.min(byte)
            };
            self.printable_ascii_max = self.printable_ascii_max.max(byte);
        }
    }

    pub(super) fn joined(self, other: Self) -> Self {
        // Union is associative, so leaf splitting, tree rotation and immutable
        // history sharing preserve the exact existence sets and envelope bounds.
        Self {
            bidi_classes: self.bidi_classes | other.bidi_classes,
            separators: self.separators | other.separators,
            printable_ascii_min: match (self.printable_ascii_min, other.printable_ascii_min) {
                (0, right) => right,
                (left, 0) => left,
                (left, right) => left.min(right),
            }, // Zero denotes the empty set and must not lower a nonempty ASCII envelope.
            printable_ascii_max: self.printable_ascii_max.max(other.printable_ascii_max),
        }
    }

    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn bidi_classes(self) -> u32 {
        self.bidi_classes
    }

    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn separators(self) -> u8 {
        self.separators
    }

    pub(crate) const fn class_mask(class: BidiClass) -> u32 {
        // Map all 23 real Bidi_Class values explicitly. Aggregate bits do not
        // depend on a dependency's enum layout or implicit discriminant values.
        let bit = match class {
            BidiClass::L => 0,
            BidiClass::R => 1,
            BidiClass::AL => 2,
            BidiClass::EN => 3,
            BidiClass::ES => 4,
            BidiClass::ET => 5,
            BidiClass::AN => 6,
            BidiClass::CS => 7,
            BidiClass::NSM => 8,
            BidiClass::BN => 9,
            BidiClass::B => 10,
            BidiClass::S => 11,
            BidiClass::WS => 12,
            BidiClass::ON => 13,
            BidiClass::LRE => 14,
            BidiClass::LRO => 15,
            BidiClass::RLE => 16,
            BidiClass::RLO => 17,
            BidiClass::PDF => 18,
            BidiClass::LRI => 19,
            BidiClass::RLI => 20,
            BidiClass::FSI => 21,
            BidiClass::PDI => 22,
        };
        1_u32 << bit
    }

    const fn separator_mask(scalar: char) -> u8 {
        // This exact six-character set is intentionally narrower than all UBA
        // S/B/control characters; their bidi classes are already recorded separately.
        match scalar {
            '\t' => 1 << 0,
            '\r' => 1 << 1,
            '\n' => 1 << 2,
            '\u{0085}' => 1 << 3,
            '\u{2028}' => 1 << 4,
            '\u{2029}' => 1 << 5,
            _ => 0,
        }
    }
}

#[cfg(test)]
#[path = "unicode_properties/tests.rs"]
mod tests;
