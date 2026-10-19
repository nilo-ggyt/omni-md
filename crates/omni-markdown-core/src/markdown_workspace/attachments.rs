//! Confined attachment import, relative links, and safe note-move rewriting.

use super::*;
use std::os::fd::{AsRawFd, FromRawFd};

mod streaming;
pub(super) use streaming::rewrite_moved_note_attachment_links_streamed;

/// Establish the reserved real asset directory only when an explicit import needs it.
pub(super) fn ensure_attachments_directory(
    canonical_root: &Path,
) -> Result<PathBuf, MarkdownWorkspaceError> {
    let directory = canonical_root.join(ATTACHMENTS_DIRECTORY_NAME);
    let mut created = false;
    match fs::symlink_metadata(&directory) {
        // An existing link or non-directory cannot own imported vault assets.
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        // An existing real directory is reused and revalidated below.
        Ok(_) => {}
        // A missing asset directory may be created by this import.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir(&directory) {
                // Record that this call changed root directory metadata and must sync it.
                Ok(()) => created = true,
                // A concurrent creation is tolerated because its resulting type and canonical boundary are rechecked.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                // Other creation failures abort import before any asset bytes are copied.
                Err(_) => return Err(MarkdownWorkspaceError::StorageUnavailable),
            }
        }
        // Lookup failures cannot be treated as safe absence.
        Err(_) => return Err(MarkdownWorkspaceError::StorageUnavailable),
    }

    let metadata = fs::symlink_metadata(&directory).map_err(|_| {
        // Translate a failed filesystem stage into the stable vault-storage error category.
        MarkdownWorkspaceError::StorageUnavailable
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        // A path substituted during creation or lookup is rejected before asset writes.
        return Err(MarkdownWorkspaceError::StorageUnavailable);
    }
    let canonical_directory = directory.canonicalize().map_err(|_| {
        // Translate a failed filesystem stage into the stable vault-storage error category.
        MarkdownWorkspaceError::StorageUnavailable
    })?;
    ensure_within_root(canonical_root, &canonical_directory)?;
    if created {
        // Only a newly created directory requires syncing the root entry here.
        sync_directory(canonical_root)?;
    }
    Ok(canonical_directory)
}

/// Stream an exclusively created asset while retaining its descriptor and confined parent authority.
pub(super) fn create_collision_safe_attachment(
    canonical_root: &Path,
    attachments_directory: &Path,
    source_name: &str,
    source: &mut File,
    expected_byte_count: u64,
) -> Result<PendingAttachmentFile, MarkdownWorkspaceError> {
    // The directory descriptor pins the validated parent; no later final-component link is followed.
    let parent = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(attachments_directory)
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    // Reject an already observed parent substitution before creating or copying even a partial asset.
    verify_attachment_parent(canonical_root, attachments_directory, &parent)?;
    for suffix in 1_u16..=1_000 {
        // Every bounded collision attempt preserves preexisting entries and acquires only a new inode.
        let candidate_name = numbered_attachment_name(source_name, suffix)?;
        let name = CString::new(candidate_name.as_bytes())
            .map_err(|_| MarkdownWorkspaceError::InvalidAttachmentName)?;
        // SAFETY: the live directory FD and NUL-terminated filename remain valid through openat.
        // O_EXCL acquires a new inode; mode0600 and O_NOFOLLOW prevent replacing/following an entry.
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
                // An occupied asset name remains untouched while the next finite suffix is attempted.
                continue;
            }
            // Other acquisition failures cannot be presented as a completed import.
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        // SAFETY: successful openat returned one fresh owned FD, transferred exactly once into File.
        let file = unsafe { File::from_raw_fd(descriptor) };
        let mut pending = PendingAttachmentFile {
            path: attachments_directory.join(candidate_name),
            directory: attachments_directory.to_owned(),
            canonical_root: canonical_root.to_owned(),
            name,
            file,
            parent,
            identity: None,
            armed: true,
        };
        // Copy through fixed scratch; every ordinary failure seals its partial owned inode for cleanup.
        let copied = copy_attachment_bytes(source, &mut pending.file, expected_byte_count)
            .and_then(|()| {
                pending
                    .file
                    .sync_all()
                    .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)
            });
        let sealed = pending.seal_identity();
        copied?;
        sealed?;
        pending.verify_current()?;
        return Ok(pending);
    }
    Err(MarkdownWorkspaceError::NameCollisionLimitExceeded)
}

