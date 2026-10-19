//! 工作区有限冻结读取、原子保存检查与旧兼容草稿；旧全片入口等待其调用方逐项迁移。

use super::*;
use crate::immutable_snapshot::{SnapshotDirectory, SnapshotError, snapshot_file};

mod bounded_text;
pub(super) use bounded_text::BoundedMarkdownText;

/// Derived inspection result paired with one exact content revision and validated file metadata.
pub(super) struct InspectedMarkdown<T> {
    pub(super) value: T,
    pub(super) revision: String,
    pub(super) byte_count: u64,
    pub(super) modified_at_ms: i64,
}

/// Owned callback result after transient mapping and path identity checks complete.
pub(super) struct ValidatedMarkdownText<T> {
    pub(super) value: T,
    pub(super) byte_count: u64,
    pub(super) modified_at_ms: i64,
}

/// 冻结同一个文件身份，只允许有限源窗口；callback失败或真实close失败都不能发布结果。
pub(super) fn inspect_bounded_markdown_text_cancellable<T>(
    path: &Path,
    maximum_bytes: u64,
    is_current: impl Fn() -> bool,
    inspect: impl FnOnce(&BoundedMarkdownText) -> Result<T, MarkdownWorkspaceError>,
    snapshots: Option<&mut SnapshotDirectory>,
) -> Result<ValidatedMarkdownText<T>, MarkdownWorkspaceError> {
    if !is_current() {
        return Err(MarkdownWorkspaceError::SearchSuperseded);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    let before = file
        .metadata()
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    if !before.is_file() {
        return Err(MarkdownWorkspaceError::StorageUnavailable);
    }
    if before.len() > maximum_bytes {
        return Err(MarkdownWorkspaceError::DocumentTooLarge);
    }
    let length =
        usize::try_from(before.len()).map_err(|_| MarkdownWorkspaceError::DocumentTooLarge)?;
    let identity = FileIdentity::from(&before);
    // 只有独立匿名冻结inode可建立有界mapping；每候选共享的只是空scratch目录。
    let captured = match snapshots {
        Some(directory) => directory.capture_backing(&file, before.len(), &is_current),
        None => SnapshotDirectory::new()
            .and_then(|mut directory| directory.capture_backing(&file, before.len(), &is_current)),
    }
    .map_err(|error| match error {
        SnapshotError::SourceChanged => MarkdownWorkspaceError::DocumentChangedDuringRead,
        SnapshotError::Cancelled => MarkdownWorkspaceError::SearchSuperseded,
        SnapshotError::Storage(_) => MarkdownWorkspaceError::StorageUnavailable,
    })?;
    let text = BoundedMarkdownText::new(captured, length)?;
    let result = inspect(&text);
    let validation = if result.is_ok() {
        // 搜索已在同一次KMP扫描完成校验；其他局部检查者仍必须证明整个冻结候选有效。
        text.ensure_validated(&is_current)
    } else {
        Ok(()) // 业务失败不再重扫；归还仍由下面同一个显式close完成。
    };
    text.close()?; // callback错误也先完成实际归还；cleanup失败不能被较早的业务错误吞掉。
    validation?;
    let value = result?;
    if !is_current() {
        return Err(MarkdownWorkspaceError::SearchSuperseded);
    }
    let after = file
        .metadata()
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    let path_after = fs::symlink_metadata(path)
        .map_err(|_| MarkdownWorkspaceError::DocumentChangedDuringRead)?;
    if path_after.file_type().is_symlink()
        || FileIdentity::from(&after) != identity
        || FileIdentity::from(&path_after) != identity
    {
        // 原路径或descriptor变化时丢弃全部派生结果，不拿新身份继续旧扫描。
        return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
    }
    Ok(ValidatedMarkdownText {
        value,
        byte_count: after.len(),
        modified_at_ms: modified_at_ms(&after),
    })
}

/// 只需revision的调用者也必须有限校验/散列；不得因callback不读正文而留全片扫描。
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
            let mut digest = Sha256::new();
            text.visit_validated_bytes(&|| true, |_, bytes| {
                digest.update(bytes);
                Ok(())
            })?;
            let digest = digest.finalize();
            let mut revision = String::with_capacity(digest.len() * 2);
            for byte in digest {
                use std::fmt::Write as _;
                let _ = write!(revision, "{byte:02x}");
            }
            // 完整UTF8和精确原字节摘要都成功后才进入消费者；消费者仍无全片借用接口。
            Ok((inspect(text)?, revision))
        },
        None,
    )?;
    let (value, revision) = inspected.value;
    Ok(InspectedMarkdown {
        value,
        revision,
        byte_count: inspected.byte_count,
        modified_at_ms: inspected.modified_at_ms,
    })
}

