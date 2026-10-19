//! 同一不可变root只计算一次内容摘要；外部文件冲突校验仍读取实际文件。

use std::sync::Weak;

use super::*;

/// 一个固定弱键和32B摘要；更换root时覆盖，不建立按代次增长的摘要表。
pub(super) struct RevisionCache {
    root: Option<Weak<MarkdownPieceTreeNode>>,
    revision: MarkdownDocumentRevision,
}

impl RevisionCache {
    /// 仅由已完成UTF8/聚合/SHA与文件身份复核的构造器提供初始真实摘要。
    pub(super) fn seeded(root: &PieceTreeLink, revision: MarkdownDocumentRevision) -> Self {
        Self {
            root: root.as_ref().map(Arc::downgrade),
            revision,
        }
    }

    fn matches(&self, root: &PieceTreeLink) -> bool {
        match (&self.root, root) {
            (Some(weak), Some(root)) => std::ptr::eq(weak.as_ptr(), Arc::as_ptr(root)),
            (None, None) => true, // 空root始终是SHA256空输入，不依赖代次或文件名。
            _ => false,           // 空/非空root不能互相冒充；Weak控制块阻止已释放地址复用。
        }
    }

    pub(super) fn read(
        &mut self,
        source: &impl PieceByteSource,
        root: &PieceTreeLink,
    ) -> Result<MarkdownDocumentRevision, MarkdownDocumentBufferError> {
        source.source_health()?; // 缓存命中也不能绕过共享原映射的永久I/O失败。
        let revision = if self.matches(root) {
            self.revision
        } else {
            // 编辑后的新root才流式读取一次真实piece；不创建全文String或摘要分片Vec。
            let mut hasher = Sha256::new();
            let hashed = hash_piece_tree(source, root.as_deref(), &mut hasher);
            source.source_health()?; // 失败出口也核同源首因，配额错误不能遮住并发已记录的I/O失败。
            hashed?;
            MarkdownDocumentRevision(hasher.finalize().into())
        };
        source.source_health()?; // 最后一次真实lease交还之后核验，再发布该root的摘要。
        self.root = root.as_ref().map(Arc::downgrade);
        self.revision = revision;
        Ok(revision)
    }
}

#[cfg(test)]
mod tests;
