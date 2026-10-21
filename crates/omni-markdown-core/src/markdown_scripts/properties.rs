//! Scalar facts from the same static Unicode 18 tables. Rendering-script policy
//! remains separate from the original Script/Script_Extensions properties.
use super::unicode_data;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Set(pub(super) [u64; 3]);
impl Set {
    /// Dense generated script IDs select one bit in a fixed three-word set;
    /// no per-scalar allocation or dynamic collection is required.
    pub(super) fn singleton(script: u8) -> Self {
        let mut words = [0; 3];
        words[usize::from(script) / 64] = 1 << (script % 64);
        Self(words)
    }
    /// Tests the same generated ID domain used by construction and SCX tables.
    pub(super) fn contains(self, script: u8) -> bool {
        self.0[usize::from(script) / 64] & (1 << (script % 64)) != 0
    }
    /// An empty intersection is an explicit incompatibility requiring a run
    /// boundary, never a wildcard that could merge unrelated scripts.
    pub(super) fn intersection(self, other: Self) -> Option<Self> {
        let words = std::array::from_fn(|i| self.0[i] & other.0[i]);
        words.iter().any(|&word| word != 0).then_some(Self(words))
    }
    /// Returns deterministic generated-ID order only; preferred primary or
    /// preceding-script selection is resolved by the run machine before this fallback.
    pub(super) fn first(self) -> Option<u8> {
        // Generated Script_Extensions sets are nonempty; consumers continue set
        // intersection only when at least one actual script remains.
        for (index, word) in self.0.into_iter().enumerate() {
            if word != 0 {
                return Some((index * 64 + word.trailing_zeros() as usize) as u8);
            }
        }
        None
    }
}

#[derive(Clone, Copy)]
pub(super) struct Bracket {
    pub(super) kind: u8,
    pub(super) opening: bool,
}
pub(super) struct Properties {
    pub(super) primary: u8,
    pub(super) scripts: Option<Set>,
    pub(super) mark: bool,
    pub(super) paragraph: bool,
    pub(super) bracket: Option<Bracket>,
}
/// Finds a scalar in sorted disjoint property ranges without expanding a table
/// or assigning the next range's property to an unlisted gap.
fn lookup(table: &[(u32, u32, u8)], scalar: u32) -> Option<u8> {
    let slot = table.partition_point(|&(_, end, _)| end < scalar);
    table
        .get(slot)
        .and_then(|&(start, _, value)| (start <= scalar).then_some(value))
}
/// Reads raw script, mark, paragraph, and bracket facts from one pinned data
/// version. Wildcard/preceding-script choices remain the run machine's policy.
pub(super) fn get(scalar: char) -> Properties {
    let point = scalar as u32;
    let primary = lookup(unicode_data::SCRIPTS, point).unwrap_or(unicode_data::UNKNOWN);
    let scripts = match lookup(unicode_data::EXTENSIONS, point) {
        Some(slot) => Some(Set(unicode_data::SETS[usize::from(slot)])),
        None if matches!(primary, unicode_data::COMMON | unicode_data::INHERITED) => None,
        None => Some(Set::singleton(primary)),
    }; // Policy treats default Common/Inherited as wildcard; explicit SCX stays constrained.
    let bracket = unicode_data::BRACKETS
        .binary_search_by_key(&point, |&(value, _, _)| value)
        .ok()
        .map(|slot| {
            let (_, kind, opening) = unicode_data::BRACKETS[slot];
            Bracket { kind, opening }
        });
    Properties {
        primary,
        scripts,
        mark: lookup(unicode_data::MARKS, point).is_some(),
        paragraph: lookup(unicode_data::PARAGRAPHS, point).is_some(),
        bracket,
    }
}