/// Compute legacy open-note character and line metrics without a normalized document copy.
pub(super) fn markdown_open_metrics(text: &str) -> MarkdownOpenMetrics {
    let bytes = text.as_bytes();
    let mut character_count = 0usize;
    let mut line_count = 1usize;
    let mut saw_carriage_return = false;
    let mut saw_crlf = false;
    let mut index = 0usize;

    while index < bytes.len() {
        // Count one scalar per UTF-8 leading byte and merge the two bytes of CRLF into one visible character.
        let byte = bytes[index];
        if byte == b'\r' {
            // A CR starts one logical line break and needs lookahead for a paired LF.
            character_count += 1;
            line_count += 1;
            saw_carriage_return = true;
            if bytes.get(index + 1) == Some(&b'\n') {
                // A paired LF belongs to the same line break and character as this CR.
                saw_crlf = true;
                index += 2;
            } else {
                // A standalone CR consumes only its own byte.
                index += 1;
            }
            continue;
        }

        if byte & 0b1100_0000 != 0b1000_0000 {
            // Continuation bytes do not begin another Unicode scalar.
            character_count += 1;
        }
        if byte == b'\n' {
            // Only an LF not already consumed by CRLF creates another logical line.
            line_count += 1;
        }
        index += 1;
    }

    let line_separator = if saw_crlf {
        // Prefer CRLF when any such pair exists, preserving the established mixed-ending behavior.
        "\r\n"
    } else if saw_carriage_return {
        // Otherwise an observed CR selects the carriage-return preference.
        "\r"
    } else {
        // LF is the default for LF-only text and documents without line breaks.
        "\n"
    };
    MarkdownOpenMetrics {
        character_count,
        line_count,
        line_separator,
    }
}

/// Pair caller inspection with a SHA-256 revision under one validated short-lived mapping.
pub(super) fn inspect_mapped_markdown<T>(
    path: &Path,
    maximum_bytes: u64,
    inspect: impl FnOnce(&str) -> T,
) -> Result<InspectedMarkdown<T>, MarkdownWorkspaceError> {
    let inspected = inspect_validated_markdown_text(path, maximum_bytes, |text| {
        // The callback and revision observe the same borrowed bytes before identity revalidation.
        let revision = revision_for_bytes(text.as_bytes());
        (inspect(text), revision)
    })?;
    let (value, revision) = inspected.value;
    Ok(InspectedMarkdown {
        value,
        revision,
        byte_count: inspected.byte_count,
        modified_at_ms: inspected.modified_at_ms,
    })
}

/// Lend validated UTF-8 only during one call and reject identity changes before publishing its result.
pub(super) fn inspect_validated_markdown_text<T>(
    path: &Path,
    maximum_bytes: u64,
    inspect: impl FnOnce(&str) -> T,
) -> Result<ValidatedMarkdownText<T>, MarkdownWorkspaceError> {
    // Ordinary explicit reads have no search owner to cancel; all resource cleanup remains local.
    inspect_validated_markdown_text_cancellable(
        path,
        maximum_bytes,
        || {
            // Only search supplies an external cancellation predicate.
            true
        },
        inspect,
        None,
    )
}