/// Own a newly copied attachment until its corresponding canonical link edit is accepted.
///
/// This token cannot be cloned or fabricated from a relative path. It retains two transient FDs;
/// commit/drop closes them. Explicit rollback reports an observed replacement/modification instead
/// of unlinking it. Identity-check/unlink is a cooperating private-directory transaction, not an
/// atomic defense against a hostile same-user process racing filesystem names between syscalls.
#[must_use = "commit after accepting the canonical link edit, or explicitly roll back the import"]
pub struct MarkdownPendingAttachmentImport {
    receipt: MarkdownAttachmentImportReceipt,
    pending: PendingAttachmentFile,
}

impl MarkdownPendingAttachmentImport {
    /// Transfer the already validated file guard and bounded insertion receipt into one token.
    pub(super) fn new(
        receipt: MarkdownAttachmentImportReceipt,
        pending: PendingAttachmentFile,
    ) -> Self {
        Self { receipt, pending }
    }

    /// Borrow insertion text without relinquishing cleanup authority over the new attachment.
    #[must_use]
    pub fn receipt(&self) -> &MarkdownAttachmentImportReceipt {
        &self.receipt
    }

    /// Accept the asset only after the caller has successfully committed its canonical link edit.
    #[must_use]
    pub fn commit(mut self) -> MarkdownAttachmentImportReceipt {
        // The durable file becomes ordinary vault data; descriptor cleanup must no longer unlink it.
        self.pending.armed = false;
        self.receipt
    }

    /// Remove this exact unchanged new asset, returning a bounded error if ownership is uncertain.
    ///
    /// # Errors
    ///
    /// Preserve detected replacements, aliases, modified content metadata and changed parent paths.
    /// A directory-sync failure after unlink reports durability uncertainty rather than recreating data.
    pub fn rollback(mut self) -> Result<(), MarkdownWorkspaceError> {
        // Explicit cleanup consumes the authority exactly once, including a failed identity check.
        self.pending.rollback()
    }
}

/// Pin the new inode and its real parent; no caller-supplied receipt/path can manufacture cleanup authority.
pub(super) struct PendingAttachmentFile {
    pub(super) path: PathBuf,
    directory: PathBuf,
    canonical_root: PathBuf,
    name: CString,
    file: File,
    parent: File,
    identity: Option<FileIdentity>,
    armed: bool,
}

impl PendingAttachmentFile {
    /// Snapshot complete copied bytes or a failed partial copy before allowing guarded cleanup.
    fn seal_identity(&mut self) -> Result<(), MarkdownWorkspaceError> {
        let metadata = self
            .file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            // An unexpected alias is preserved; this transaction cannot claim exclusive file ownership.
            return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
        }
        self.identity = Some(FileIdentity::from(&metadata));
        Ok(())
    }

    /// Reject an observed missing/changed asset before publishing its insertion receipt.
    pub(super) fn verify_current(&self) -> Result<(), MarkdownWorkspaceError> {
        // A provisional receipt must still name the acquired inode under its original confined parent.
        self.checked_entry()?
            .ok_or(MarkdownWorkspaceError::DocumentChangedDuringRead)?;
        Ok(())
    }

    /// Validate the pinned directory, original descriptor and current parent-relative entry.
    fn checked_entry(&self) -> Result<Option<File>, MarkdownWorkspaceError> {
        let expected = self
            .identity
            .ok_or(MarkdownWorkspaceError::StorageUnavailable)?;
        // Acquisition and rollback share the same observed parent identity/confinement requirement.
        verify_attachment_parent(&self.canonical_root, &self.directory, &self.parent)?;
        let original = self
            .file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        if !original.is_file() || original.nlink() != 1 || FileIdentity::from(&original) != expected
        {
            // Later writes or aliases revoke cleanup authority even if the original inode remains open.
            return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
        }
        // SAFETY: the pinned parent FD and NUL-terminated single filename remain live during openat.
        // O_NOFOLLOW/NONBLOCK reject symlink/device substitution without following or blocking on it.
        let descriptor = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                // The original entry is already absent; no other name or inode belongs to rollback.
                return Ok(None);
            }
            // A substituted symlink, unavailable entry or other IO error is preserved for explicit recovery.
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        // SAFETY: openat returned one new owned FD and File takes its sole closing responsibility.
        let current = unsafe { File::from_raw_fd(descriptor) };
        let metadata = current
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        if !metadata.is_file() || metadata.nlink() != 1 || FileIdentity::from(&metadata) != expected
        {
            // The same filename now refers to externally changed data; never unlink that observed entry.
            return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
        }
        Ok(Some(current))
    }

    /// Verify ownership and perform exactly one parent-relative unlink, without destructor retries.
    fn rollback(&mut self) -> Result<(), MarkdownWorkspaceError> {
        if !self.armed {
            // A committed or previously attempted token has no remaining unlink authority.
            return Ok(());
        }
        self.armed = false;
        let Some(_current) = self.checked_entry()? else {
            // An already absent entry needs no deletion, and other names remain outside this authority.
            return Ok(());
        };
        // SAFETY: the pinned directory FD and validated single-component name are live; flag0 unlinks
        // only that directory entry. Identity checks above define the cooperating-writer boundary.
        if unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) } != 0 {
            // A failed unlink leaves the entry untouched and is visible to the explicit caller.
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        // Persist the removal before reporting complete rollback; the original FD closes with this token.
        self.parent
            .sync_all()
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)
    }
}

