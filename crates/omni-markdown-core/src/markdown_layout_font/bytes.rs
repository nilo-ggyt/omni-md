//! Capture an independent anonymous inode from a verified source descriptor. Fonts borrow a stable
//! blob instead of mapping the user-controlled source that can later be truncated.
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

/// Clones and blob wrappers share one frozen mapping, without retaining the source path, source
/// descriptor, or a writable inode handle. This font-only blob grants no full Markdown-body lease.
#[derive(Clone)]
pub struct MarkdownLayoutFontBytes(Arc<Payload>);
struct Payload {
    // The unique mapping owner actually calls munmap; observer atomics retain neither Font nor VM.
    mapping: ReadonlyFontMapping,
    // Keep only capture-time scalar identity. Different point sizes may share frozen bytes without
    // retaining a source path/descriptor or inheriting another font instance's axes or metrics.
    source: FileIdentity,
}

/// Observe completed disposal, actual munmap, and descriptor close separately; independent atomics
/// are not a live transaction snapshot. After the final owner ends, check completed, unmap success,
/// zero mapped bytes, and no failure together. These facts do not measure kernel RSS.
#[derive(Clone)]
pub struct MarkdownLayoutFontBytesResourceObserver {
    facts: Arc<MappingFacts>,
    captured_bytes: usize,
}
impl MarkdownLayoutFontBytesResourceObserver {
    /// Compare receipts for the same actual mapping without keeping it alive or inferring sharing
    /// from equal paths, lengths, or content. Different point-size Font payloads may share these
    /// bytes; font-payload lifetime and final mapping release remain distinct responsibilities.
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
        // The frozen font factory authorizes the complete immutable font blob. Document source
        // readers keep their separate bounded-lease contract and cannot use this font-only owner.
        self.0.mapping.as_ref()
    }
}

impl MarkdownLayoutFontBytes {
    /// Freeze an independent unnamed inode, then recheck the original descriptor and path without
    /// UTF-8 validation, hashing, or a full-font content scan.
    ///
    /// Metadata revalidation provides the same optimistic concurrency protection as document open;
    /// it cannot detect an external writer deliberately restoring all identity metadata. Memory
    /// safety relies on the independent frozen inode, not on treating the source as immutable.
    ///
    /// # Errors
    /// Reject nonregular files, final-component symlinks, empty/oversized sources, path replacement,
    /// source changes, and actual snapshot/storage failures.
    pub fn capture_file(path: &Path, max_bytes: u64) -> Result<Self> {
        capture(path, max_bytes, || {})
    }

    /// Share frozen bytes within the caller's existing finite font set, without a process cache.
    /// Reuse requires the initial file identity; each new Font still parses/validates its own face,
    /// axes, and point size. Like initial capture, this optimistic check cannot recognize a writer
    /// deliberately restoring all identity metadata.
    ///
    /// # Errors
    /// Reject inaccessible/nonregular sources or the current byte-budget violation; an ordinary
    /// file identity change returns None so the caller can capture the actual new source.
    pub fn share_unchanged_file(&self, path: &Path, max_bytes: u64) -> Result<Option<Self>> {
        let metadata =
            std::fs::symlink_metadata(path).map_err(MarkdownLayoutFontBytesError::Storage)?;
        if !regular(&metadata) {
            return Err(MarkdownLayoutFontBytesError::NotRegularFile);
        }
        if metadata.len() > max_bytes {
            return Err(MarkdownLayoutFontBytesError::FileTooLarge);
        }
        // Any changed inode, extent, mtime, or ctime requires real recapture; path equality alone
        // cannot authorize the old font bytes.
        Ok((FileIdentity::from(&metadata) == self.0.source).then(|| self.clone()))
    }
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.mapping.len()
    }
    /// The small wrapper shares the original payload. A Font blob's strong reference legitimately
    /// extends this mapping's lifetime until that actual consumer finishes.
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
        return Err(MarkdownLayoutFontBytesError::NotRegularFile); // Never wait on or map devices, FIFOs, or other special descriptors.
    }
    let identity = FileIdentity::from(&before);
    if FileIdentity::from(&path_before) != identity {
        return Err(MarkdownLayoutFontBytesError::SourceChanged); // Reject inode replacement between path checking and open.
    }
    if before.len() == 0 {
        return Err(MarkdownLayoutFontBytesError::EmptyFile);
    }
    let bytes =
        usize::try_from(before.len()).map_err(|_| MarkdownLayoutFontBytesError::FileTooLarge)?;
    if before.len() > max_bytes || bytes > isize::MAX as usize {
        return Err(MarkdownLayoutFontBytesError::FileTooLarge); // Bound the extent before capturing or borrowing font bytes.
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
    after_snapshot(); // Production does nothing; the existing test replaces its own path at this actual publication boundary.
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
    // The original descriptor serves only optimistic identity checks and must close before blob
    // publication. A close failure is returned as Storage even if validation already found a change.
    if let Err(error) = close_descriptor(file.into_raw_fd()) {
        mapping.record_error(&error);
        // Mapping cleanup preserves its first recorded errno; failed munmap cannot report a zero
        // extent. No blob is published after this source-descriptor failure.
        mapping
            .close()
            .map_err(MarkdownLayoutFontBytesError::Storage)?;
        return Err(MarkdownLayoutFontBytesError::Storage(error));
    }
    mapping.source_closed();
    if let Err(error) = validation {
        // Return private VM before reporting SourceChanged. Preserve the existing error order:
        // cleanup Storage replaces that validation error if close itself fails; no combined error
        // or successful release is fabricated.
        mapping
            .close()
            .map_err(MarkdownLayoutFontBytesError::Storage)?;
        return Err(error); // SourceChanged is returned only after successful private-mapping cleanup.
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
        assert_eq!(captured.as_ref(), [0, 0xff, 0x80, 7]); // Binary font bytes require no UTF-8 validation or original-inode lease.
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
            drop(blob); // The last shared value really drops on this joined thread; the observer retains no VM owner.
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
        // Observe the same actual mapping lifetime; another live instance must prevent an early
        // successful-release receipt.
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
        // A new inode needs recapture even at equal length; font-file updates cannot reuse old glyph bytes.
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