/// Snapshot a mutable source safely while allowing a closing search owner to stop fallback copying.
pub(super) fn inspect_validated_markdown_text_cancellable<T>(
    path: &Path,
    maximum_bytes: u64,
    is_current: impl Fn() -> bool,
    inspect: impl FnOnce(&str) -> T,
    snapshots: Option<&mut SnapshotDirectory>,
) -> Result<ValidatedMarkdownText<T>, MarkdownWorkspaceError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
    let before = file.metadata().map_err(|_| {
        // Translate a failed filesystem stage into the stable vault-storage error category.
        MarkdownWorkspaceError::StorageUnavailable
    })?;
    if !before.is_file() {
        // Never let a device, FIFO, or directory enter the source snapshot contract.
        return Err(MarkdownWorkspaceError::StorageUnavailable);
    }
    if before.len() > maximum_bytes {
        // Reject oversized files before establishing a mapping or invoking the inspection callback.
        return Err(MarkdownWorkspaceError::DocumentTooLarge);
    }
    let identity = FileIdentity::from(&before);

    // Only the distinct unnamed snapshot inode is mapped; source mutation can invalidate the result
    // through metadata revalidation below but cannot mutate an already borrowed Rust UTF-8 slice.
    let captured = match snapshots {
        // A vault search reuses only its empty scratch namespace; every returned mapping owns a distinct inode.
        Some(directory) => directory.capture(&file, before.len(), &is_current),
        // Explicit standalone reads own and release their entire namespace within this acquisition.
        None => snapshot_file(&file, before.len(), &is_current),
    };
    let snapshot = captured.map_err(|error| {
        // Preserve expected cancellation and changed-source semantics at the workspace boundary.
        match error {
            // A truncated or grown source cannot satisfy this inspection's captured baseline.
            SnapshotError::SourceChanged => MarkdownWorkspaceError::DocumentChangedDuringRead,
            // The caller will discard superseded search results after all local resources have dropped.
            SnapshotError::Cancelled => MarkdownWorkspaceError::SearchSuperseded,
            // Filesystem acquisition errors remain explicit storage failures.
            SnapshotError::Storage(_) => MarkdownWorkspaceError::StorageUnavailable,
        }
    })?;
    let text = std::str::from_utf8(snapshot.as_deref().unwrap_or_default()).map_err(|_| {
        // Invalid source bytes are rejected only after they are safely frozen in private storage.
        MarkdownWorkspaceError::InvalidUtf8Document
    })?;
    let value = inspect(text);
    if !is_current() {
        // A closed search owner needs no further source metadata I/O or stale callback result publication.
        return Err(MarkdownWorkspaceError::SearchSuperseded);
    }

    let after = file.metadata().map_err(|_| {
        // Translate a failed filesystem stage into the stable vault-storage error category.
        MarkdownWorkspaceError::StorageUnavailable
    })?;
    let path_after = fs::symlink_metadata(path).map_err(|_| {
        // A failed identity recheck cannot establish that inspected or copied source bytes stayed stable.
        MarkdownWorkspaceError::DocumentChangedDuringRead
    })?;
    if path_after.file_type().is_symlink()
        || FileIdentity::from(&after) != identity
        || FileIdentity::from(&path_after) != identity
    {
        // Concurrent replacement, metadata mutation, or symlink substitution invalidates all derived output.
        return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
    }
    Ok(ValidatedMarkdownText {
        value,
        byte_count: after.len(),
        modified_at_ms: modified_at_ms(&after),
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

/// Compute the stable lowercase SHA-256 revision for exact persisted UTF-8 bytes.
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

/// Validate a recovery identifier without requiring the original note to remain present.
pub(super) fn validate_conflict_draft_relative_path(
    relative_path: &str,
) -> Result<(), MarkdownWorkspaceError> {
    let path = validate_relative_path(relative_path, false)?;
    if targets_reserved_attachments_tree(path) {
        // Reserved asset paths cannot own note conflict drafts.
        return Err(MarkdownWorkspaceError::InvalidRelativePath);
    }
    if !has_markdown_extension(path) {
        // Only Markdown note identifiers are eligible for recovery storage.
        return Err(MarkdownWorkspaceError::NotMarkdown);
    }
    if relative_path.len() > MAX_RECOVERY_RELATIVE_PATH_BYTES {
        // Bound serialized path bytes before encoding the private recovery record.
        return Err(MarkdownWorkspaceError::InvalidRelativePath);
    }
    Ok(())
}

/// Map an untrusted relative identifier to a fixed-length private recovery filename.
pub(super) fn conflict_draft_path(recovery_directory: &Path, relative_path: &str) -> PathBuf {
    let digest = Sha256::digest(relative_path.as_bytes());
    let mut file_name = String::with_capacity(digest.len() * 2 + ".draft".len());
    for byte in digest {
        // Hex encoding preserves a filesystem-safe deterministic key without exposing directory syntax.
        use std::fmt::Write as _;
        let _ = write!(file_name, "{byte:02x}");
    }
    file_name.push_str(".draft");
    recovery_directory.join(file_name)
}

/// Accept only a real owner-private recovery directory and distinguish absence from unsafe storage.
pub(super) fn existing_conflict_recovery_directory(
    recovery_directory: &Path,
) -> Result<bool, MarkdownWorkspaceError> {
    match fs::symlink_metadata(recovery_directory) {
        // A non-symlink directory without group or other permissions can hold private unsaved drafts.
        Ok(metadata)
            if !metadata.file_type().is_symlink()
                && metadata.is_dir()
                && metadata.permissions().mode() & 0o077 == 0 =>
        {
            Ok(true)
        }
        // An existing unsafe type or permission mode must not receive draft bytes.
        Ok(_) => Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
        // A missing directory is a normal no-recovery state that writers may create explicitly.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        // Other lookup failures prevent safe recovery and remain caller-visible.
        Err(_) => Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
    }
}

/// Create owner-private recovery storage only when an explicit draft write requires it.
pub(super) fn ensure_conflict_recovery_directory(
    recovery_directory: &Path,
) -> Result<(), MarkdownWorkspaceError> {
    if existing_conflict_recovery_directory(recovery_directory)? {
        // An already validated directory needs no mutation or permission change.
        return Ok(());
    }

    let parent = recovery_directory
        .parent()
        .ok_or(MarkdownWorkspaceError::ConflictRecoveryUnavailable)?;
    fs::create_dir_all(parent).map_err(|_| {
        // A failed private-storage stage must keep unsaved recovery persistence visibly unsuccessful.
        MarkdownWorkspaceError::ConflictRecoveryUnavailable
    })?;
    match fs::create_dir(recovery_directory) {
        // This call created the directory and will validate its final type before use.
        Ok(()) => {}
        // A concurrent creator is tolerated only because the resulting entry is validated below.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        // Other creation errors prevent reliable draft persistence.
        Err(_) => return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
    }

    let metadata = fs::symlink_metadata(recovery_directory).map_err(|_| {
        // A failed private-storage stage must keep unsaved recovery persistence visibly unsuccessful.
        MarkdownWorkspaceError::ConflictRecoveryUnavailable
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        // A replaced path or non-directory is rejected before permissions or data writes.
        return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable);
    }
    fs::set_permissions(recovery_directory, fs::Permissions::from_mode(0o700)).map_err(|_| {
        // A failed private-storage stage must keep unsaved recovery persistence visibly unsuccessful.
        MarkdownWorkspaceError::ConflictRecoveryUnavailable
    })?;
    sync_conflict_recovery_directory(parent)?;
    Ok(())
}

/// Require a regular owner-private file before mapping, replacing, or deleting a recovery record.
pub(super) fn validate_conflict_draft_file(
    metadata: &Metadata,
) -> Result<(), MarkdownWorkspaceError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
    {
        // Links, special entries, and shared permissions violate the private draft contract.
        Err(MarkdownWorkspaceError::ConflictDraftCorrupted)
    } else {
        // A regular private file is eligible for structural record validation.
        Ok(())
    }
}

/// Persist the legacy conflict record through an exclusive private temporary file and atomic rename.
pub(super) fn write_conflict_draft(
    recovery_directory: &Path,
    relative_path: &str,
    content: &str,
    save_sequence: &AtomicU64,
) -> Result<(), MarkdownWorkspaceError> {
    validate_conflict_draft_relative_path(relative_path)?;
    if content.len() as u64 > MAX_DOCUMENT_BYTES {
        // Reject an oversized draft without altering any existing recovery record.
        return Err(MarkdownWorkspaceError::DocumentTooLarge);
    }
    ensure_conflict_recovery_directory(recovery_directory)?;

    let destination = conflict_draft_path(recovery_directory, relative_path);
    match fs::symlink_metadata(&destination) {
        // Existing draft storage must still be a valid private regular file.
        Ok(metadata) => validate_conflict_draft_file(&metadata)?,
        // Absence permits a first draft to be atomically installed.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        // Lookup failures must not be treated as an absent replaceable destination.
        Err(_) => return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
    }

    // Serialize bounded big-endian lengths and the exact note identifier before unsaved content.
    let relative_path_bytes = relative_path.as_bytes();
    let relative_path_byte_count = u32::try_from(relative_path_bytes.len()).map_err(|_| {
        // The path length or representation cannot safely fit the bounded recovery identifier contract.
        MarkdownWorkspaceError::InvalidRelativePath
    })?;
    let content_byte_count = u64::try_from(content.len()).map_err(|_| {
        // The requested extent cannot fit the editor's supported byte or signed-coordinate limit.
        MarkdownWorkspaceError::DocumentTooLarge
    })?;
    let mut header = [0_u8; CONFLICT_DRAFT_HEADER_BYTES];
    header[..CONFLICT_DRAFT_MAGIC.len()].copy_from_slice(CONFLICT_DRAFT_MAGIC);
    header[8..12].copy_from_slice(&relative_path_byte_count.to_be_bytes());
    header[12..20].copy_from_slice(&content_byte_count.to_be_bytes());

    // Write and sync under rollback ownership; rename publishes a complete record in one directory operation.
    let mut temporary = create_temporary_conflict_draft(recovery_directory, save_sequence)?;
    temporary
        .file
        .write_all(&header)
        .and_then(|()| {
            // Serialize the exact note identifier after the fixed header under the same rollback guard.
            temporary.file.write_all(relative_path_bytes)
        })
        .and_then(|()| {
            // Append unsaved UTF-8 bytes only after the complete identifier has been written.
            temporary.file.write_all(content.as_bytes())
        })
        .and_then(|()| {
            // Make the complete temporary record durable before its name is atomically published.
            temporary.file.sync_all()
        })
        .map_err(|_| {
            // A failed private-storage stage must keep unsaved recovery persistence visibly unsuccessful.
            MarkdownWorkspaceError::ConflictRecoveryUnavailable
        })?;
    fs::rename(&temporary.path, &destination).map_err(|_| {
        // A failed private-storage stage must keep unsaved recovery persistence visibly unsuccessful.
        MarkdownWorkspaceError::ConflictRecoveryUnavailable
    })?;
    temporary.disarm();
    sync_conflict_recovery_directory(recovery_directory)
}

/// Recover one validated private record and verify its identity after decoding.
pub(super) fn read_conflict_draft(
    recovery_directory: &Path,
    expected_relative_path: &str,
) -> Result<Option<String>, MarkdownWorkspaceError> {
    validate_conflict_draft_relative_path(expected_relative_path)?;
    if !existing_conflict_recovery_directory(recovery_directory)? {
        // Without a recovery directory there is no draft to load and no reason to create storage.
        return Ok(None);
    }

    let path = conflict_draft_path(recovery_directory, expected_relative_path);
    let path_metadata = match fs::symlink_metadata(&path) {
        // An existing path is validated for private regular-file ownership below.
        Ok(metadata) => metadata,
        // A missing per-note record is a normal no-draft result.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // Other lookup failures cannot be silently treated as absent unsaved work.
        Err(_) => return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
    };
    validate_conflict_draft_file(&path_metadata)?;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| {
            // Unreadable, invalidly encoded, or unstable private-record bytes cannot supply recovered unsaved text.
            MarkdownWorkspaceError::ConflictDraftCorrupted
        })?;
    let before = file.metadata().map_err(|_| {
        // Unreadable, invalidly encoded, or unstable private-record bytes cannot supply recovered unsaved text.
        MarkdownWorkspaceError::ConflictDraftCorrupted
    })?;
    validate_conflict_draft_file(&before)?;
    let maximum_recovery_bytes = (CONFLICT_DRAFT_HEADER_BYTES + MAX_RECOVERY_RELATIVE_PATH_BYTES)
        as u64
        + MAX_DOCUMENT_BYTES;
    if before.len() < CONFLICT_DRAFT_HEADER_BYTES as u64 || before.len() > maximum_recovery_bytes {
        // Reject impossible record sizes before mapping or trusting encoded lengths.
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }
    let identity = FileIdentity::from(&before);

    // Recovery files can also be modified by same-user tools. Freeze their bytes before parsing
    // lengths or lending UTF-8 views; private permissions alone are not an immutable-inode guarantee.
    let mapping = snapshot_file(&file, before.len(), || {
        // Explicit recovery loading has no independently cancellable worker owner.
        true
    })
    .map_err(|_| {
        // A changed extent or unavailable snapshot cannot provide trustworthy unsaved recovery text.
        MarkdownWorkspaceError::ConflictDraftCorrupted
    })?;
    let content = parse_conflict_draft(
        mapping.as_deref().unwrap_or_default(),
        expected_relative_path,
    )?;

    let after = file.metadata().map_err(|_| {
        // Unreadable, invalidly encoded, or unstable private-record bytes cannot supply recovered unsaved text.
        MarkdownWorkspaceError::ConflictDraftCorrupted
    })?;
    let path_after = fs::symlink_metadata(&path).map_err(|_| {
        // Unreadable, invalidly encoded, or unstable private-record bytes cannot supply recovered unsaved text.
        MarkdownWorkspaceError::ConflictDraftCorrupted
    })?;
    if FileIdentity::from(&after) != identity
        || FileIdentity::from(&path_after) != identity
        || path_after.file_type().is_symlink()
    {
        // A replacement or mutation during decoding invalidates the recovered content.
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }
    Ok(Some(content))
}

