//! 显式复制的独立只读输出；项目只保留固定写入scratch，不创建全文字符串镜像。

use super::*;
use objc2_app_kit::{NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::NSData;
#[cfg(feature = "native-paged-body-probe")]
use omni_markdown_core::MarkdownLayoutTextSource;
use std::io::BufWriter;
use std::os::fd::AsRawFd;
use std::sync::Mutex;

const COPY_SCRATCH_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
/// 只区分真实源读取、输出存储与系统发布阶段，不将已清空的剪贴板说成保持原样。
pub(in crate::native_surface) enum MarkdownPasteboardWriteError {
    #[error("无法读取当前 Markdown 正文：{0}")]
    DocumentReadUnavailable(#[source] MarkdownDocumentBufferError),
    #[error("无法准备复制文本的临时存储：{0}")]
    OutputUnavailable(#[from] io::Error),
    #[error("无法准备系统剪贴板数据，此次复制未写入剪贴板")]
    DataPreparationRejected,
    #[error("系统剪贴板写入未成功，原剪贴板内容可能已被清空")]
    PasteboardWriteRejected,
}

/// 原子取消命名后的inode只由这份只读映射保活，不留路径、FD或canonical引用。
struct ReadOnlyOutputMapping {
    bytes: *mut c_void,
    length: usize,
}

// SAFETY: 映射只读且没有Rust借用或可写文件句柄；任意线程只可消费所有权并munmap。
unsafe impl Send for ReadOnlyOutputMapping {}

impl Drop for ReadOnlyOutputMapping {
    /// Native回调和交付前失败都归还同一OS映射；不调用AppKit或访问已关闭的插件。
    fn drop(&mut self) {
        // SAFETY: 本owner只来自成功mmap，保存完整原地址/长度且不Clone；最终释放只执行一次。
        let result = unsafe { libc::munmap(self.bytes, self.length) };
        let error = (result != 0).then(io::Error::last_os_error);
        #[cfg(feature = "native-markdown-runtime-probe")]
        {
            // 只观察此owner刚发生的真实OS操作，不以Drop次数或窗口关闭代替munmap成功。
            let _ = writeln!(
                io::stderr().lock(),
                "native_copy_output_release mapping={:p} bytes={} actual_munmap_result={result} os_error={:?}",
                self.bytes,
                self.length,
                error.as_ref().and_then(io::Error::raw_os_error)
            );
        }
        if let Some(error) = error {
            // 复制可能早已完成，后台释放错误不能改写旧UI结果；保留实际OS失败而不伪称已回收。
            let _ = writeln!(io::stderr().lock(), "复制输出映射释放失败：{error}");
        }
    }
}

impl ReadOnlyOutputMapping {
    /// block只持本次输出owner，Fn回调可在线程间释放；不借原Doc、原文件或后继会话。
    fn into_data(self) -> Result<Retained<NSData>, MarkdownPasteboardWriteError> {
        let bytes = NonNull::new(self.bytes)
            .ok_or(MarkdownPasteboardWriteError::DataPreparationRejected)?;
        let length = self.length;
        let owner = Mutex::new(Some(self));
        let deallocator = block2::RcBlock::new(move |_bytes: NonNull<c_void>, _length: usize| {
            // 先移出owner并结束锁，再执行实际munmap；局部block释放也能清理未交付的owner。
            let mapping = owner.lock().expect("剪贴板输出所有权锁不能中毒").take();
            drop(mapping);
        });
        // SAFETY: bytes/length属于捕获的同一完整只读映射；NSData复制block并承担其后续寿命。
        // block仅捕获线程安全的输出owner，不持NSData、AppKit对象、Doc或FD，不形成回指环。
        Ok(unsafe {
            NSData::initWithBytesNoCopy_length_deallocator(
                NSData::alloc(),
                bytes,
                length,
                Some(&deallocator),
            )
        })
    }
}

/// 一次用户复制的完整输出；空值无需映射，非空值不依赖插件关闭后的任何业务资源。
pub(in crate::native_surface) struct MarkdownPasteboardOutput {
    mapping: Option<ReadOnlyOutputMapping>,
}

impl MarkdownPasteboardOutput {
    /// 只调用原canonical流和文件操作；调用方可在返回后归还Doc借用，再开始Native发布。
    pub(in crate::native_surface) fn prepare(
        document: &MarkdownDocumentBuffer,
    ) -> Result<Self, MarkdownPasteboardWriteError> {
        if document.byte_len() == 0 {
            // 即使空树也保留原source_health失败语义，不用空字符串掩盖已失效的源。
            document
                .write_to(&mut io::sink())
                .map_err(MarkdownPasteboardWriteError::DocumentReadUnavailable)?;
            return Ok(Self { mapping: None });
        }
        Self::prepare_output(|writer| {
            document
                .write_to(writer)
                .map_err(MarkdownPasteboardWriteError::DocumentReadUnavailable)
        })
    }

    /// 复制原Input捕获的Body/Cell来源；所有范围先核验，再逐有限piece写出同一个匿名输出。
    #[cfg(feature = "native-paged-body-probe")]
    pub(in crate::native_surface) fn prepare_source_ranges(
        source: &MarkdownLayoutTextSource,
        ranges: &[std::ops::Range<usize>],
    ) -> Result<Self, MarkdownPasteboardWriteError> {
        // 原源首错优先于范围和输出FD错误；空选择也必须经过同一个真实健康门。
        source
            .write_utf16_range_to(0..0, &mut io::sink())
            .map_err(MarkdownPasteboardWriteError::DocumentReadUnavailable)?;
        let mut previous_end = None;
        for range in ranges {
            if range.start > range.end || previous_end.is_some_and(|end| end > range.start) {
                return Err(MarkdownPasteboardWriteError::DocumentReadUnavailable(
                    MarkdownDocumentBufferError::RangeOutsideDocument,
                ));
            } // 原有序选择不能倒置、重叠或重排；caret也保留其真实局部位置顺序。
            source
                .byte_offset_for_utf16(range.start)
                .and_then(|_| source.byte_offset_for_utf16(range.end))
                .map_err(MarkdownPasteboardWriteError::DocumentReadUnavailable)?;
            previous_end = Some(range.end);
        }
        if ranges.iter().all(std::ops::Range::is_empty) {
            return Ok(Self { mapping: None }); // 无选中文字无需文件；非法caret已在上面拒绝。
        }
        let output = Self::prepare_output(|writer| {
            let mut previous_end = None;
            for range in ranges {
                if range.is_empty() {
                    continue; // 空caret不改变非空选择的拼接位置，也不制造额外空行。
                }
                if previous_end.is_some_and(|end| end < range.start) {
                    writer.write_all(b"\n")?;
                } // 保留既有语义：间隔选择用换行，相邻选择直接拼接原始字节。
                source
                    .write_utf16_range_to(range.clone(), writer)
                    .map_err(MarkdownPasteboardWriteError::DocumentReadUnavailable)?;
                previous_end = Some(range.end);
            }
            Ok(())
        });
        // flush/sync/mmap失败也不能遮住同一Source刚发生的真实首错或Cell内容撤权。
        source
            .write_utf16_range_to(0..0, &mut io::sink())
            .map_err(MarkdownPasteboardWriteError::DocumentReadUnavailable)?;
        output
    }

    /// 全文和选区共享同一个匿名输出生命周期；闭包只写Rust文件，不调用Native或借出Doc。
    fn prepare_output(
        write: impl FnOnce(
            &mut BufWriter<&mut std::fs::File>,
        ) -> Result<(), MarkdownPasteboardWriteError>,
    ) -> Result<Self, MarkdownPasteboardWriteError> {
        let (mut file, path) = tempfile::NamedTempFile::new()?.into_parts();
        // 显式成功unlink是隐私和资源前提；失败仍由原TempPath/FD的RAII完成清理，不触及剪贴板。
        std::fs::remove_file(&path)?;
        drop(path);
        {
            // 原piece仅借有限源片；64KiB scratch聚合小写入，不重新编码、正规化换行或构造全文值。
            let mut writer = BufWriter::with_capacity(COPY_SCRATCH_BYTES, &mut file);
            write(&mut writer)?;
            writer.flush()?;
        }
        // 真实输出写入错误必须在系统发布前发现；匿名输出无需目录持久化或新增保存协议。
        file.sync_data()?;
        let length = usize::try_from(file.metadata()?.len())
            .map_err(|_| io::Error::new(io::ErrorKind::FileTooLarge, "复制输出超出本机地址范围"))?;
        // SAFETY: 唯一FD属于刚刚成功unlink的输出inode；全部字节已写完，不再有可写别名。
        // length来自同一输出FD的实际已写字节数，映射只读且不暴露为可变Rust切片。
        let bytes = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if bytes == libc::MAP_FAILED {
            return Err(io::Error::last_os_error().into()); // FD随失败退出关闭，尚无映射或Native输出。
        }
        let mapping = ReadOnlyOutputMapping { bytes, length };
        drop(file); // 仅映射保匿名inode；任何NSData或剪贴板寿命都不再持FD。
        Ok(Self {
            mapping: Some(mapping),
        })
    }

    /// 只供显式复制动作发布；成功值由系统保留，插件返回不清空general pasteboard。
    pub(in crate::native_surface) fn publish(
        self,
        pasteboard: &NSPasteboard,
    ) -> Result<(), MarkdownPasteboardWriteError> {
        let objects = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            let data = match self.mapping {
                Some(mapping) => mapping.into_data()?, // 只转移本次独立输出，绝不复制全文UTF16。
                None => NSData::data(), // 空文档按原纯文本类型发布空值，不建立len0 mmap。
            };
            let item = NSPasteboardItem::new();
            // SAFETY: 继续使用原AppKit导出的标准纯文本UTI，不更改用户正文或加入文件URL类型。
            let string_type = unsafe { NSPasteboardTypeString };
            if !item.setData_forType(&data, string_type) {
                return Err(MarkdownPasteboardWriteError::DataPreparationRejected);
            }
            // 完整item先在局部准备成功；数组/数据失败都发生在清空系统剪贴板之前。
            Ok(NSArray::from_slice(&[ProtocolObject::<
                dyn NSPasteboardWriting,
            >::from_ref(&*item)]))
        }))
        .map_err(|_| MarkdownPasteboardWriteError::DataPreparationRejected)??;
        let written = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            // 这是用户明确授权的发布点；此后失败不能承诺旧剪贴内容仍完整存在。
            pasteboard.clearContents();
            pasteboard.writeObjects(&objects)
        }))
        .map_err(|_| MarkdownPasteboardWriteError::PasteboardWriteRejected)?;
        if written {
            Ok(()) // 只在系统确认写入后报告成功；输出后续寿命归Native，不进入插件空闲缓存。
        } else {
            Err(MarkdownPasteboardWriteError::PasteboardWriteRejected)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 未保存的Unicode和CRLF必须按canonical原字节复制，准备不改变正文、历史或保存状态。
    fn prepared_copy_preserves_unsaved_unicode_and_outlives_the_original_document() {
        let directory = tempfile::tempdir().expect("复制输入目录");
        let path = directory.path().join("正文.md");
        std::fs::write(&path, "Hello\r\n中文 😀 e\u{301}\r\n").expect("写入复制输入");
        let mut document =
            MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("打开真实canonical");
        document
            .replace_utf16_range(0..5, "Edited", MarkdownTextSelection::caret(6))
            .expect("提交原canonical未保存编辑");
        let generation = document.generation();
        let revision = document.current_revision().expect("原revision");
        let selection = document.selection();
        let output = MarkdownPasteboardOutput::prepare(&document).expect("准备独立复制输出");
        assert_eq!(document.generation(), generation);
        assert_eq!(document.current_revision().unwrap(), revision);
        assert_eq!(document.selection(), selection);
        assert!(document.is_dirty() && document.can_undo() && !document.can_redo());
        drop(document);
        directory.close().expect("删除原文件与目录");
        let mapping = output.mapping.as_ref().expect("非空复制必须具有真实映射");
        // SAFETY: output仍唯一拥有完整只读映射；原Doc和路径已释放，无源引用参与此读取。
        let bytes =
            unsafe { std::slice::from_raw_parts(mapping.bytes.cast::<u8>(), mapping.length) };
        assert_eq!(bytes, "Edited\r\n中文 😀 e\u{301}\r\n".as_bytes());
    }

    #[test]
    /// 跨多个原4KiB片和64KiB写scratch的内容保持精确字节，不通过全文String准备测试数据。
    fn prepared_copy_streams_across_original_pieces_and_fixed_write_scratch() {
        const PATTERN: &[u8] = "English\r\n中文 😀 e\u{301}\r\n".as_bytes();
        let directory = tempfile::tempdir().expect("复制输入目录");
        let path = directory.path().join("跨片.md");
        {
            let mut file = std::fs::File::create(&path).expect("创建跨片输入");
            for _ in 0..10_000 {
                // 固定小片生成真实文件，测试同样不保留另一个全文堆镜像。
                file.write_all(PATTERN).expect("写入跨片输入");
            }
        }
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        let output = MarkdownPasteboardOutput::prepare(&document).unwrap();
        drop(document);
        directory.close().unwrap();
        let mapping = output.mapping.as_ref().unwrap();
        assert_eq!(mapping.length, PATTERN.len() * 10_000);
        // SAFETY: 输出映射完整初始化且当前owner仍活；有限比较切片只借输出字节，没有创建全文副本。
        let bytes =
            unsafe { std::slice::from_raw_parts(mapping.bytes.cast::<u8>(), mapping.length) };
        for chunk in bytes.as_chunks::<{ PATTERN.len() }>().0 {
            assert_eq!(chunk, PATTERN);
        }
    }

    #[test]
    /// 空canonical仍经过原读取合同，但不能为了发布空文本分配文件或无效零长度映射。
    fn empty_copy_has_no_mapping_and_preserves_the_empty_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("空.md");
        std::fs::write(&path, []).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        let output = MarkdownPasteboardOutput::prepare(&document).unwrap();
        assert!(output.mapping.is_none());
        assert_eq!(document.byte_len(), 0);
        assert!(!document.is_dirty() && !document.can_undo() && !document.can_redo());
    }

    #[test]
    #[cfg(feature = "native-paged-body-probe")]
    /// 多选保留各段精确字节并跳过未选正文；原Doc释放后仍可独立交付输出。
    fn prepared_ranges_preserve_unicode_selection_and_release_the_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("多选.md");
        std::fs::write(&path, "A中😀B\r\nC文D").unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        document
            .replace_utf16_range(7..8, "改", MarkdownTextSelection::caret(8))
            .unwrap();
        let revision = document.current_revision().unwrap();
        let generation = document.generation();
        let selection = document.selection();
        let source = MarkdownLayoutTextSource::capture(&document);
        let output = MarkdownPasteboardOutput::prepare_source_ranges(&source, &[1..4, 8..9])
            .expect("有间隔多选必须只复制原选择");
        assert_eq!(document.current_revision().unwrap(), revision);
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert!(document.is_dirty() && document.can_undo() && !document.can_redo());
        drop((source, document)); // 原冻结读取者也先归还，输出不能借测试剩余Source保活正文。
        directory.close().unwrap();
        let mapping = output.mapping.as_ref().unwrap();
        // SAFETY: 本输出仍独占完整只读映射；原Doc、路径和编辑段都已释放。
        let bytes =
            unsafe { std::slice::from_raw_parts(mapping.bytes.cast::<u8>(), mapping.length) };
        assert_eq!(bytes, "中😀\n文".as_bytes());
    }

    #[test]
    #[cfg(feature = "native-paged-body-probe")]
    /// 相邻范围采用标准原生连接，空caret既不插空行也不产生输出映射。
    fn prepared_ranges_join_adjacent_selections_without_extra_newlines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("相邻.md");
        std::fs::write(&path, "A中😀B").unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let output =
            MarkdownPasteboardOutput::prepare_source_ranges(&source, &[0..1, 1..1, 1..2]).unwrap();
        let mapping = output.mapping.as_ref().unwrap();
        // SAFETY: 与上例同一真实输出owner仍在本栈；比较不复制原文稿。
        let bytes =
            unsafe { std::slice::from_raw_parts(mapping.bytes.cast::<u8>(), mapping.length) };
        assert_eq!(bytes, "A中".as_bytes());
        let empty =
            MarkdownPasteboardOutput::prepare_source_ranges(&source, &[0..0, 5..5]).unwrap();
        assert!(empty.mapping.is_none());
        assert!(!document.is_dirty() && !document.can_undo() && !document.can_redo());
    }

    #[test]
    #[cfg(feature = "native-paged-body-probe")]
    /// 非BMP字符内部端点不能写入或发布半个字符；错误仍携带Core真实范围失败。
    fn prepared_ranges_reject_partial_surrogate_without_changing_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("端点.md");
        std::fs::write(&path, "A😀Z").unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        let revision = document.current_revision().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        assert!(matches!(
            MarkdownPasteboardOutput::prepare_source_ranges(&source, std::slice::from_ref(&(1..2))),
            Err(MarkdownPasteboardWriteError::DocumentReadUnavailable(
                MarkdownDocumentBufferError::NotUnicodeBoundary
            ))
        ));
        assert_eq!(document.current_revision().unwrap(), revision);
        assert!(!document.is_dirty() && !document.can_undo() && !document.can_redo());
    }

    #[test]
    #[cfg(feature = "native-paged-body-probe")]
    /// 原Source输出保持间隔/相邻/空caret语义；释放真实reader和Doc后只剩独立输出映射。
    fn prepared_source_ranges_outlive_the_source_without_retaining_the_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("来源多选.md");
        std::fs::write(&path, "A中😀B\r\nC文D").unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        document
            .replace_utf16_range(7..8, "改", MarkdownTextSelection::caret(8))
            .unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let reader = source.resource_observer();
        let backing = source.original_backing_observer().unwrap();
        let generation = document.generation();
        let selection = document.selection();
        let output =
            MarkdownPasteboardOutput::prepare_source_ranges(&source, &[1..4, 4..4, 4..5, 8..9])
                .unwrap();
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert!(document.is_dirty() && document.can_undo() && !document.can_redo());
        drop((source, document));
        directory.close().unwrap();
        assert!(reader.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
        let mapping = output.mapping.as_ref().unwrap();
        // SAFETY: 本栈仍持完整只读输出，原文稿及Source reader都已真实归还。
        let bytes =
            unsafe { std::slice::from_raw_parts(mapping.bytes.cast::<u8>(), mapping.length) };
        assert_eq!(bytes, "中😀B\n文".as_bytes());
    }

    #[test]
    #[cfg(feature = "native-paged-body-probe")]
    /// 后继范围非法也不能形成可发布候选；空caret同样核端点且不分配输出映射。
    fn source_output_preflights_all_ranges_and_scalar_endpoints() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "A😀Z").unwrap();
        let document = MarkdownDocumentBuffer::open(file.path(), 4096).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let split_caret = 2..2;
        for (ranges, expected) in [
            (
                vec![0..1, 1..2],
                MarkdownDocumentBufferError::NotUnicodeBoundary,
            ),
            (
                vec![split_caret],
                MarkdownDocumentBufferError::NotUnicodeBoundary,
            ),
            (
                vec![0..3, 1..3],
                MarkdownDocumentBufferError::RangeOutsideDocument,
            ),
            (
                vec![3..3, 0..1],
                MarkdownDocumentBufferError::RangeOutsideDocument,
            ),
            (
                vec![0..1, 4..5],
                MarkdownDocumentBufferError::RangeOutsideDocument,
            ),
        ] {
            assert!(matches!(
                MarkdownPasteboardOutput::prepare_source_ranges(&source, &ranges),
                Err(MarkdownPasteboardWriteError::DocumentReadUnavailable(actual)) if actual == expected
            ));
        }
        assert!(
            MarkdownPasteboardOutput::prepare_source_ranges(&source, &[0..0, 4..4])
                .unwrap()
                .mapping
                .is_none()
        );
        assert!(
            MarkdownPasteboardOutput::prepare_source_ranges(&source, &[])
                .unwrap()
                .mapping
                .is_none()
        );
        assert!(!document.is_dirty() && !document.can_undo() && !document.can_redo());
    }
}
