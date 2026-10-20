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

/// 同一次降树实际取得的叶与紧邻右子树；只随本次 LocatedItem 借用，不保存历史路径。
/// following 的首端必须正好是该叶末端，不能把任意祖先或相近数值当成后继权限。
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
/// 在原 forest 中定位实际叶；父子覆盖和紧邻右子树只随本次行内 LocatedItem 返回。
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

/// 仅原行内上一项的真实末端可借已认证右子树；跨树时只接受原固定根的精确首端。
/// 没有该事实就交回 None，由调用方保留普通 lookup；不增加目录、路径栈或跨调用缓存。
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
    } // 已带出的邻接事实错位不能悄悄退回普通查询，把原目录错误洗成成功。
    lookup_root(rows, root, offset).map(Some)
}

/// 原根与带出的右子树共用同一真实读取、解码和父子覆盖认证。
/// 即使 Root 已读过，仍从原 Rows 读取它，保留 index 的健康/poison 与 IO 首错。
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
        return Err(Error::InconsistentCoverage); // 首条实际根必须匹配本次原forest的已发布覆盖。
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
        if left.root.start != root.start
            || right.root.end != root.end
            || left.root.end != right.root.start
            || left.root.level.checked_add(1) != Some(root.level)
            || right.root.level != left.root.level
        {
            return Err(Error::InconsistentCoverage);
        }
        // 完整 DecodedNode 只在本次降树中移交，选中孩子后不重读相同 record。
        // 仅带出的相邻 Root 可供本次视觉 run 借用，不跨 run/行缓存或扩大读取范围。
        node = if offset < left.root.end {
            // 两个孩子已经在本次查询中完整读过；越靠近叶的右孩子就是更近的真实后继。
            // 向右下降时保留上层候选；最终只带出首端与当前叶末端完全相同的那一棵。
            following = Some(right.root);
            left
        } else {
            right
        };
    }
}