/// Compare the current real confined directory with the descriptor pinned by this import operation.
pub(super) fn verify_attachment_parent(
    canonical_root: &Path,
    directory_path: &Path,
    parent: &File,
) -> Result<(), MarkdownWorkspaceError> {
    let directory = fs::symlink_metadata(directory_path)
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    let pinned = parent
        .metadata()
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    if directory.file_type().is_symlink()
        || !directory.is_dir()
        || directory.dev() != pinned.dev()
        || directory.ino() != pinned.ino()
    {
        // An observed path replacement cannot acquire or remove entries through a different owner.
        return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
    }
    let canonical = directory_path
        .canonicalize()
        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
    // This is an observed cooperating-directory boundary; separate syscalls are not an atomic race defense.
    ensure_within_root(canonical_root, &canonical)
}

impl Drop for PendingAttachmentFile {
    fn drop(&mut self) {
        if self.armed && self.rollback().is_err() {
            // Destructors never panic, retry or disclose paths/bodies; explicit rollback reports this error.
            let _ = writeln!(
                std::io::stderr().lock(),
                "Attachment rollback could not verify or synchronize its owned entry"
            );
        }
    }
}

#[cfg(test)]
mod import_tests;

/// Preserve a source extension while adding a collision suffix only when needed.
pub(super) fn numbered_attachment_name(
    source_name: &str,
    suffix: u16,
) -> Result<String, MarkdownWorkspaceError> {
    if suffix == 1 {
        // The first candidate retains the exact validated source filename.
        return Ok(source_name.to_owned());
    }
    let source_path = Path::new(source_name);
    let stem = source_path
        .file_stem()
        .and_then(|stem| {
            // A public note or asset identifier requires a UTF-8 filename stem that can round-trip through the UI.
            stem.to_str()
        })
        .ok_or(MarkdownWorkspaceError::UnsupportedFileName)?;
    let extension = source_path.extension().and_then(|extension| {
        // Only a UTF-8 suffix can participate in supported note or asset type recognition.
        extension.to_str()
    });
    Ok(match extension {
        // A real extension stays after the numeric suffix so image and file-type detection still work.
        Some(extension) if !extension.is_empty() => format!("{stem} {suffix}.{extension}"),
        // An extensionless filename appends the suffix directly to its stem.
        _ => format!("{stem} {suffix}"),
    })
}

/// Copy the observed source extent through a fixed buffer and reject shrinkage or growth.
pub(super) fn copy_attachment_bytes(
    source: &mut File,
    destination: &mut File,
    expected_byte_count: u64,
) -> Result<(), MarkdownWorkspaceError> {
    let mut buffer = [0_u8; ATTACHMENT_COPY_BUFFER_BYTES];
    let mut remaining = expected_byte_count;
    while remaining > 0 {
        // Each read is capped by both remaining observed bytes and the fixed copy buffer.
        let requested = remaining.min(buffer.len() as u64) as usize;
        let read = source.read(&mut buffer[..requested]).map_err(|_| {
            // A failed external-source read or lookup must not be reported as a successfully imported file.
            MarkdownWorkspaceError::ImportSourceUnavailable
        })?;
        if read == 0 {
            // An early EOF means the source shrank; the caller's pending guard removes partial output.
            return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
        }
        destination.write_all(&buffer[..read]).map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        remaining -= read as u64;
    }

    let mut trailing_byte = [0_u8; 1];
    if source.read(&mut trailing_byte).map_err(|_| {
        // A failed external-source read or lookup must not be reported as a successfully imported file.
        MarkdownWorkspaceError::ImportSourceUnavailable
    })? != 0
    {
        // A byte beyond the observed extent means the source grew during import and cannot be accepted.
        return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
    }
    Ok(())
}