/// 固定header只决定合法范围与预算；此函数不借用正文，也不把未固定的源文件声明为不可变。
pub(super) fn conflict_draft_payload_extent(
    header: &[u8],
    record_bytes: usize,
) -> Result<std::ops::Range<usize>, MarkdownWorkspaceError> {
    if header.len() < CONFLICT_DRAFT_HEADER_BYTES
        || &header[..CONFLICT_DRAFT_MAGIC.len()] != CONFLICT_DRAFT_MAGIC
    {
        // 缺失或错误magic不能授权按后面的不可信长度分配、复制或建立映射。
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }

    // 仅在20B头完整后解码两个big-endian长度，后续所有端点均使用checked算术。
    let mut relative_path_length_bytes = [0_u8; 4];
    relative_path_length_bytes.copy_from_slice(&header[8..12]);
    let relative_path_byte_count = u32::from_be_bytes(relative_path_length_bytes) as usize;
    let mut content_length_bytes = [0_u8; 8];
    content_length_bytes.copy_from_slice(&header[12..20]);
    let content_byte_count =
        usize::try_from(u64::from_be_bytes(content_length_bytes)).map_err(|_| {
            // 无法寻址的正文长度不是可恢复的有界record。
            MarkdownWorkspaceError::ConflictDraftCorrupted
        })?;
    if relative_path_byte_count > MAX_RECOVERY_RELATIVE_PATH_BYTES
        || content_byte_count as u64 > MAX_DOCUMENT_BYTES
    {
        // 路径和正文沿用旧写入格式的上限，防止header诱导无界读取。
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }

    let relative_path_end = CONFLICT_DRAFT_HEADER_BYTES
        .checked_add(relative_path_byte_count)
        .ok_or(MarkdownWorkspaceError::ConflictDraftCorrupted)?;
    let content_end = relative_path_end
        .checked_add(content_byte_count)
        .ok_or(MarkdownWorkspaceError::ConflictDraftCorrupted)?;
    if content_end != record_bytes {
        // 丢字节或额外尾字节都不能被当作部分有效的恢复内容。
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }
    Ok(relative_path_end..content_end)
}

