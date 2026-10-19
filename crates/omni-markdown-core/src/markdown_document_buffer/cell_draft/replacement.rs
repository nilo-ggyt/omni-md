//! 同一次Cell清洗的有限堆快路与独占匿名输出；长结果不保留整格堆字节。

use super::*;
use crate::immutable_snapshot::CanonicalSnapshotOutput;
use crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_BYTES;

/// 准入完成后只持最终字节资源，不保raw root、Doc或第三个Scene读取名额。
pub(super) enum CellReplacement {
    Inline(String),
    Frozen(Arc<OriginalBacking>),
}

impl CellReplacement {
    /// 原canonical预算使用真实输出字节数，文件映射extent不当作堆分配计量。
    /// 仅观察已接受输出extent，文件缓冲尚未flush时也不触发正文复制。
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Inline(text) => text.len(),
            Self::Frozen(backing) => backing.len(),
        }
    }

    /// 唯一准入claim只记录实际保留的堆正文容量；封存资源由真实backing owner负责。
    pub(super) fn capacity(&self) -> usize {
        match self {
            Self::Inline(text) => text.capacity(),
            Self::Frozen(_) => 0,
        }
    }

    /// 候选文件还未成为canonical Source，失败不能poison尚未改变的raw草稿。
    pub(super) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        match self {
            Self::Inline(_) => Ok(()),
            Self::Frozen(backing) => backing
                .health()
                .map_err(|error| edit_read_error(error.into(), false)),
        }
    }

    /// 比较原canonical的一个真实小片；不因发现差异而跳过调用方后继Source读取。
    pub(super) fn matches_bytes(
        &self,
        offset: usize,
        bytes: &[u8],
    ) -> Result<bool, MarkdownDocumentBufferError> {
        match self {
            Self::Inline(text) => {
                Ok(text.as_bytes().get(offset..offset + bytes.len()) == Some(bytes))
            }
            Self::Frozen(backing) => {
                let lease = backing
                    .lease(offset..offset + bytes.len())
                    .map_err(|error| edit_read_error(error.into(), false))?;
                let same = &*lease == bytes;
                lease
                    .finish()
                    .map_err(|error| edit_read_error(error.into(), false))?;
                Ok(same)
            }
        }
    }

    /// 同一封存结果进入原PreparedTextInput；没有第二种公开提交票或重新清洗阶段。
    pub(super) fn input(&self, utf16: usize) -> prepared_text_input::TextInputReplacement<'_> {
        match self {
            Self::Inline(text) => prepared_text_input::TextInputReplacement::Text(text),
            Self::Frozen(backing) => prepared_text_input::TextInputReplacement::Frozen {
                backing: Arc::clone(backing),
                utf16,
            },
        }
    }

    #[cfg(test)]
    /// 直接业务测试观察这个实际候选的最后munmap，不从claim数字推断文件已经释放。
    pub(super) fn backing_observer(&self) -> Option<crate::MarkdownOriginalBackingObserver> {
        match self {
            Self::Inline(_) => None,
            Self::Frozen(backing) => Some(backing.observer()),
        }
    }
}

enum OutputStorage {
    Inline(String),
    File(CanonicalSnapshotOutput),
}

/// 清洗和真实表尾续写共享一个输出；只有这一个4KiB缓冲，完成后才封存唯一写者。
pub(super) struct CellReplacementOutput {
    storage: OutputStorage,
    bytes: usize,
    utf16: usize,
    scratch: [u8; ORIGINAL_PIECE_BYTES],
    scratch_len: usize,
}

impl CellReplacementOutput {
    /// 64Ki UTF16仅选择原有限String快路；更大的输入直接写匿名文件而非功能拒绝。
    pub(super) fn new(
        metrics: MarkdownDocumentMetrics,
        prefix: &str,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        let storage = if metrics.utf16_len <= MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS {
            let capacity = metrics
                .byte_len
                .saturating_add(metrics.utf16_len)
                .saturating_add(prefix.len())
                .min(TABLE_WINDOW_MAX_BYTES);
            OutputStorage::Inline(String::with_capacity(capacity))
        } else {
            OutputStorage::File(CanonicalSnapshotOutput::new().map_err(edit_snapshot_error)?)
        };
        let mut output = Self {
            storage,
            bytes: 0,
            utf16: 0,
            scratch: [0; ORIGINAL_PIECE_BYTES],
            scratch_len: 0,
        };
        output.push_text(prefix)?; // 已认证的implicit分隔符先进入输出，但不参与正文的全局trim状态。
        Ok(output)
    }

    pub(super) fn len(&self) -> usize {
        self.bytes
    }
    /// 同次标量写入累计的UTF16长度，用于原候选选区和变化日志。
    pub(super) fn utf16_len(&self) -> usize {
        self.utf16
    }

    /// 大尾部padding或容器前缀不能把小Cell扩成无界String；在增长前改由同一匿名工厂承接。
    fn require_file(&mut self, next_bytes: usize) -> Result<(), MarkdownDocumentBufferError> {
        if next_bytes <= TABLE_WINDOW_MAX_BYTES {
            return Ok(()); // 原有限快路仍满足既有堆预算，不为小提交增加文件操作。
        }
        self.promote()
    }

    /// 已认证的超量源尾部只允许流式落盘；它不能通过多次小push累积成另一份大String。
    pub(super) fn prepare_suffix(
        &mut self,
        bytes: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if bytes > EDIT_SEGMENT_BYTES {
            self.promote()?;
        }
        self.require_file(
            self.bytes
                .checked_add(bytes)
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
        )
    }

