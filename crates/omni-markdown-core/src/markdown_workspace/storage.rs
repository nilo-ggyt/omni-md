//! Bounded frozen workspace reads, atomic persistence, and historical recovery record validation.

use super::*;
use crate::immutable_snapshot::{SnapshotDirectory, SnapshotError};

mod bounded_text;
pub(super) use bounded_text::BoundedMarkdownText;

/// Derived inspection result paired with one exact content revision and validated file metadata.
pub(super) struct InspectedMarkdown<T> {
    pub(super) value: T,
    pub(super) revision: String,
    /// Native save receipts bind their exact persisted extent to this inspection.
    #[cfg(feature = "native-markdown-candidate")]
    pub(super) byte_count: u64,
    /// Native saves expose this persisted display timestamp; portable callers need value/revision.
    #[cfg(feature = "native-markdown-candidate")]
    pub(super) modified_at_ms: i64,
}

/// Owned callback result after transient mapping and path identity checks complete.
pub(super) struct ValidatedMarkdownText<T> {
    pub(super) value: T,
    /// Carry the validated extent only to the native persistence receipt, never as a second source.
    #[cfg(feature = "native-markdown-candidate")]
    pub(super) byte_count: u64,
    /// Native saves expose this persisted display timestamp; portable callers need the callback result.
    #[cfg(feature = "native-markdown-candidate")]
    pub(super) modified_at_ms: i64,
}

/// Freeze one file identity and lend only bounded source windows. Callback failure,
/// validation failure, or a real close failure prevents publishing the derived result.
pub(super) fn inspect_bounded_markdown_text_cancellable<T>(
    path: &Path,
    maximum_bytes: u64,
    is_current: impl Fn() -> bool,
    inspect: impl FnOnce(&BoundedMarkdownText) -> Result<T, MarkdownWorkspaceError>,
    snapshots: Option<&mut SnapshotDirectory>,
) -> Result<ValidatedMarkdownText<T>, MarkdownWorkspaceError> {
    if !is_current() {
        // Reject cancelled work before opening a source or creating anonymous snapshot storage.
        return Err(MarkdownWorkspaceError::SearchSuperseded);
    }
    // Do not follow a final symlink or block opening a special file; metadata below
    // must independently prove a regular file before capture is allowed.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    let before = file
        .metadata()
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    if !before.is_file() {
        // A nonregular descriptor is not a finite Markdown source, even when open succeeded.
        return Err(MarkdownWorkspaceError::StorageUnavailable);
    }
    if before.len() > maximum_bytes {
        // Enforce the caller's candidate limit before allocating snapshot resources.
        return Err(MarkdownWorkspaceError::DocumentTooLarge);
    }
    let length =
        usize::try_from(before.len()).map_err(|_| MarkdownWorkspaceError::DocumentTooLarge)?;
    let identity = FileIdentity::from(&before);
    // Map only an independently frozen anonymous inode. Candidates may reuse an
    // empty scratch directory, but never a mutable file or another candidate's bytes.
    let captured = match snapshots {
        Some(directory) => directory.capture_backing(&file, before.len(), &is_current),
        None => SnapshotDirectory::new()
            .and_then(|mut directory| directory.capture_backing(&file, before.len(), &is_current)),
    }
    .map_err(|error| match error {
        // Preserve source mutation and cancellation as distinct caller-visible outcomes.
        SnapshotError::SourceChanged => MarkdownWorkspaceError::DocumentChangedDuringRead,
        SnapshotError::Cancelled => MarkdownWorkspaceError::SearchSuperseded,
        SnapshotError::Storage(_) => MarkdownWorkspaceError::StorageUnavailable,
    })?;
    let text = BoundedMarkdownText::new(captured, length)?;
    let result = inspect(&text);
    let validation = if result.is_ok() {
        // Search validates during its own KMP traversal; a local inspector must
        // still prove the entire frozen candidate is UTF-8 before publishing success.
        text.ensure_validated(&is_current)
    } else {
        // Do not rescan after a consumer failure; the explicit close below still runs.
        Ok(())
    };
    // Reclaim the actual source even after consumer or validation failure. Keep
    // its outcome separate so cleanup cannot replace the earlier operation cause.
    let cleanup = text.close();
    let inspected = result.and_then(|value| validation.map(|()| value));
    let value = match (inspected, cleanup) {
        // Only successful inspection, complete validation, and reclamation can
        // release the finite derived result for final source-identity checks.
        (Ok(value), Ok(())) => value,
        // A single failure retains its original typed meaning and source chain.
        (Err(cause), Ok(())) => return Err(cause),
        (Ok(_), Err(cleanup)) => return Err(cleanup),
        // Both facts matter: the inspection failed first, and actual cleanup also
        // failed. Preserve both while exposing the first cause through Error::source.
        (Err(cause), Err(cleanup)) => {
            return Err(MarkdownWorkspaceError::InspectionCleanupFailed {
                cause: Box::new(cause),
                cleanup: Box::new(cleanup),
            });
        }
    };
    if !is_current() {
        // Successful inspection and cleanup cannot revive a superseded search request.
        return Err(MarkdownWorkspaceError::SearchSuperseded);
    }
    // Compare both the still-open descriptor and the current pathname with the
    // original inode, extent, mtime, and ctime before exposing derived metadata.
    let after = file
        .metadata()
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    let path_after = fs::symlink_metadata(path)
        .map_err(|_| MarkdownWorkspaceError::DocumentChangedDuringRead)?;
    if path_after.file_type().is_symlink()
        || FileIdentity::from(&after) != identity
        || FileIdentity::from(&path_after) != identity
    {
        // Discard all derived results when either identity changed; never continue
        // the old scan using a replacement path or descriptor observation.
        return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
    }
    Ok(ValidatedMarkdownText {
        value,
        #[cfg(feature = "native-markdown-candidate")]
        byte_count: after.len(),
        #[cfg(feature = "native-markdown-candidate")]
        modified_at_ms: modified_at_ms(&after),
    })
}