/// 固定record的精确内嵌path必须匹配请求；只返回正文范围，不分配正文String。
pub(super) fn conflict_draft_payload_range(
    bytes: &[u8],
    expected_relative_path: &str,
) -> Result<std::ops::Range<usize>, MarkdownWorkspaceError> {
    let payload = conflict_draft_payload_extent(bytes, bytes.len())?;
    if bytes.get(CONFLICT_DRAFT_HEADER_BYTES..payload.start)
        != Some(expected_relative_path.as_bytes())
    {
        // 请求path本身是合法UTF-8，精确byte相等也验证了存储path编码；文件名哈希不足以代替此检查。
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }
    Ok(payload)
}

/// 仅legacy String接口复制正文；原生恢复使用同一范围验证后直接接immutable mapping。
pub(super) fn parse_conflict_draft(
    bytes: &[u8],
    expected_relative_path: &str,
) -> Result<String, MarkdownWorkspaceError> {
    let payload = conflict_draft_payload_range(bytes, expected_relative_path)?;
    let content = std::str::from_utf8(
        bytes
            .get(payload)
            .ok_or(MarkdownWorkspaceError::ConflictDraftCorrupted)?,
    )
    .map_err(|_| {
        // legacy返回类型要求完整String，但非法UTF-8仍拒绝，不进行lossy修复。
        MarkdownWorkspaceError::ConflictDraftCorrupted
    })?;
    Ok(content.to_owned())
}

