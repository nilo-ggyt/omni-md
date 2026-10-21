//! Immutable item search trees in the same anonymous file. Sixty-four fixed roots
//! avoid an in-memory directory per item or another file descriptor.
//!
//! Each inserted leaf merges only equal-sized adjacent trees, amortizing fewer than
//! two nodes per item. Original directories stay immutable; Source-range lookup
//! descends at most 64 levels instead of rescanning every item for each visual run.
use super::*;
const NODE: u8 = 13;

#[derive(Clone, Copy)]
pub(super) struct Root {
    record: usize,
    start: usize,
    end: usize,
    items: usize,
    level: u8,
}

/// Actual leaf and adjacent right subtree from one descent, retained only with the
/// current LocatedItem borrow. The right subtree starts exactly at the leaf's end;
/// a nearby coordinate or arbitrary ancestor cannot supply successor authority.
pub(super) struct Located {
    pub(super) key: ItemKey,
    pub(super) following: Option<Root>,
}
pub(super) struct Build {
    key: ItemKey,
    start: usize,
    end: usize,
    current: Option<Root>,
}
impl Build {
    pub(super) fn new(key: ItemKey, range: Range<usize>) -> Self {
        Self {
            key,
            start: range.start,
            end: range.end,
            current: None,
        }
    }
    pub(super) fn key(&self) -> ItemKey {
        self.key
    }
    pub(super) fn step(
        &mut self,
        rows: &mut Rows,
        forest: &mut [Option<Root>; 64],
        budget: usize,
    ) -> Result<bool> {
        // Append the original item's leaf once. Across budget yields, retain its
        // current merge root while the forest still owns unmerged left roots.
        let mut written = 0;
        if self.current.is_none() {
            let record = rows.len();
            let root = Root {
                record,
                start: self.start,
                end: self.end,
                items: 1,
                level: 0,
            };
            rows.append(encode(root, None, Some(self.key))?)?;
            self.current = Some(root);
            written += 1;
        }
        loop {
            let current = self.current.ok_or(Error::InconsistentCoverage)?;
            let slot = forest
                .get_mut(usize::from(current.level))
                .ok_or(Error::Overflow)?;
            let Some(left) = *slot else {
                *slot = Some(current);
                return Ok(true);
            };
            if written == budget {
                return Ok(false);
            } // Do not spend another record; the original forest still owns left.
            // Merge only equal-size, equal-level adjacent Source coverage. Child
            // records remain immutable and precede the newly appended parent.
            if left.end != current.start
                || left.items != current.items
                || left.level != current.level
            {
                return Err(Error::InconsistentCoverage);
            }
            let root = Root {
                record: rows.len(),
                start: left.start,
                end: current.end,
                items: left
                    .items
                    .checked_add(current.items)
                    .ok_or(Error::Overflow)?,
                level: current.level.checked_add(1).ok_or(Error::Overflow)?,
            };
            rows.append(encode(root, Some((left.record, current.record)), None)?)?;
            *slot = None;
            self.current = Some(root);
            written += 1;
        }
    }
}
fn encode(root: Root, children: Option<(usize, usize)>, item: Option<ItemKey>) -> Result<[u8; 64]> {
    // A fixed record stores either actual child pointers or the original opaque
    // item key. Sentinel fields remain explicit so decoding can reject mixed forms.
    let mut row = [0; 64];
    for (at, value) in [(0, root.start), (8, root.end), (48, root.items)] {
        row[at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
    }
    let (left, right) = children
        .map(|(left, right)| -> Result<_> { Ok((number(left)?, number(right)?)) })
        .transpose()?
        .unwrap_or((NONE, NONE));
    row[16..24].copy_from_slice(&left.to_le_bytes());
    row[24..32].copy_from_slice(&right.to_le_bytes());
    row[32..40].copy_from_slice(
        &item
            .map(|value| number(value.record))
            .transpose()?
            .unwrap_or(NONE)
            .to_le_bytes(),
    );
    row[40..48].copy_from_slice(&item.map_or(0, |value| value.origin).to_le_bytes());
    row[56] = NODE;
    row[57] = u8::from(item.is_some());
    row[58] = root.level;
    Ok(row)
}
struct DecodedNode {
    root: Root,
    children: Option<(usize, usize)>,
    item: Option<ItemKey>,
}
fn decode(row: [u8; 64], record: usize) -> Result<DecodedNode> {
    // Authenticate the record kind/reserved bytes and balanced-tree cardinality
    // before any pointer can guide another read from the original file.
    if row[56] != NODE || row[57] > 1 || row[59..].iter().any(|byte| *byte != 0) {
        return Err(Error::InconsistentCoverage);
    }
    let root = Root {
        record,
        start: usize_at(&row, 0)?,
        end: usize_at(&row, 8)?,
        items: usize_at(&row, 48)?,
        level: row[58],
    };
    if root.start >= root.end
        || root.level >= 64
        || 1usize.checked_shl(u32::from(root.level)) != Some(root.items)
    {
        return Err(Error::InconsistentCoverage);
    }
    if row[57] == 1 {
        // Leaves reference an earlier committed directory with a nonzero origin.
        let key = ItemKey {
            record: usize_at(&row, 32)?,
            origin: u64_at(&row, 40),
        };
        if root.level != 0
            || key.record >= record
            || key.origin == 0
            || u64_at(&row, 16) != NONE
            || u64_at(&row, 24) != NONE
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(DecodedNode {
            root,
            children: None,
            item: Some(key),
        })
    } else {
        // Internal nodes reference two earlier nodes and cannot also claim an item.
        let children = (usize_at(&row, 16)?, usize_at(&row, 24)?);
        if root.level == 0
            || children.0 >= record
            || children.1 >= record
            || u64_at(&row, 32) != NONE
            || u64_at(&row, 40) != 0
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(DecodedNode {
            root,
            children: Some(children),
            item: None,
        })
    }
}
/// Locate an actual leaf in the original forest. Return authenticated parent/child
/// coverage and the adjacent right subtree only for this line-local LocatedItem.
pub(super) fn lookup(
    rows: &mut Rows,
    forest: &[Option<Root>; 64],
    offset: usize,
) -> Result<Located> {
    let root = forest
        .iter()
        .flatten()
        .find(|root| offset >= root.start && offset < root.end)
        .copied()
        .ok_or(Error::InconsistentCoverage)?;
    lookup_root(rows, root, offset)
}

/// Only the preceding line-local item's real end may use its authenticated right
/// subtree. Across trees, require the exact start of an original fixed root. Without
/// that fact, return None for ordinary lookup without adding a cache or path stack.
pub(super) fn lookup_following(
    rows: &mut Rows,
    forest: &[Option<Root>; 64],
    following: Option<Root>,
    offset: usize,
) -> Result<Option<Located>> {
    let root = following.or_else(|| {
        forest
            .iter()
            .flatten()
            .find(|root| root.start == offset)
            .copied()
    });
    let Some(root) = root else {
        return Ok(None);
    };
    if root.start != offset {
        return Err(Error::InconsistentCoverage);
    } // A contradictory carried adjacency is corruption, not a successful fallback.
    lookup_root(rows, root, offset).map(Some)
}

/// Original roots and carried right subtrees share actual reads, decoding, and
/// coverage authentication. Reread the root from Rows even when its scalar facts
/// are known, preserving index health/poison and actual first I/O failure.
fn lookup_root(rows: &mut Rows, root: Root, offset: usize) -> Result<Located> {
    if offset < root.start || offset >= root.end {
        return Err(Error::InconsistentCoverage);
    }
    let mut node = decode(rows.read(root.record)?, root.record)?;
    if node.root.start != root.start
        || node.root.end != root.end
        || node.root.items != root.items
        || node.root.level != root.level
    {
        return Err(Error::InconsistentCoverage); // Match the actual published root facts.
    }
    let mut following = None;
    loop {
        let DecodedNode {
            root,
            children,
            item,
        } = node;
        if let Some(item) = item {
            if following.is_some_and(|next: Root| next.start != root.end) {
                return Err(Error::InconsistentCoverage);
            }
            return Ok(Located {
                key: item,
                following,
            });
        }
        let (left, right) = children.ok_or(Error::InconsistentCoverage)?;
        let left = decode(rows.read(left)?, left)?;
        let right = decode(rows.read(right)?, right)?;
        // Both real children must exactly partition their parent at the next level;
        // a malformed edge cannot turn into a valid item or successor location.
        if left.root.start != root.start
            || right.root.end != root.end
            || left.root.end != right.root.start
            || left.root.level.checked_add(1) != Some(root.level)
            || right.root.level != left.root.level
        {
            return Err(Error::InconsistentCoverage);
        }
        // Move the already decoded child within this descent instead of rereading
        // it. Only the adjacent Root may be borrowed by this same visual run; no
        // cross-run/line cache or expanded Source authority is created.
        node = if offset < left.root.end {
            // Both children were read. A deeper right sibling is the nearer actual
            // successor; rightward descent retains the ancestor candidate. At the
            // leaf, its start must equal that leaf's exact end.
            following = Some(right.root);
            left
        } else {
            right
        };
    }
}
