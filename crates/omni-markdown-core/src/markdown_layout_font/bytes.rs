//! 从已经核验的源FD捕获独立匿名inode；Font只借用稳定Blob，不映射可被用户截断的源。
use std::{
    fs::{Metadata, OpenOptions},
    io,
    os::{
        fd::IntoRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    sync::{Arc, atomic::Ordering},
};

use thiserror::Error;

use crate::immutable_snapshot::{SnapshotError, snapshot_font_file};

mod mapping;
use mapping::{MappingFacts, ReadonlyFontMapping, close_descriptor};

#[derive(Debug, Error)]
pub enum MarkdownLayoutFontBytesError {
    #[error("字体来源不是普通文件或末级路径是符号链接")]
    NotRegularFile,
    #[error("字体来源为空")]
    EmptyFile,
    #[error("字体来源超出允许大小或地址范围")]
    FileTooLarge,
    #[error("字体来源在捕获期间被替换或修改")]
    SourceChanged,
    #[error("字体匿名快照捕获未完成")]
    SnapshotUnavailable,
    #[error("无法访问字体来源或匿名快照")]
    Storage(#[source] io::Error),
}
type Result<T> = std::result::Result<T, MarkdownLayoutFontBytesError>;

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}
impl From<&Metadata> for FileIdentity {
    fn from(value: &Metadata) -> Self {
        Self {
            device: value.dev(),
            inode: value.ino(),
            bytes: value.len(),
            modified_seconds: value.mtime(),
            modified_nanoseconds: value.mtime_nsec(),
            changed_seconds: value.ctime(),
            changed_nanoseconds: value.ctime_nsec(),
        }
    }
}
fn regular(value: &Metadata) -> bool {
    value.is_file() && !value.file_type().is_symlink()
}

/// 所有Clone和Blob wrapper共享同一映射；不保源路径、源FD或可写inode句柄。
#[derive(Clone)]
pub struct MarkdownLayoutFontBytes(Arc<Payload>);
struct Payload {
    // VM由唯一owner实际munmap；observer仅保固定原子，不参与Font或映射保活。
    mapping: ReadonlyFontMapping,
    // 只保留捕获时的文件标量身份；不同字号可复用同一冻结字节，不保源路径或源FD。
    source: FileIdentity,
}

/// Drop步骤、实际munmap和descriptor关闭分别观察；多字段不是运行中事务快照。
/// 最后owner结束后应同时核completed、unmap_succeeded、mapped_bytes为0且failed为false。
#[derive(Clone)]
pub struct MarkdownLayoutFontBytesResourceObserver {
    facts: Arc<MappingFacts>,
    captured_bytes: usize,
}
impl MarkdownLayoutFontBytesResourceObserver {
    /// 只比较同一实际映射的释放收据，不保活映射，也不以文件路径、长度或内容相同猜共享。
    /// 多个字号可拥有不同 Font payload 而共享该映射，释放责任须分别判断。
    #[must_use]
    pub fn same_mapping_resource(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.facts, &other.facts)
    }

    #[must_use]
    pub fn mapping_drop_completed(&self) -> bool {
        self.facts.completed.load(Ordering::Acquire)
    }
    #[must_use]
    pub fn captured_bytes(&self) -> usize {
        self.captured_bytes
    }
    #[must_use]
    pub fn unmap_succeeded(&self) -> bool {
        self.facts.unmapped.load(Ordering::Acquire)
    }
    #[must_use]
    pub fn mapped_bytes(&self) -> usize {
        self.facts.mapped_bytes.load(Ordering::Acquire)
    }
    #[must_use]
    pub fn backing_descriptor_closed(&self) -> bool {
        self.facts.backing_closed.load(Ordering::Acquire)
    }
    #[must_use]
    pub fn source_descriptor_closed(&self) -> bool {
        self.facts.source_closed.load(Ordering::Acquire)
    }
    #[must_use]
    pub fn failed(&self) -> bool {
        self.facts.first_errno().is_some()
    }
    #[must_use]
    pub fn first_errno(&self) -> Option<i32> {
        self.facts.first_errno()
    }
}

impl AsRef<[u8]> for MarkdownLayoutFontBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.mapping.as_ref()
    }
}

impl MarkdownLayoutFontBytes {
    /// 先冻结独立无名inode，再核原FD和路径；不执行UTF8、hash或整字体内容扫描。
    ///
    /// 元数据复核是与Document open相同的乐观并发保护，不能检测蓄意恢复全部时间戳
    /// 的外部写入；内存安全由独立匿名inode保证，而非把原文件视为不可变。
    ///
    /// # Errors
    /// 非普通文件、末级符号链接、空/过大来源、路径替换、源变动或实际快照IO失败均拒绝。
    pub fn capture_file(path: &Path, max_bytes: u64) -> Result<Self> {
        capture(path, max_bytes, || {})
    }