/// Remove only a validated private note record and durably publish the deletion.
pub(super) fn remove_conflict_draft(
    recovery_directory: &Path,
    relative_path: &str,
) -> Result<(), MarkdownWorkspaceError> {
    if !existing_conflict_recovery_directory(recovery_directory)? {
        // Absent recovery storage makes removal an idempotent no-op.
        return Ok(());
    }
    let path = conflict_draft_path(recovery_directory, relative_path);
    match fs::symlink_metadata(&path) {
        // Validate the existing record before deleting a path in private recovery storage.
        Ok(metadata) => validate_conflict_draft_file(&metadata)?,
        // An already absent record has the desired final state.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        // Other lookup errors must be reported so callers do not assume a draft was cleared.
        Err(_) => return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
    }
    fs::remove_file(path).map_err(|_| {
        // A failed private-storage stage must keep unsaved recovery persistence visibly unsuccessful.
        MarkdownWorkspaceError::ConflictRecoveryUnavailable
    })?;
    sync_conflict_recovery_directory(recovery_directory)
}

/// Sync recovery directory metadata with its dedicated user-visible persistence failure.
pub(super) fn sync_conflict_recovery_directory(path: &Path) -> Result<(), MarkdownWorkspaceError> {
    File::open(path)
        .and_then(|directory| {
            // Persist directory-entry changes before the operation reports a durable receipt.
            directory.sync_all()
        })
        .map_err(|_| {
            // A failed private-storage stage must keep unsaved recovery persistence visibly unsuccessful.
            MarkdownWorkspaceError::ConflictRecoveryUnavailable
        })
}

