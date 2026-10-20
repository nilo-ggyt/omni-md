//! 布局算法共享同一个冻结正文payload；身份与代次固定，有限借用不授予Native活文档权限。
use std::{
    hash::{Hash, Hasher},
    io::Write,
    ops::Range,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{MarkdownDocumentBuffer, MarkdownDocumentBufferError, MarkdownTextSnapshot};

#[cfg(feature = "paged-line-compose")]
pub(crate) mod folds;

#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
mod table_row;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub use table_row::VerifiedMarkdownLayoutTableRow;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
mod root_leaf_boundary;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub use root_leaf_boundary::VerifiedMarkdownRootLeafStyleBoundary;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub(crate) mod table_rows;

#[cfg(feature = "s2-table-window-prototype")]
mod cell_draft;
#[cfg(feature = "s2-table-window-prototype")]
mod table_cell;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub use table_cell::{
    MarkdownTableCellDisplayPreparation, MarkdownTableCellDisplayPreparationStep,
    MarkdownTableCellDisplayResourceObserver, MarkdownTableCellDisplayResources,
    MarkdownTableCellDisplaySource, MarkdownTableCellVisibleSourceRanges,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 布局源的坐标域；只读格不具 canonical 输入权限，活动草稿也不能借局部坐标修改正文。
pub enum MarkdownLayoutTextSourceDomain {
    Canonical,
    #[cfg(feature = "s2-table-window-prototype")]
    ReadOnlyCell,
    #[cfg(feature = "s2-table-window-prototype")]
    ActiveCellDraft,
}

enum SourceDomain {
    Canonical,
    #[cfg(feature = "s2-table-window-prototype")]
    // 只读格单独持有固定结构 metadata；所有普通正文 capture 不必为最大格上下文放大 payload。
    ReadOnlyCell(Box<crate::markdown_document_buffer::VerifiedMarkdownTableCellSource>),
    #[cfg(feature = "s2-table-window-prototype")]
    ActiveCellDraft(crate::markdown_document_buffer::MarkdownCellDraftSourceAuthority),
}

/// 所有布局算法的单次正文借用上限；不因piece或软行长度变大而扩大。
pub const MARKDOWN_LAYOUT_SOURCE_BYTES: usize = 4096;

/// 同一个payload只含原Snapshot与固定聚合；Clone只共享Arc，不另捕获或复制正文资源。
#[derive(Clone)]
pub struct MarkdownLayoutTextSource(Arc<Payload>);
struct Payload {
    // 按字段声明顺序析构：最后的标记只能在这个Snapshot已经交还后发布完成事实。
    snapshot: MarkdownTextSnapshot,
    generation: u64,
    bytes: usize,
    utf16: usize,
    domain: SourceDomain,
    dropped: PayloadDrop,
}
struct PayloadDrop(Arc<AtomicBool>);
impl Drop for PayloadDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// 只标识原不可变payload，不保活Snapshot、mmap、编辑段、活动文档或历史。
#[derive(Clone)]
pub struct MarkdownLayoutTextAuthority {
    source: Weak<Payload>,
}

impl PartialEq for MarkdownLayoutTextAuthority {
    fn eq(&self, other: &Self) -> bool {
        // Weak保留原控制块身份，销毁后也不把地址复用或相同正文误判为同一次capture。
        Weak::ptr_eq(&self.source, &other.source)
    }
}
impl Eq for MarkdownLayoutTextAuthority {}
impl PartialOrd for MarkdownLayoutTextAuthority {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for MarkdownLayoutTextAuthority {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 这只是不同原payload键的稳定总序，不表示正文顺序、代次更新或仍有活动权限。
        // Weak保留控制块期间地址不能重用，故与eq/hash保持一致；同源文本位置另按UTF16比较。
        self.source.as_ptr().cmp(&other.source.as_ptr())
    }
}
impl Hash for MarkdownLayoutTextAuthority {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // 仅用于Native位置等有限键；hash不暴露正文，且相等值始终使用同一原payload地址。
        self.source.as_ptr().hash(state);
    }
}

/// 只保留固定析构标记；它不参与源保活，也不把Weak失效提前冒充字段实际析构。
#[derive(Clone)]
pub struct MarkdownLayoutTextSourceObserver {
    dropped: Arc<AtomicBool>,
}
impl MarkdownLayoutTextSourceObserver {
    /// 仅证明本payload已经交还Snapshot，不声称其他Snapshot或Native值也已释放共享mmap。
    #[must_use]
    pub fn payload_released(&self) -> bool {
        self.dropped.load(Ordering::Acquire)
    }
}

impl MarkdownLayoutTextSource {
    /// 在同一次不可变文档借用中捕获generation与原Snapshot；没有可重入调用或全文扫描。
    ///
    /// 后续编辑、Undo/Redo和文档销毁不会改变这个值。generation只描述捕获事实，
    /// 不能取代Native消费时原owner/当前代次的权限检查。
    #[must_use]
    pub fn capture(document: &MarkdownDocumentBuffer) -> Self {
        Self(Arc::new(Payload {
            snapshot: document.text_snapshot(),
            generation: document.generation(),
            bytes: document.byte_len(),
            utf16: document.utf16_len(),
            domain: SourceDomain::Canonical,
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        }))
    }
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.0.generation
    }
    /// 只交付真实构造路径的域类别；不能凭没有 draft 票就把只读局部源当成完整正文。
    #[must_use]
    pub fn domain(&self) -> MarkdownLayoutTextSourceDomain {
        match &self.0.domain {
            SourceDomain::Canonical => MarkdownLayoutTextSourceDomain::Canonical,
            #[cfg(feature = "s2-table-window-prototype")]
            SourceDomain::ReadOnlyCell(_) => MarkdownLayoutTextSourceDomain::ReadOnlyCell,
            #[cfg(feature = "s2-table-window-prototype")]
            SourceDomain::ActiveCellDraft(_) => MarkdownLayoutTextSourceDomain::ActiveCellDraft,
        }
    }
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.bytes
    }
    #[must_use]
    pub fn utf16_len(&self) -> usize {
        self.0.utf16
    }
    /// 空文档也按真实payload allocation隔离；同文本/代次不能冒充同一源。
    #[must_use]
    pub fn authority(&self) -> MarkdownLayoutTextAuthority {
        MarkdownLayoutTextAuthority {
            source: Arc::downgrade(&self.0),
        }
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLayoutTextSourceObserver {
        MarkdownLayoutTextSourceObserver {
            dropped: Arc::clone(&self.0.dropped.0),
        }
    }
    /// 同payload端点转换；即使首尾或缓存命中也验证真实源健康，不另capture或读取整段。
    ///
    /// # Errors
    /// 原映射失败、位置越界或切入代理项时保留原错误；只定位原树的有限叶，不返回正文借用。
    #[cfg(any(feature = "paged-shaping", test))]
    pub fn byte_offset_for_utf16(
        &self,
        position: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.0.snapshot.byte_offset_for_utf16(position)
    }
    /// 原冻结payload的精确字符域谓词；不另捕获文档或把整item读入临时缓冲。
    /// 原owner仍须在调用前后核对同一authority，本方法与端点转换复用原错误类型。
    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn is_printable_ascii_range(
        &self,
        range: Range<usize>,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        self.0.snapshot.is_printable_ascii_range(range)
    }

    /// 原Script后缀证明只借本payload的Frozen，不另捕获Source或读取端点。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_printable_ascii_suffix(
        &self,
        range: Range<usize>,
    ) -> Result<
        crate::markdown_document_buffer::FrozenPrintableAsciiSuffix,
        MarkdownDocumentBufferError,
    > {
        self.0.snapshot.begin_printable_ascii_suffix(range)
    }

    /// 字节、标量和目录分别按当前原量子收费；真实来源失败保持原错误。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_printable_ascii_suffix(
        &self,
        cursor: &mut crate::markdown_document_buffer::FrozenPrintableAsciiSuffix,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<
        crate::markdown_document_buffer::FrozenPrintableAsciiSuffixStep,
        MarkdownDocumentBufferError,
    > {
        self.0
            .snapshot
            .advance_printable_ascii_suffix(cursor, bytes, scalars, metadata)
    }

    /// 原Dense GCB消费者使用的同源树游标；不创建第二Source或Reader，也不提供裸正文。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_ascii_word_run(
        &self,
        position: usize,
        forward: bool,
    ) -> Result<crate::markdown_document_buffer::MarkdownAsciiWordRun, MarkdownDocumentBufferError>
    {
        self.0
            .snapshot
            .begin_ascii_word_run(position, forward, self.authority())
    }

    /// 游标不能借相同文本或共享root跨Source复用；源失败先于身份拒绝，访问后再保最后I/O首因。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_ascii_word_run(
        &self,
        run: &mut crate::markdown_document_buffer::MarkdownAsciiWordRun,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<
        crate::markdown_document_buffer::MarkdownAsciiWordStep,
        crate::MarkdownLineComposeError,
    > {
        self.health()?;
        if !self.permits(run.authority()) {
            return Err(crate::MarkdownLineComposeError::SourceMismatch);
        }
        let result = self
            .0
            .snapshot
            .advance_ascii_word_run(run, bytes, scalars, metadata);
        self.health()?;
        result.map_err(Into::into)
    }
    /// 同一冻结全源的三态精确提示；脚本消费者仍须在纯 Core 交接前后核原 authority。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn printable_ascii_has_letters(
        &self,
    ) -> Result<Option<bool>, MarkdownDocumentBufferError> {
        self.0.snapshot.printable_ascii_has_letters()
    }

    /// 读取同一冻结payload的真实永久I/O状态，不能以Weak身份或旧索引命中替代此门。
    pub(crate) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.0.snapshot.original_backing_health()
    }
    /// 取消检查只持原子事实，不为观察原始I/O失败而捕获冻结正文的强owner。
    #[must_use]
    pub fn original_backing_observer(&self) -> Option<crate::MarkdownOriginalBackingObserver> {
        self.0.snapshot.original_backing_observer()
    }

    /// 将同一Body/Cell来源的UTF-16范围按原UTF-8片段写出，不复制整段或整格正文。
    ///
    /// # Errors
    /// 真实源健康、范围次序和两个标量端点均在首次writer调用前核验；空范围也不绕过健康门。
    /// 写入失败可留下部分目标，但不改变来源或其输入状态；原lease回收及Cell内容撤权
    /// 仍沿同一Snapshot门传播，实际源错误优先于writer失败。
    pub fn write_utf16_range_to(
        &self,
        range: Range<usize>,
        destination: &mut impl Write,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.health()?;
        if range.start > range.end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        } // 不夹取或倒置用户选择，两个位置的界限和标量边界继续由原Snapshot转换核验。
        let start = self.0.snapshot.byte_offset_for_utf16(range.start)?;
        let end = self.0.snapshot.byte_offset_for_utf16(range.end)?;
        // Snapshot visitor只借相交的原piece，每次回调最多4KiB；大选择不扩大单次源借用。
        // 不经布局的4KiB单次查询入口，也不为显式大复制构造选区String或反复读取已写正文。
        let result = self.0.snapshot.visit_byte_range(start..end, |bytes| {
            destination
                .write_all(bytes)
                .map_err(|_| MarkdownDocumentBufferError::WriteUnavailable)
        });
        self.health()?; // 最后一个编辑片的writer也可触发撤权或Source首错，不能因无后继叶而漏核。
        result
    }

    /// UBA与其他分页阶段共享原冻结值；只增加原Arc引用，不重捕文档或复制正文。
    #[cfg(feature = "paged-layout-breaks")]
    pub(crate) fn snapshot_for_bidi(&self) -> MarkdownTextSnapshot {
        self.0.snapshot.clone()
    }

    pub(crate) fn permits(&self, authority: &MarkdownLayoutTextAuthority) -> bool {
        Weak::ptr_eq(&authority.source, &Arc::downgrade(&self.0))
    }

    /// 交付原piece的有限借用，不复制正文；UTF8内部端点由各算法的同一状态机续接。
    pub(crate) fn visit_byte_range(
        &self,
        range: Range<usize>,
        visit: impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.health()?;
        if range.start > range.end || range.end > self.byte_len() {
            // 倒置/越界是源坐标错误，不把它伪装成可以重试更大配额的问题。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if range.len() > MARKDOWN_LAYOUT_SOURCE_BYTES {
            // 预算不能作为建议值；非法大请求在借用任何正文之前拒绝。
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        self.0.snapshot.visit_byte_range(range, visit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_body_range_keeps_original_and_edit_pieces_after_document_release() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("跨片来源.md");
        let original = format!("{}\r\n中😀e\u{301}{}尾", "a".repeat(4095), "b".repeat(4193));
        std::fs::write(&path, &original).unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 65536).unwrap();
        document
            .replace_utf16_range(
                4097..4098,
                "编辑😀\r\n",
                crate::MarkdownTextSelection::caret(4103),
            )
            .unwrap();
        let expected = original.replacen('中', "编辑😀\r\n", 1);
        let units = expected.encode_utf16().collect::<Vec<_>>();
        let range = 4091..8198;
        let expected = String::from_utf16(&units[range.clone()]).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let observer = source.original_backing_observer().unwrap();
        let generation = document.generation();
        let selection = document.selection();
        let mut bytes = Vec::new();
        source
            .write_utf16_range_to(range.clone(), &mut bytes)
            .unwrap();
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert!(document.is_dirty() && document.can_undo() && !document.can_redo());
        drop(document); // 冻结Body来源只保原字节，不保活动Doc及其历史。
        directory.close().unwrap();
        bytes.clear();
        source.write_utf16_range_to(range, &mut bytes).unwrap();
        assert_eq!(bytes, expected.as_bytes());
        drop(source);
        assert!(observer.unmap_succeeded() && observer.mapped_bytes() == 0 && !observer.failed());
    }

    #[test]
    fn streamed_source_rejects_every_invalid_scalar_endpoint_before_writer_io() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "A😀\r\n中").unwrap();
        let document = MarkdownDocumentBuffer::open(file.path(), 4096).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        for (range, expected) in [
            (2..3, MarkdownDocumentBufferError::NotUnicodeBoundary),
            (0..2, MarkdownDocumentBufferError::NotUnicodeBoundary),
            (2..2, MarkdownDocumentBufferError::NotUnicodeBoundary),
            (
                Range { start: 4, end: 1 },
                MarkdownDocumentBufferError::RangeOutsideDocument,
            ),
            (0..7, MarkdownDocumentBufferError::RangeOutsideDocument),
        ] {
            let mut bytes = b"existing".to_vec();
            assert_eq!(
                source.write_utf16_range_to(range, &mut bytes),
                Err(expected)
            );
            assert_eq!(bytes, b"existing"); // 部分输出不能在非法端点被发现前触及真实writer。
        }
    }

    #[test]
    fn source_readonly_fd_failure_preserves_body_selection_and_both_history_directions() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "原文😀\r\n尾").unwrap();
        let mut document = MarkdownDocumentBuffer::open(file.path(), 4096).unwrap();
        document
            .replace_utf16_range(0..2, "修改", crate::MarkdownTextSelection::caret(2))
            .unwrap();
        document
            .replace_utf16_range(0..2, "再改", crate::MarkdownTextSelection::caret(2))
            .unwrap();
        document.undo_text_edit().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let generation = document.generation();
        let selection = document.selection();
        let revision = document.current_revision().unwrap();
        let mut readonly = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(
            source.write_utf16_range_to(0..source.utf16_len(), &mut readonly),
            Err(MarkdownDocumentBufferError::WriteUnavailable)
        );
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert_eq!(document.current_revision().unwrap(), revision);
        assert!(document.is_dirty() && document.can_undo() && document.can_redo());
        let mut bytes = Vec::new();
        source
            .write_utf16_range_to(0..source.utf16_len(), &mut bytes)
            .unwrap();
        assert_eq!(bytes, "修改😀\r\n尾".as_bytes());
    }

    #[test]
    fn final_edit_piece_writer_failure_cannot_hide_actual_source_reclaim_eio() {
        struct ReclaimingWriter<'a> {
            document: &'a MarkdownDocumentBuffer,
            readonly: std::fs::File,
        }
        impl Write for ReclaimingWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                // 写入目标仍是真实只读FD；先使已读原页的真实回收操作发生EIO。
                self.document.fail_next_original_reclaim_for_test();
                assert_eq!(
                    self.document.reclaim_original_read_windows_for_test(),
                    Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                        libc::EIO
                    ))
                );
                self.readonly.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.readonly.flush()
            }
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"x".repeat(128 * 1024)).unwrap();
        let mut document = MarkdownDocumentBuffer::open(file.path(), 256 * 1024).unwrap();
        document
            .insert_utf16(0, "编辑", crate::MarkdownTextSelection::caret(2))
            .unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        source
            .visit_byte_range(64 * 1024..64 * 1024 + 1, |_| Ok(()))
            .unwrap();
        let observer = source.original_backing_observer().unwrap();
        let generation = document.generation();
        let selection = document.selection();
        {
            let mut writer = ReclaimingWriter {
                document: &document,
                readonly: std::fs::File::open("/dev/null").unwrap(),
            };
            // 选择仅含编辑片；最后叶没有Original lease，仍须在返回writer失败前核原Source健康。
            assert_eq!(
                source.write_utf16_range_to(0..2, &mut writer),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
        }
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert!(document.is_dirty() && document.can_undo() && !document.can_redo());
        drop((source, document));
        assert!(observer.unmap_succeeded() && observer.mapped_bytes() == 0);
        assert_eq!(observer.first_errno(), Some(libc::EIO));
    }

    #[test]
    fn printable_domain_uses_same_frozen_payload_and_excludes_ascii_controls() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source-domain.md");
        std::fs::write(&path, b"printable ~").unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        assert_eq!(
            source.is_printable_ascii_range(0..source.utf16_len()),
            Ok(true)
        );
        document
            .insert_utf16(2, "\t", crate::MarkdownTextSelection::caret(3))
            .unwrap();
        let edited = MarkdownLayoutTextSource::capture(&document);
        // byte与UTF16等长仍可能含C0；独立capture不能替换原payload的精确字符域事实。
        assert_eq!(edited.byte_len(), edited.utf16_len());
        assert_eq!(
            edited.is_printable_ascii_range(0..edited.utf16_len()),
            Ok(false)
        );
        assert!(source.permits(&authority) && !edited.permits(&authority));
        assert_eq!(
            source.is_printable_ascii_range(0..source.utf16_len()),
            Ok(true)
        );
    }

    #[test]
    fn bounded_visit_rejects_invalid_requests_before_exposing_any_piece() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("finite-source.md");
        std::fs::write(&path, b"x".repeat(8192)).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 16384).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let mut calls = 0;
        assert_eq!(
            source.visit_byte_range(0..4097, |_| {
                calls += 1;
                Ok(())
            }),
            Err(MarkdownDocumentBufferError::MaterializationLimitExceeded)
        );
        assert_eq!(
            source.visit_byte_range(Range { start: 1, end: 0 }, |_| {
                calls += 1;
                Ok(())
            }),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        assert_eq!(
            source.visit_byte_range(8192..8193, |_| {
                calls += 1;
                Ok(())
            }),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        source
            .visit_byte_range(8192..8192, |_| {
                calls += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(calls, 0);
        assert_eq!(source.byte_offset_for_utf16(0), Ok(0));
        assert_eq!(source.byte_offset_for_utf16(8192), Ok(8192));
        assert_eq!(
            source.byte_offset_for_utf16(8193),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        let mut bytes = 0;
        source
            .visit_byte_range(4095..8191, |piece| {
                assert!(piece.len() <= 4096);
                bytes += piece.len();
                Ok(())
            })
            .unwrap();
        assert_eq!(bytes, 4096);
        let clone = source.clone();
        let other = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        assert!(authority == clone.authority());
        assert!(authority != other.authority());
        let mut identities = std::collections::HashSet::new();
        identities.insert(authority.clone());
        identities.insert(clone.authority());
        identities.insert(other.authority());
        assert_eq!(identities.len(), 2); // hash与实际Weak身份一致，相同文本的独立capture仍各有键。
        let ordered: std::collections::BTreeSet<_> = identities.into_iter().collect();
        assert_eq!(ordered.len(), 2); // Native位置的跨源总序不能把不同源折叠为相同位置。
        assert!(clone.permits(&source.authority()));
        assert!(!other.permits(&source.authority()));
    }
}