    /// 在调用方现有有限字体集合内共享同一未变来源的冻结字节；不建立进程缓存。
    /// 文件身份与首次捕获一致才可复用，face、轴位、字号仍由新Font各自解析和验证。
    /// 与首次捕获相同，这项乐观复核不能识别外部蓄意恢复全部元数据的写入。
    ///
    /// # Errors
    /// 来源不可访问、变成非普通文件或超出本次字节预算时拒绝；正常文件换代返回None。
    pub fn share_unchanged_file(&self, path: &Path, max_bytes: u64) -> Result<Option<Self>> {
        let metadata =
            std::fs::symlink_metadata(path).map_err(MarkdownLayoutFontBytesError::Storage)?;
        if !regular(&metadata) {
            return Err(MarkdownLayoutFontBytesError::NotRegularFile);
        }
        if metadata.len() > max_bytes {
            return Err(MarkdownLayoutFontBytesError::FileTooLarge);
        }
        // 新inode、长度、mtime或ctime任一变化都回到真实捕获，不能只凭路径复用旧字体。
        Ok((FileIdentity::from(&metadata) == self.0.source).then(|| self.clone()))
    }
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.mapping.len()
    }
    /// 小wrapper只共享原payload，Font的Blob strong边可自然延长同一映射使用期。
    #[must_use]
    pub fn blob(&self) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
        Arc::new(self.clone())
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLayoutFontBytesResourceObserver {
        MarkdownLayoutFontBytesResourceObserver {
            facts: self.0.mapping.facts(),
            captured_bytes: self.byte_len(),
        }
    }
}