/// Build safe relative Markdown for an imported asset without embedding a filesystem-absolute path.
pub(super) fn markdown_attachment_insertion(
    note_relative_path: &str,
    attachment_name: &str,
) -> Result<String, MarkdownWorkspaceError> {
    let mut destination = markdown_attachment_relative_prefix(note_relative_path)?;
    destination.push_str(&encode_markdown_destination_segment(attachment_name));

    let label = escape_markdown_link_label(attachment_name);
    let image_prefix = if is_markdown_image_attachment(attachment_name) {
        // A recognized image extension uses the Markdown image prefix.
        "!"
    } else {
        // Other attachments use an ordinary clickable Markdown link.
        ""
    };
    Ok(format!("{image_prefix}[{label}]({destination})"))
}

/// Build a relative path from a note parent back to the root asset directory.
pub(super) fn markdown_attachment_relative_prefix(
    note_relative_path: &str,
) -> Result<String, MarkdownWorkspaceError> {
    let note_path = validate_relative_path(note_relative_path, false)?;
    let note_parent = note_path
        .parent()
        .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
    let mut destination = String::new();
    for _ in note_parent.components() {
        // Each note-parent component requires one upward step before entering root asset storage.
        destination.push_str("../");
    }
    destination.push_str(ATTACHMENTS_DIRECTORY_NAME);
    destination.push('/');
    Ok(destination)
}

/// Rewrite recognized imported inline destinations while preserving code, escapes, and unrelated links.
#[cfg(test)]
pub(super) fn rewrite_inline_attachment_destinations(
    content: &str,
    source_prefix: &str,
    destination_prefix: &str,
) -> Option<String> {
    let bytes = content.as_bytes();
    let source_prefix_bytes = source_prefix.as_bytes();
    let mut rewritten = None::<String>;
    let mut copied_until = 0;
    let mut line_start = 0;
    let mut inline_code_ticks = None::<usize>;
    let mut fenced_code = None::<(u8, usize)>;

    while line_start < bytes.len() {
        // Process complete source lines while carrying fence and inline-code state across them.
        let line_end = bytes[line_start..]
            .iter()
            .position(|byte| {
                // Find the next source line boundary without decoding unrelated UTF-8 content.
                *byte == b'\n'
            })
            .map_or(bytes.len(), |offset| {
                // Translate the line-local newline position into the current document byte coordinate.
                line_start + offset
            });

        if inline_code_ticks.is_none()
            && let Some((marker, marker_count, marker_end)) =
                markdown_fence_marker(bytes, line_start, line_end)
        {
            // A fence marker outside inline code may open or close an opaque fenced region.
            if let Some((opening_marker, opening_count)) = fenced_code {
                // Inside an existing fence, only a compatible closing run ends the opaque region.
                if marker == opening_marker
                    && marker_count >= opening_count
                    && bytes[marker_end..line_end].iter().all(|byte| {
                        // A closing fence suffix must not contain non-whitespace content.
                        byte.is_ascii_whitespace()
                    })
                {
                    // A matching marker with sufficient length and no non-whitespace suffix closes the fence.
                    fenced_code = None;
                }
                line_start = line_end.saturating_add(1);
                continue;
            }
            fenced_code = Some((marker, marker_count));
            line_start = line_end.saturating_add(1);
            continue;
        }
        if fenced_code.is_some() {
            // Opaque fenced content remains byte-for-byte untouched.
            line_start = line_end.saturating_add(1);
            continue;
        }

        let mut index = line_start;
        while index < line_end {
            // Scan each nonfenced line once; every path advances the byte cursor.
            match bytes[index] {
                // An escape outside code prevents the next byte from becoming link or code syntax.
                b'\\' if inline_code_ticks.is_none() => {
                    index = (index + 2).min(line_end);
                }
                // A backtick run changes inline-code state only when its full delimiter length matches.
                b'`' => {
                    let tick_count = repeated_ascii_byte_count(bytes, index, line_end, b'`');
                    match inline_code_ticks {
                        // A run matching the opening delimiter ends inline code.
                        Some(opening_count) if opening_count == tick_count => {
                            inline_code_ticks = None;
                        }
                        // Outside code, remember this run as the required closing length.
                        None => inline_code_ticks = Some(tick_count),
                        // A different-length run inside code is literal content and preserves the opening delimiter.
                        _ => {}
                    }
                    index += tick_count;
                }
                // Only a link destination with the exact imported prefix is eligible for rewriting.
                b']' if inline_code_ticks.is_none()
                    && bytes.get(index + 1) == Some(&b'(')
                    && bytes[index + 2..].starts_with(source_prefix_bytes) =>
                {
                    let destination_start = index + 2;
                    let attachment_name_start = destination_start + source_prefix_bytes.len();
                    if is_encoded_attachment_destination(bytes, attachment_name_start, line_end) {
                        // A safe encoded filename followed by a closing parenthesis identifies a complete imported destination.
                        let output = rewritten.get_or_insert_with(|| {
                            // Allocate rewritten output lazily only after finding the first eligible imported destination.
                            String::with_capacity(
                                content.len()
                                    + destination_prefix.len().saturating_sub(source_prefix.len()),
                            )
                        });
                        output.push_str(&content[copied_until..destination_start]);
                        output.push_str(destination_prefix);
                        copied_until = attachment_name_start;
                        index = attachment_name_start;
                    } else {
                        // A malformed or unrelated destination remains unchanged while scanning continues.
                        index += 1;
                    }
                }
                // Other bytes preserve their source meaning and advance normally.
                _ => index += 1,
            }
        }
        line_start = line_end.saturating_add(1);
    }

    rewritten.map(|mut output| {
        // Complete the copied suffix only when at least one real source rewrite occurred.
        output.push_str(&content[copied_until..]);
        output
    })
}