/// Compute a revision through finite validated leases even when the consumer itself
/// does not read text; a revision-only call receives no whole-source borrow privilege.
pub(super) fn inspect_bounded_markdown<T>(
    path: &Path,
    maximum_bytes: u64,
    inspect: impl FnOnce(&BoundedMarkdownText) -> Result<T, MarkdownWorkspaceError>,
) -> Result<InspectedMarkdown<T>, MarkdownWorkspaceError> {
    let inspected = inspect_bounded_markdown_text_cancellable(
        path,
        maximum_bytes,
        || true,
        |text| {
            // Digest the exact original bytes, sharing the traversal with UTF-8 validation.
            let mut digest = Sha256::new();
            text.visit_validated_bytes(&|| true, |_, bytes| {
                digest.update(bytes);
                Ok(())
            })?;
            let digest = digest.finalize();
            let mut revision = String::with_capacity(digest.len() * 2);
            for byte in digest {
                // The public revision is fixed-size lowercase hexadecimal, not a text copy.
                use std::fmt::Write as _;
                let _ = write!(revision, "{byte:02x}");
            }
            // Enter the consumer only after complete UTF-8 validation and the exact
            // byte digest succeed; its source API still exposes only finite windows.
            Ok((inspect(text)?, revision))
        },
        None,
    )?;
    let (value, revision) = inspected.value;
    Ok(InspectedMarkdown {
        value,
        revision,
        #[cfg(feature = "native-markdown-candidate")]
        byte_count: inspected.byte_count,
        #[cfg(feature = "native-markdown-candidate")]
        modified_at_ms: inspected.modified_at_ms,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Descriptor and path identity snapshot, including ctime to detect changes beyond file length.
pub(super) struct FileIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) byte_count: u64,
    pub(super) modified_seconds: i64,
    pub(super) modified_nanoseconds: i64,
    pub(super) changed_seconds: i64,
    pub(super) changed_nanoseconds: i64,
}

impl From<&Metadata> for FileIdentity {
    /// Capture inode, size, modification, and change metadata for before-and-after read validation.
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            byte_count: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

/// Compute the exact persisted revision for finite regression fixture bytes.
#[cfg(test)]
pub(super) fn revision_for_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut revision = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Format each digest byte into exactly two lowercase ASCII digits.
        use std::fmt::Write as _;
        let _ = write!(revision, "{byte:02x}");
    }
    revision
}

/// Convert filesystem modification time to a bounded epoch-millisecond display value.
pub(super) fn modified_at_ms(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| {
            // Treat pre-epoch or unsupported filesystem times as absent display metadata.
            modified.duration_since(UNIX_EPOCH).ok()
        })
        .map_or(0, |duration| {
            // Clamp large timestamps before converting to the public signed millisecond representation.
            duration.as_millis().min(i64::MAX as u128) as i64
        })
}

/// Make an explicit vault directory mutation durable or report its storage failure.
pub(super) fn sync_directory(path: &Path) -> Result<(), MarkdownWorkspaceError> {
    File::open(path)
        .and_then(|directory| {
            // Persist directory-entry changes before the operation reports a durable receipt.
            directory.sync_all()
        })
        .map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })
}

/// Same-parent save temporary whose ownership transfers only after atomic publication.
#[cfg(feature = "native-markdown-candidate")]
pub(super) struct TemporaryMarkdownSave {
    pub(super) path: PathBuf,
    pub(super) file: File,
    pub(super) armed: bool,
}

#[cfg(feature = "native-markdown-candidate")]
impl TemporaryMarkdownSave {
    /// Acquire a new owner-private temporary file exclusively and arm rollback before returning it.
    pub(super) fn create(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            armed: true,
        })
    }

    /// Transfer ownership after successful publication so Drop no longer unlinks the committed file.
    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

/// Acquire a same-directory save temporary so final publication can use atomic rename.
#[cfg(feature = "native-markdown-candidate")]
pub(super) fn create_temporary_markdown_save(
    parent: &Path,
    save_sequence: &AtomicU64,
) -> Result<TemporaryMarkdownSave, MarkdownWorkspaceError> {
    for _ in 0..TEMPORARY_SAVE_ATTEMPTS {
        // Try a bounded sequence of exclusive sibling names without retaining background state.
        let sequence = save_sequence.fetch_add(1, AtomicOrdering::Relaxed);
        let temporary_path = parent.join(format!(
            ".omni-md-save-{}-{sequence}.tmp",
            std::process::id()
        ));
        match TemporaryMarkdownSave::create(&temporary_path) {
            // An acquired temporary carries its descriptor and rollback guard into the save transaction.
            Ok(temporary) => return Ok(temporary),
            // A collision is retried without opening or changing the existing file.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            // Other filesystem failures abort the transaction before publication.
            Err(_) => return Err(MarkdownWorkspaceError::StorageUnavailable),
        }
    }
    Err(MarkdownWorkspaceError::StorageUnavailable)
}

#[cfg(feature = "native-markdown-candidate")]
impl Drop for TemporaryMarkdownSave {
    /// Best-effort rollback of an unpublished temporary file without panicking during teardown.
    fn drop(&mut self) {
        if self.armed {
            // Only an armed, unpublished temporary still belongs to this rollback guard.
            let _ = fs::remove_file(&self.path);
        }
    }
}