/// Exclusive private recovery temporary with descriptor ownership and best-effort rollback.
pub(super) struct TemporaryConflictDraft {
    pub(super) path: PathBuf,
    pub(super) file: File,
    pub(super) armed: bool,
}

impl TemporaryConflictDraft {
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

/// Acquire a bounded-attempt private draft temporary without overwriting another in-flight save.
pub(super) fn create_temporary_conflict_draft(
    recovery_directory: &Path,
    save_sequence: &AtomicU64,
) -> Result<TemporaryConflictDraft, MarkdownWorkspaceError> {
    for _ in 0..TEMPORARY_SAVE_ATTEMPTS {
        // Combine process identity and a monotonic session sequence; collisions consume one bounded retry.
        let sequence = save_sequence.fetch_add(1, AtomicOrdering::Relaxed);
        let temporary_path = recovery_directory.join(format!(
            ".omni-md-conflict-{}-{sequence}.tmp",
            std::process::id()
        ));
        match TemporaryConflictDraft::create(&temporary_path) {
            // Exclusive creation transfers the temporary file and rollback obligation to the caller.
            Ok(temporary) => return Ok(temporary),
            // An occupied name belongs to another writer and must remain untouched.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            // Other I/O failures stop draft persistence immediately.
            Err(_) => return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
        }
    }
    Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable)
}

impl Drop for TemporaryConflictDraft {
    /// Best-effort rollback of an unpublished temporary file without panicking during teardown.
    fn drop(&mut self) {
        if self.armed {
            // Only an armed, unpublished temporary still belongs to this rollback guard.
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Same-parent save temporary whose ownership transfers only after atomic publication.
pub(super) struct TemporaryMarkdownSave {
    pub(super) path: PathBuf,
    pub(super) file: File,
    pub(super) armed: bool,
}

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

impl Drop for TemporaryMarkdownSave {
    /// Best-effort rollback of an unpublished temporary file without panicking during teardown.
    fn drop(&mut self) {
        if self.armed {
            // Only an armed, unpublished temporary still belongs to this rollback guard.
            let _ = fs::remove_file(&self.path);
        }
    }
}