    /// 原有限快路字节完整转入一个匿名文件后才释放String，失败整体放弃候选。
    fn promote(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        if let OutputStorage::Inline(text) = &self.storage {
            let mut file = CanonicalSnapshotOutput::new().map_err(edit_snapshot_error)?;
            let mut offset = 0;
            while offset < text.len() {
                let mut end = (offset + ORIGINAL_PIECE_BYTES).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                } // 仅在完整标量之间转移有限堆快路字节。
                file.write_all(&text.as_bytes()[offset..end])
                    .map_err(edit_snapshot_error)?;
                offset = end;
            }
            self.storage = OutputStorage::File(file); // 所有旧有限字节成功写入后才Drop原String。
        }
        Ok(())
    }

    /// 接受一个完整UTF-8片段，逐4KiB写入；调用方仍负责最终文档/会话容量的原事务准入。
    pub(super) fn push_text(&mut self, text: &str) -> Result<(), MarkdownDocumentBufferError> {
        let next = self
            .bytes
            .checked_add(text.len())
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        let units = self
            .utf16
            .checked_add(text.encode_utf16().count())
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        self.require_file(next)?;
        match &mut self.storage {
            OutputStorage::Inline(output) => output.push_str(text),
            OutputStorage::File(_) => {
                let mut offset = 0;
                while offset < text.len() {
                    if self.scratch_len == self.scratch.len() {
                        self.flush()?;
                    } // 满块先真实写出，不保第二份输出buffer。
                    let available = self.scratch.len() - self.scratch_len;
                    let mut end = (offset + available).min(text.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    if end == offset {
                        self.flush()?; // 剩余尾隙不足下个完整标量；下一轮从空缓冲继续。
                        continue;
                    }
                    let chunk = &text.as_bytes()[offset..end];
                    self.scratch[self.scratch_len..self.scratch_len + chunk.len()]
                        .copy_from_slice(chunk);
                    self.scratch_len += chunk.len();
                    offset = end;
                }
            }
        }
        self.bytes = next;
        self.utf16 = units;
        Ok(())
    }

    /// Source回调只交当前真实小片；UTF-8验证失败仍由原lease归还后再传播。
    pub(super) fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), MarkdownDocumentBufferError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)?;
        self.push_text(text)
    }

    /// 全局trim只回退已写尾部；即使尾空白很长也不缓存，后续表尾从新EOF继续写入。
    pub(super) fn truncate(
        &mut self,
        bytes: usize,
        utf16: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.flush()?;
        let bytes = match &mut self.storage {
            OutputStorage::Inline(text) => {
                text.truncate(bytes);
                text.len()
            }
            OutputStorage::File(file) => {
                file.truncate(bytes).map_err(edit_snapshot_error)?;
                file.len()
            }
        };
        self.bytes = bytes;
        self.utf16 = utf16;
        Ok(())
    }

    /// 尾缓冲不跨seal；冻结后唯一File写权被消费，映射通过原owner真正关闭descriptor。
    pub(super) fn seal(
        mut self,
        claim: &mut super::resources::CommitClaim,
    ) -> Result<(CellReplacement, usize), MarkdownDocumentBufferError> {
        self.flush()?;
        let replacement = match self.storage {
            OutputStorage::Inline(text) => CellReplacement::Inline(text),
            OutputStorage::File(file) => match file.freeze().map_err(edit_snapshot_error)? {
                Some(backing) if backing.len() <= EDIT_SEGMENT_BYTES => {
                    // 原raw可以很长而trim后的有效结果很小；只保这份有界结果，
                    // 先真正归还匿名映射，再把有限堆字节交给原提交票。
                    let observer = backing.observer();
                    claim.observe_backing(observer.clone()); // 必须先绑定，覆盖读取或最终释放的真实失败。
                    let copied = (|| {
                        let mut bytes = Vec::with_capacity(backing.len());
                        while bytes.len() < backing.len() {
                            let end = (bytes.len() + ORIGINAL_PIECE_BYTES).min(backing.len());
                            let lease = backing
                                .lease(bytes.len()..end)
                                .map_err(|error| edit_read_error(error.into(), false))?;
                            bytes.extend_from_slice(&lease);
                            lease
                                .finish()
                                .map_err(|error| edit_read_error(error.into(), false))?;
                        }
                        String::from_utf8(bytes)
                            .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)
                    })();
                    drop(backing);
                    let text = copied?; // 真实读取首错不被随后清理结果覆盖，也不污染raw Source。
                    if let Some(errno) = observer.first_errno() {
                        return Err(MarkdownDocumentBufferError::EditStorageUnavailable(errno));
                    }
                    if !observer.unmap_succeeded() {
                        // 只有最终munmap成功才能交付Inline；未归还的实际映射不能冒充小结果完成。
                        return Err(MarkdownDocumentBufferError::EditStorageUnavailable(
                            libc::EIO,
                        ));
                    }
                    claim.release_backing_ticket(); // 临时映射已实际释放；Inline成功不向Doc转移文件义务。
                    CellReplacement::Inline(text)
                }
                Some(backing) => {
                    claim.observe_backing(backing.observer()); // 大候选仍独占此映射，直到失败退场或原publish接管。
                    CellReplacement::Frozen(Arc::new(backing))
                }
                None => CellReplacement::Inline(String::new()), // 全局trim为空时没有映射，不制造零长文件叶。
            },
        };
        Ok((replacement, self.utf16))
    }

    /// 只有真实File分支拥有缓冲字节；失败后整个候选退场，不继续复用部分写入输出。
    fn flush(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        if self.scratch_len == 0 {
            return Ok(());
        }
        let OutputStorage::File(file) = &mut self.storage else {
            unreachable!("有限String快路不持文件缓冲");
        };
        file.write_all(&self.scratch[..self.scratch_len])
            .map_err(edit_snapshot_error)?;
        self.scratch_len = 0;
        Ok(())
    }
}
