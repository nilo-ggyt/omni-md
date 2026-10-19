//! 同一匿名File的不可变item搜索树；固定64个根，无N个内存目录或第二FD。
//!
//! 每次签入一个leaf，只把等大小相邻树合并，摊销少于两个node/item。原始目录始终不改，
//! 回读按原source范围降树，最多64层；不会为每个视觉run从文尾重新扫整串item。
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
            } // 本回合不再写额外目录；旧root仍由原forest持有。
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
/// 在同一个原forest查询中消费已读取节点；保留父子覆盖，不把局部解码结果保成后继查询缓存。
pub(super) fn lookup(
    rows: &mut Rows,
    forest: &[Option<Root>; 64],
    offset: usize,
) -> Result<ItemKey> {
    let root = forest
        .iter()
        .flatten()
        .find(|root| offset >= root.start && offset < root.end)
        .copied()
        .ok_or(Error::InconsistentCoverage)?;
    let mut node = decode(rows.read(root.record)?, root.record)?;
    if node.root.start != root.start
        || node.root.end != root.end
        || node.root.items != root.items
        || node.root.level != root.level
    {
        return Err(Error::InconsistentCoverage); // 首条实际根必须匹配本次原forest的已发布覆盖。
    }
    loop {
        let DecodedNode {
            root,
            children,
            item,
        } = node;
        if let Some(item) = item {
            return Ok(item);
        }
        let (left, right) = children.ok_or(Error::InconsistentCoverage)?;
        let left = decode(rows.read(left)?, left)?;
        let right = decode(rows.read(right)?, right)?;
        if left.root.start != root.start
            || right.root.end != root.end
            || left.root.end != right.root.start
            || left.root.level.checked_add(1) != Some(root.level)
            || right.root.level != left.root.level
        {
            return Err(Error::InconsistentCoverage);
        }
        // 两个孩子均已真实读取并完整解码，选中后只移交这份局部结果；
        // 严格降一层且不跨调用保留，不为下一轮重读相同record或扩大读取范围。
        node = if offset < left.root.end { left } else { right };
    }
}