fn capture(
    path: &Path,
    max_bytes: u64,
    after_snapshot: impl FnOnce(),
) -> Result<MarkdownLayoutFontBytes> {
    let path_before =
        std::fs::symlink_metadata(path).map_err(MarkdownLayoutFontBytesError::Storage)?;
    if !regular(&path_before) {
        return Err(MarkdownLayoutFontBytesError::NotRegularFile);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(MarkdownLayoutFontBytesError::Storage)?;
    let before = file
        .metadata()
        .map_err(MarkdownLayoutFontBytesError::Storage)?;
    if !regular(&before) {
        return Err(MarkdownLayoutFontBytesError::NotRegularFile); // 不在设备/FIFO等特殊FD上等待或映射。
    }
    let identity = FileIdentity::from(&before);
    if FileIdentity::from(&path_before) != identity {
        return Err(MarkdownLayoutFontBytesError::SourceChanged); // path检查与open之间换inode同样拒绝。
    }
    if before.len() == 0 {
        return Err(MarkdownLayoutFontBytesError::EmptyFile);
    }
    let bytes =
        usize::try_from(before.len()).map_err(|_| MarkdownLayoutFontBytesError::FileTooLarge)?;
    if before.len() > max_bytes || bytes > isize::MAX as usize {
        return Err(MarkdownLayoutFontBytesError::FileTooLarge); // 在捕获/借用任何字体正文前限制extent。
    }
    let frozen = snapshot_font_file(&file, before.len())
        .map_err(|error| match error {
            SnapshotError::SourceChanged => MarkdownLayoutFontBytesError::SourceChanged,
            SnapshotError::Cancelled => MarkdownLayoutFontBytesError::SnapshotUnavailable,
            SnapshotError::Storage(error) => MarkdownLayoutFontBytesError::Storage(error),
        })?
        .ok_or(MarkdownLayoutFontBytesError::SnapshotUnavailable)?;
    let mapping =
        ReadonlyFontMapping::map(frozen).map_err(MarkdownLayoutFontBytesError::Storage)?;
    after_snapshot(); // 产品为空操作；测试只在此真实边界替换路径，验证同一发布门。
    let validation = (|| {
        let after = file
            .metadata()
            .map_err(|_| MarkdownLayoutFontBytesError::SourceChanged)?;
        let path_after = std::fs::symlink_metadata(path)
            .map_err(|_| MarkdownLayoutFontBytesError::SourceChanged)?;
        if !regular(&after)
            || !regular(&path_after)
            || FileIdentity::from(&after) != identity
            || FileIdentity::from(&path_after) != identity
            || mapping.len() != bytes
        {
            return Err(MarkdownLayoutFontBytesError::SourceChanged);
        }
        Ok(())
    })();
    // 原用户文件FD只用于乐观身份核验，发布前真实close；Blob不保路径或源descriptor。
    if let Err(error) = close_descriptor(file.into_raw_fd()) {
        mapping.record_error(&error);
        // map.close保留同一首因，实际munmap失败也不能把资源数量假归零。
        mapping
            .close()
            .map_err(MarkdownLayoutFontBytesError::Storage)?;
        return Err(MarkdownLayoutFontBytesError::Storage(error));
    }
    mapping.source_closed();
    if let Err(error) = validation {
        mapping
            .close()
            .map_err(MarkdownLayoutFontBytesError::Storage)?;
        return Err(error); // 路径变动时先实际归还私有VM，再保留SourceChanged类别。
    }
    Ok(MarkdownLayoutFontBytes(Arc::new(Payload {
        mapping,
        source: identity,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_font_bytes_survive_original_truncate_and_last_blob_controls_drop_step() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("font.bin");
        std::fs::write(&path, [0, 0xff, 0x80, 7]).unwrap();
        let captured = MarkdownLayoutFontBytes::capture_file(&path, 4).unwrap();
        let observer = captured.resource_observer();
        let blob = captured.blob();
        std::fs::write(&path, []).unwrap();
        assert_eq!(captured.as_ref(), [0, 0xff, 0x80, 7]); // 无UTF8验证或原inode借用。
        assert_eq!(observer.captured_bytes(), 4);
        drop(captured);
        assert!(!observer.mapping_drop_completed());
        assert!(!observer.unmap_succeeded());
        assert_eq!(observer.mapped_bytes(), 4);
        assert!(observer.backing_descriptor_closed() && observer.source_descriptor_closed());
        assert_eq!(blob.as_ref().as_ref(), [0, 0xff, 0x80, 7]);
        drop(blob);
        assert!(observer.mapping_drop_completed());
        assert!(observer.unmap_succeeded());
        assert_eq!(observer.mapped_bytes(), 0);
        assert!(!observer.failed());
        assert!(observer.backing_descriptor_closed() && observer.source_descriptor_closed());
    }

    #[test]
    fn immutable_font_value_is_send_sync_and_last_background_blob_releases_actual_vm() {
        fn assert_traits<T: Send + Sync>() {}
        assert_traits::<MarkdownLayoutFontBytes>();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("font.bin");
        std::fs::write(&path, b"font").unwrap();
        let value = MarkdownLayoutFontBytes::capture_file(&path, 4).unwrap();
        let observer = value.resource_observer();
        let blob = value.blob();
        drop(value);
        std::thread::spawn(move || {
            assert_eq!(blob.as_ref().as_ref(), b"font");
            drop(blob); // 最后共享值在后台真实析构，observer不参与其VM保活。
        })
        .join()
        .unwrap();
        assert!(observer.mapping_drop_completed() && observer.unmap_succeeded());
        assert_eq!(observer.mapped_bytes(), 0);
        assert!(!observer.failed());
        assert!(observer.backing_descriptor_closed() && observer.source_descriptor_closed());
    }

    #[test]
    fn path_replacement_after_snapshot_cannot_publish_the_old_font() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("font.bin");
        let next = directory.path().join("next.bin");
        std::fs::write(&path, b"font").unwrap();
        std::fs::write(&next, b"next").unwrap();
        let result = capture(&path, 4, || std::fs::rename(&next, &path).unwrap());
        assert!(matches!(
            result,
            Err(MarkdownLayoutFontBytesError::SourceChanged)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"next");
    }

    #[test]
    fn unchanged_font_source_shares_one_mapping_until_its_last_instance_releases() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("font.bin");
        std::fs::write(&path, b"font").unwrap();
        let first = MarkdownLayoutFontBytes::capture_file(&path, 4).unwrap();
        let release = first.resource_observer();
        let second = first.share_unchanged_file(&path, 4).unwrap().unwrap();
        // 原观察者见到同一真实映射的生存期；不会因为另一个字号实例还活着而提前报归还。
        drop(first);
        assert!(!release.mapping_drop_completed());
        assert_eq!(second.as_ref(), b"font");
        assert_eq!(release.mapped_bytes(), 4);
        drop(second);
        assert!(release.mapping_drop_completed() && release.unmap_succeeded());
        assert_eq!(release.mapped_bytes(), 0);
        assert!(release.source_descriptor_closed() && release.backing_descriptor_closed());
        assert!(!release.failed());
    }

    #[test]
    fn replaced_font_source_is_recaptured_without_mutating_the_existing_instance() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("font.bin");
        let replacement = directory.path().join("replacement.bin");
        std::fs::write(&path, b"font").unwrap();
        let original = MarkdownLayoutFontBytes::capture_file(&path, 4).unwrap();
        // 不同inode即使字节数相同也必须重新捕获，避免字体文件更新时套用旧字形数据。
        std::fs::write(&replacement, b"next").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(original.share_unchanged_file(&path, 4).unwrap().is_none());
        let next = MarkdownLayoutFontBytes::capture_file(&path, 4).unwrap();
        assert_eq!(original.as_ref(), b"font");
        assert_eq!(next.as_ref(), b"next");
    }

    #[test]
    fn size_symlink_and_special_file_checks_happen_before_capture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("font.bin");
        std::fs::write(&path, b"font").unwrap();
        assert!(matches!(
            MarkdownLayoutFontBytes::capture_file(&path, 3),
            Err(MarkdownLayoutFontBytesError::FileTooLarge)
        ));
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(matches!(
            MarkdownLayoutFontBytes::capture_file(&link, 4),
            Err(MarkdownLayoutFontBytesError::NotRegularFile)
        ));
        assert!(matches!(
            MarkdownLayoutFontBytes::capture_file(directory.path(), 4),
            Err(MarkdownLayoutFontBytesError::NotRegularFile)
        ));
        std::fs::write(&path, []).unwrap();
        assert!(matches!(
            MarkdownLayoutFontBytes::capture_file(&path, 4),
            Err(MarkdownLayoutFontBytesError::EmptyFile)
        ));
    }
}