/// Recognize a possible fence run with Markdown's maximum three-space indentation.
#[cfg(test)]
pub(super) fn markdown_fence_marker(
    bytes: &[u8],
    line_start: usize,
    line_end: usize,
) -> Option<(u8, usize, usize)> {
    let mut marker_start = line_start;
    while marker_start < line_end && marker_start - line_start < 4 && bytes[marker_start] == b' ' {
        // Inspect at most four spaces so excess indentation is rejected without scanning the whole line.
        marker_start += 1;
    }
    if marker_start - line_start > 3 {
        // Four leading spaces indicate ordinary indented content rather than a fence marker.
        return None;
    }
    let marker = *bytes.get(marker_start)?;
    if !matches!(marker, b'`' | b'~') {
        // Only backticks and tildes can participate in the fence state machine.
        return None;
    }
    let marker_count = repeated_ascii_byte_count(bytes, marker_start, line_end, marker);
    (marker_count >= 3).then_some((marker, marker_count, marker_start + marker_count))
}

/// Count one contiguous ASCII delimiter run within the supplied line bounds.
#[cfg(test)]
pub(super) fn repeated_ascii_byte_count(
    bytes: &[u8],
    start: usize,
    end: usize,
    expected: u8,
) -> usize {
    bytes[start..end]
        .iter()
        .take_while(|byte| {
            // The delimiter run ends at the first byte different from the expected ASCII marker.
            **byte == expected
        })
        .count()
}

/// Accept only a nonempty encoded filename segment immediately followed by the link terminator.
#[cfg(test)]
pub(super) fn is_encoded_attachment_destination(
    bytes: &[u8],
    start: usize,
    line_end: usize,
) -> bool {
    let mut index = start;
    while index < line_end
        && (bytes[index].is_ascii_alphanumeric()
            || matches!(bytes[index], b'%' | b'-' | b'.' | b'_' | b'~'))
    {
        // Only URL-safe ASCII and percent-encoded bytes may belong to a generated asset destination.
        index += 1;
    }
    index > start && bytes.get(index) == Some(&b')')
}

/// Percent-encode a single filename segment so Markdown punctuation cannot escape the destination.
pub(super) fn encode_markdown_destination_segment(file_name: &str) -> String {
    let mut encoded = String::with_capacity(file_name.len());
    for byte in file_name.as_bytes() {
        // Encode UTF-8 bytes rather than scalar values to preserve standard percent-encoded filename semantics.
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            // Unreserved ASCII bytes can appear unchanged inside one destination segment.
            encoded.push(char::from(*byte));
        } else {
            // All other bytes are encoded as uppercase hexadecimal escapes.
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// Escape label punctuation while preserving the filename's Unicode display text.
pub(super) fn escape_markdown_link_label(file_name: &str) -> String {
    let mut escaped = String::with_capacity(file_name.len());
    for character in file_name.chars() {
        // Each scalar is copied exactly once after any required Markdown escape.
        if matches!(character, '\\' | '[' | ']') {
            // Backslashes and square brackets need an escape to remain literal label text.
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Classify the maintained image-extension allowlist without inspecting or decoding asset contents.
pub(super) fn is_markdown_image_attachment(file_name: &str) -> bool {
    file_name
        .rsplit_once('.')
        .map(|(_, extension)| {
            // Type classification consumes only the final extension, leaving the original filename intact.
            extension
        })
        .is_some_and(|extension| {
            // Use the maintained case-insensitive image suffix allowlist without opening or decoding asset contents.
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "avif"
                    | "bmp"
                    | "gif"
                    | "heic"
                    | "heif"
                    | "jpeg"
                    | "jpg"
                    | "png"
                    | "svg"
                    | "tif"
                    | "tiff"
                    | "webp"
            )
        })
}
