//! 单个8KiB派生UTF-16窗口；原piece树与匿名映射仍是唯一正文来源。

use std::sync::Weak;

use super::*;

pub(super) const READ_WINDOW_UTF16_UNITS: usize = 4096;

/// Weak保留控制块身份而不保留树；即使分配器复用其他地址也不能误认旧root。
struct ReadKey {
    root: Option<Weak<MarkdownPieceTreeNode>>,
    generation: u64,
    utf16_len: usize,
}

impl ReadKey {
    fn matches(&self, root: &PieceTreeLink, generation: u64, utf16_len: usize) -> bool {
        // generation和真实聚合长度都必须一致；root比较不升级Weak或读取正文。
        if self.generation != generation || self.utf16_len != utf16_len {
            return false;
        }
        match (&self.root, root) {
            (Some(weak), Some(root)) => std::ptr::eq(weak.as_ptr(), Arc::as_ptr(root)),
            (None, None) => true,
            // 空/非空root的权限不可互换，即使两者恰有相同的外部代次。
            _ => false,
        }
    }
}

#[derive(Default)]
/// 未读时没有正文分配；替换窗口原地复用同一box，不累计已访问页。
pub(super) struct Utf16ReadCache {
    key: Option<ReadKey>,
    units: Option<Box<[u16; READ_WINDOW_UTF16_UNITS]>>,
    valid: Range<usize>,
}

impl Utf16ReadCache {
    #[cfg(test)]
    pub(super) fn allocated_window_for_test(&self) -> Option<(usize, usize)> {
        // 仅测试观察同一个实际Box与其固定容量，不导出正文、指针或生产保活句柄。
        self.units
            .as_ref()
            .map(|units| (units.as_ptr().addr(), std::mem::size_of_val(units.as_ref())))
    }

    pub(super) fn copy(
        &mut self,
        source: &impl PieceByteSource,
        root: &PieceTreeLink,
        generation: u64,
        offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        source.source_health()?;
        let utf16_len = piece_tree_metrics(root).utf16_len;
        let end = offset
            .checked_add(destination.len())
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        if end > utf16_len {
            // 在写入调用方内存或分配窗口之前拒绝全部非法范围，不静默截断。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if !self
            .key
            .as_ref()
            .is_some_and(|key| key.matches(root, generation, utf16_len))
        {
            // 真实root/generation任一变化就撤销旧窗口；Weak不保活旧正文或历史。
            self.valid = 0..0;
            self.key = Some(ReadKey {
                root: root.as_ref().map(Arc::downgrade),
                generation,
                utf16_len,
            });
        }
        let mut written = 0;
        while written < destination.len() {
            // 每轮先核共享源健康；其他线程在旧窗口填满后发生的回收错误也必须传播。
            source.source_health()?;
            let current = offset + written;
            if !self.valid.contains(&current) {
                // 对齐的是UTF-16单元而非标量，允许代理对跨两个真实读取窗口。
                let start = current - current % READ_WINDOW_UTF16_UNITS;
                let end = start.saturating_add(READ_WINDOW_UTF16_UNITS).min(utf16_len);
                self.refill(source, root, start..end)?;
            }
            let count = (self.valid.end - current).min(destination.len() - written);
            let local = current - self.valid.start;
            let units = self
                .units
                .as_ref()
                .ok_or(MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)?;
            // 只复制调用方请求的交集；预取窗口不会改变外部length或实际UTF-16坐标。
            destination[written..written + count].copy_from_slice(&units[local..local + count]);
            written += count;
        }
        // 空范围也有健康检查；不能让hit或无字符操作绕过原共享源的永久失败。
        source.source_health()
    }

    fn refill(
        &mut self,
        source: &impl PieceByteSource,
        root: &PieceTreeLink,
        range: Range<usize>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        // 先撤销原权限再原地覆盖；任何读取/真实回收失败都只留下不可发布的存储。
        self.valid = 0..0;
        let units = self
            .units
            .get_or_insert_with(|| Box::new([0; READ_WINDOW_UTF16_UNITS]));
        let mut written = 0;
        copy_utf16_tree_range(
            source,
            root.as_deref(),
            range.clone(),
            0,
            &mut units[..range.len()],
            &mut written,
        )?;
        if written != range.len() {
            // 树必须精确覆盖全部窗口；短读不能发布有效range或补零伪造正文。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        source.source_health()?;
        self.valid = range;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
