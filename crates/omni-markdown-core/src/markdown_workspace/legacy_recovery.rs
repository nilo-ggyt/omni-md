//! Recovers pending records through bounded mappings and immutable content-bound
//! resolution markers. Existing `.draft` files are never deleted or moved.

use super::*;
use crate::immutable_snapshot::snapshot_canonical;
use crate::markdown_document_buffer::MarkdownDocumentRevision;

/// Validates the frozen record's header and exact embedded path without
/// borrowing its complete text payload as one slice.
fn bounded_conflict_payload_range(
    snapshot: &crate::original_backing::OriginalBacking,
    expected: &str,
) -> Result<std::ops::Range<usize>, MarkdownWorkspaceError> {
    if snapshot.len() < CONFLICT_DRAFT_HEADER_BYTES {
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }
    let payload = snapshot
        .with_bytes(0..CONFLICT_DRAFT_HEADER_BYTES, |header| {
            conflict_draft_payload_extent(header, snapshot.len())
        })
        .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)??;
    if payload.start - CONFLICT_DRAFT_HEADER_BYTES != expected.len() {
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }
    let mut at = 0;
    while at < expected.len() {
        let end = at
            .saturating_add(crate::original_backing::ORIGINAL_READ_BYTES)
            .min(expected.len());
        let equal = snapshot
            .with_bytes(
                CONFLICT_DRAFT_HEADER_BYTES + at..CONFLICT_DRAFT_HEADER_BYTES + end,
                |bytes| bytes == &expected.as_bytes()[at..end],
            )
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        if !equal {
            return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
        }
        at = end;
    }
    Ok(payload)
}

const RESOLUTION_MAGIC: &[u8; 8] = b"OMNIMR1\0";
const RESOLUTION_HEADER_BYTES: usize = 89;
const RESOLUTION_CHECKSUM_BYTES: usize = 32;
const MAX_RESOLUTION_BYTES: usize = 16 * 1_024;

#[derive(Clone, Debug, Eq, PartialEq)]
/// Marker durability is separate from an already verified explicit handoff. A
/// recording failure cannot trap the user after choosing reload or save-as-new.
pub enum MarkdownRecoveryRecording {
    /// This decision, or an identical existing receipt, has confirmed durability.
    Recorded,
    /// Another complete valid decision exists for this record and is not overwritten.
    AlreadyResolved,
    /// Recording is incomplete. Preserve the record and existing marker; a normal
    /// reopen may present recovery again.
    NotRecorded(MarkdownWorkspaceError),
}

/// Prepares a small ticket while the original editor is active, without publishing
/// a marker or creating a replacement document buffer.
pub enum MarkdownRecoveryPreparation {
    /// An ordinary document has no pending record and creates no recovery storage.
    NotPending,
    /// The data decision is verified. The caller must still revalidate its binding
    /// and pass the existing strict barrier before releasing the original editor.
    Prepared(MarkdownRecoveryHandoff),
}

/// The sole target opened after the strict barrier, with independent recording
/// facts. A bad marker cannot conceal an already verified explicit handoff.
pub struct MarkdownRecoveryResolutionOutcome {
    /// Ordinary target-disk model, retaining neither the old editor/history nor
    /// another long-lived owner.
    pub document: MarkdownDocumentBuffer,
    /// Recording failure must be surfaced separately, never reported as a fully
    /// persisted decision.
    pub recording: MarkdownRecoveryRecording,
}

/// Linear ticket created only by a verified explicit decision. Its constructor
/// is private, it is not cloneable, and it retains no text, descriptor, or mapping.
pub struct MarkdownRecoveryHandoff {
    canonical_root: PathBuf,
    source_relative_path: String,
    record_digest: [u8; 32],
    source_revision: MarkdownDocumentRevision,
    target_relative_path: String,
    target_revision: [u8; 32],
    target_byte_count: u64,
    resolution_bytes: Vec<u8>,
}

impl MarkdownRecoveryHandoff {
    /// Returns the relative identity of this handoff's sole target; callers cannot
    /// use this accessor to substitute another destination.
    pub fn target_relative_path(&self) -> &str {
        &self.target_relative_path
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Immutable-marker publication facts, separate from the one-shot data handoff.
enum MarkerPublication {
    /// The complete marker was published and directory synchronization finished.
    Written,
    /// The identical receipt exists, and this operation reconfirmed directory durability.
    Idempotent,
    /// Another valid decision exists for this record and is never overwritten.
    AlreadyResolved,
}

/// Markers contain bounded metadata only. Revision and byte count come from
/// verified disk contents or a verified saved copy.
struct ResolutionReceipt<'a> {
    kind: u8,
    target_path: &'a str,
    revision: [u8; 32],
    byte_count: u64,
}

impl MarkdownSessionAccess {
    /// Looks up one exact pending record. Invalid markers cannot suppress drafts;
    /// source bytes are borrowed only from a separate immutable snapshot.
    pub(super) fn open_pending_legacy_recovery(
        &self,
        note_path: &Path,
        relative_path: &str,
    ) -> Result<Option<MarkdownDocumentBuffer>, MarkdownWorkspaceError> {
        validate_conflict_draft_relative_path(relative_path)?;
        if !existing_conflict_recovery_directory(&self.conflict_recovery_directory)? {
            // Ordinary opens with no record create no recovery directory, marker, or
            // long-lived recovery owner.
            return Ok(None);
        }
        let record_path = conflict_draft_path(&self.conflict_recovery_directory, relative_path);
        let metadata = match fs::symlink_metadata(&record_path) {
            // An exact filename is not content authority; validate its private
            // regular-file identity before trusting any record bytes.
            Ok(metadata) => metadata,
            // Missing this note's record means a normal disk open, not a directory scan.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // Permission or I/O failure cannot masquerade as absence of recoverable data.
            Err(_) => return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
        };
        validate_conflict_draft_file(&metadata)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&record_path)
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        let before = file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        validate_conflict_draft_file(&before)?;
        let extent = usize::try_from(before.len())
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        let mut header = [0_u8; CONFLICT_DRAFT_HEADER_BYTES];
        file.read_exact(&mut header)
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        // This 20-byte header rejects invalid lengths and excess size only. It does
        // not grant a slice over the original inode, which may still be truncated.
        conflict_draft_payload_extent(&header, extent)?;
        let identity = FileIdentity::from(&before);
        let snapshot = snapshot_canonical(&file, before.len(), || {
            // Synchronous native open has no separate worker. The snapshot factory
            // still uses a fixed 64-KiB buffer for a cross-volume copy fallback.
            true
        })
        .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?
        .ok_or(MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        // Revalidate the frozen header and exact path. The earlier mutable header
        // limits admission only; it cannot authorize interpreting different bytes.
        let payload = bounded_conflict_payload_range(&snapshot, relative_path)?;
        let mut hasher = Sha256::new();
        let mut at = 0;
        while at < snapshot.len() {
            // The record digest covers every exact header, path, and payload byte.
            // Advance only after each bounded lease completes real cleanup.
            let end = at
                .saturating_add(crate::original_backing::ORIGINAL_READ_BYTES)
                .min(snapshot.len());
            snapshot
                .with_bytes(at..end, |bytes| hasher.update(bytes))
                .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
            at = end;
        }
        let digest: [u8; 32] = hasher.finalize().into();
        let after = file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        let path_after = fs::symlink_metadata(&record_path)
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        if FileIdentity::from(&after) != identity
            || FileIdentity::from(&path_after) != identity
            || path_after.file_type().is_symlink()
        {
            // A modified or replaced source releases local resources and cannot
            // publish a partial model belonging to another revision.
            return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
        }
        drop(file);
        if read_resolution_marker(&self.conflict_recovery_directory, relative_path, &digest)
            .is_ok_and(|marker| marker.is_some())
        {
            // This complete content identity already has a valid decision. Preserve
            // the original record and release its mapping before returning.
            snapshot
                .close()
                .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
            return Ok(None);
        }
        // Missing or corrupt markers keep recovery pending. The model validates
        // payload UTF-8 and builds its piece index through bounded leases only.
        MarkdownDocumentBuffer::from_legacy_recovery(
            note_path,
            snapshot,
            payload,
            digest,
            MAX_DOCUMENT_BYTES,
        )
        .map(Some)
        .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)
    }

    /// After explicit reload intent, verifies current disk bytes and prepares a
    /// bounded ticket without publishing a marker or replacement model.
    ///
    /// # Errors
    /// Unverifiable disk content, an unconfined path, or invalid receipt admission
    /// preserves the current model and all files.
    /// Before releasing the old editor, revalidate and use the existing before/after
    /// resign generation checks to reject late-arriving input.
    pub fn prepare_legacy_recovery_after_reload(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<MarkdownRecoveryPreparation, MarkdownWorkspaceError> {
        let Some(digest) = document.legacy_recovery_record_digest() else {
            // Ordinary reload creates no ticket, recovery metadata, or storage directory.
            return Ok(MarkdownRecoveryPreparation::NotPending);
        };
        let relative_path = relative_markdown_path(&self.canonical_root, document.path())?;
        let path = resolve_existing_markdown(&self.canonical_root, &relative_path)?;
        let disk = inspect_markdown_save_baseline(&path)?;
        let receipt = ResolutionReceipt {
            kind: 1,
            target_path: "",
            revision: decode_revision(&disk.revision)?,
            byte_count: disk.byte_count,
        };
        // This is verified current disk content, not the missing historical baseline.
        // The pending model remains barred from ordinary overwrite saves.
        self.prepare_recovery_handoff(document, &relative_path, &digest, &relative_path, &receipt)
    }

    /// Verifies a saved copy against live pieces and prepares only a ticket. If old
    /// editor release fails, discarding the ticket has no marker side effects.
    ///
    /// # Errors
    /// Receipt/document/copy mismatch, an unconfined path, or failed copy durability
    /// returns an error while preserving the current model.
    /// Even an existing valid marker does not replace proof that this live draft has
    /// a durable copy. This operation never grants authority to overwrite the original.
    pub fn prepare_legacy_recovery_after_save_as_new(
        &self,
        document: &MarkdownDocumentBuffer,
        saved: &MarkdownNoteMutationReceipt,
    ) -> Result<MarkdownRecoveryPreparation, MarkdownWorkspaceError> {
        let Some(digest) = document.legacy_recovery_record_digest() else {
            // Ordinary save-as uses its existing API without recovery tickets or markers.
            return Ok(MarkdownRecoveryPreparation::NotPending);
        };
        let relative_path = relative_markdown_path(&self.canonical_root, document.path())?;
        validate_conflict_draft_relative_path(&saved.relative_path)?;
        let target = resolve_existing_markdown(&self.canonical_root, &saved.relative_path)?;
        let actual = inspect_markdown_save_baseline(&target)?;
        if target == document.path()
            || actual.revision != saved.revision
            || actual.byte_count != saved.byte_count
            || saved.revision
                != document
                    .current_revision()
                    .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?
                    .encode_hex()
            || saved.byte_count != document.byte_len() as u64
        {
            // An existing file without the current live pieces cannot prove save-as
            // success and must not create a handoff ticket.
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        // Save-as receipts follow file/directory fsync. Reconfirm target durability
        // so an invented receipt cannot borrow authority from an unsynced file.
        let target_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&target)
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        let target_before = target_file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        if !target_before.is_file() {
            // A FIFO/device substitution cannot cause blocking open or acquire the
            // authority of a durable regular-file copy.
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        let synced_identity = FileIdentity::from(&target_before);
        target_file
            .sync_all()
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        let parent = target
            .parent()
            .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
        sync_directory(parent)?;
        let rechecked = inspect_markdown_save_baseline(&target)?;
        let target_after = target_file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        let path_after = fs::symlink_metadata(&target)
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        if rechecked.revision != actual.revision
            || rechecked.byte_count != actual.byte_count
            || FileIdentity::from(&target_after) != synced_identity
            || FileIdentity::from(&path_after) != synced_identity
            || path_after.file_type().is_symlink()
        {
            // An identical replacement inode may not be the descriptor just synced.
            // Any observed replacement or mutation during sync rejects the ticket.
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        let receipt = ResolutionReceipt {
            kind: 2,
            target_path: &saved.relative_path,
            revision: decode_revision(&saved.revision)?,
            byte_count: saved.byte_count,
        };
        self.prepare_recovery_handoff(
            document,
            &relative_path,
            &digest,
            &saved.relative_path,
            &receipt,
        )
    }

    /// Validate the ticket binding while the original pending model lives, without
    /// reopening its path, acquiring a new mapping, or publishing a marker.
    /// A current-revision cache miss may stream existing mapped pieces through
    /// their original bounded read leases before rejecting a stale ticket.
    ///
    /// # Errors
    /// A mismatched session root, source path, record identity, or live revision
    /// makes the caller retain the original editor and history.
    /// Success does not replace native generation checks around resign. Later input
    /// requires refusing editor release and discarding this ticket.
    pub fn validate_legacy_recovery_handoff(
        &self,
        document: &MarkdownDocumentBuffer,
        handoff: &MarkdownRecoveryHandoff,
    ) -> Result<(), MarkdownWorkspaceError> {
        if self.canonical_root != handoff.canonical_root
            || document.path().strip_prefix(&handoff.canonical_root).ok()
                != Some(Path::new(&handoff.source_relative_path))
            || document.legacy_recovery_record_digest() != Some(handoff.record_digest)
            || document
                .current_revision()
                .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?
                != handoff.source_revision
        {
            // Later input, another vault, or another record model cannot reuse this
            // decision. Every persistent file remains untouched.
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        Ok(())
    }

    /// Consumes the ticket only after the old editor passes its strict barrier.
    /// Open the sole target before attempting marker publication.
    ///
    /// # Errors
    /// Root/target-version mismatch or unsafe reads return an error without a marker;
    /// a normal open can recover the original record again.
    /// Marker failure yields `NotRecorded` and preserves complete files while the
    /// verified target can still be handed off. The caller must accurately warn
    /// about recording failure. This is no general recovery bypass and must not run
    /// after failed release of the original editor.
    pub fn open_resolved_legacy_recovery(
        &self,
        handoff: MarkdownRecoveryHandoff,
    ) -> Result<MarkdownRecoveryResolutionOutcome, MarkdownWorkspaceError> {
        self.open_recovery_handoff_with_sync(handoff, sync_conflict_recovery_directory)
    }

    /// The private durability operation runs only after opening the actual target
    /// and publishing a complete marker, enabling real filesystem failure regression.
    fn open_recovery_handoff_with_sync(
        &self,
        handoff: MarkdownRecoveryHandoff,
        synchronize: impl FnOnce(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownRecoveryResolutionOutcome, MarkdownWorkspaceError> {
        if self.canonical_root != handoff.canonical_root {
            // Another vault cannot consume this ticket; touch neither target nor marker.
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        let target =
            resolve_existing_markdown(&self.canonical_root, &handoff.target_relative_path)?;
        // Construct the replacement only after old-editor release. Ordinary
        // record-aware open still offers no bypass parameter.
        let document = MarkdownDocumentBuffer::open(&target, MAX_DOCUMENT_BYTES)
            .map_err(markdown_document_buffer_open_error)?;
        let revision = document.original_revision().and_then(|revision| {
            // Convert only the fixed 64-byte digest representation, never document
            // text. A normal disk model must carry a real baseline.
            decode_revision(&revision.encode_hex()).ok()
        });
        if revision != Some(handoff.target_revision)
            || document.byte_len() as u64 != handoff.target_byte_count
        {
            // A changed target releases its local mapping without publishing a marker.
            // Preserve both the original record and the previously saved copy.
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        let recording = match publish_resolution_marker(
            &self.conflict_recovery_directory,
            &handoff.source_relative_path,
            &handoff.record_digest,
            &handoff.resolution_bytes,
            &self.save_sequence,
            synchronize,
        ) {
            // Newly published and identical receipts both confirm directory durability
            // and share the same accurate UI recording fact.
            Ok(MarkerPublication::Written | MarkerPublication::Idempotent) => {
                MarkdownRecoveryRecording::Recorded
            }
            // Preserve another valid decision; do not claim this receipt replaced it.
            Ok(MarkerPublication::AlreadyResolved) => MarkdownRecoveryRecording::AlreadyResolved,
            // Preserve typed corrupt-marker or post-rename durability uncertainty
            // together with every complete persistent file.
            Err(error) => MarkdownRecoveryRecording::NotRecorded(error),
        };
        Ok(MarkdownRecoveryResolutionOutcome {
            document,
            recording,
        })
    }

    /// Stores bounded receipt metadata only. No text, descriptor, or mapping is
    /// retained, and discarding the ticket has no disk side effects.
    fn prepare_recovery_handoff(
        &self,
        document: &MarkdownDocumentBuffer,
        relative_path: &str,
        digest: &[u8; 32],
        target_relative_path: &str,
        receipt: &ResolutionReceipt<'_>,
    ) -> Result<MarkdownRecoveryPreparation, MarkdownWorkspaceError> {
        let resolution_bytes = encode_resolution(relative_path, digest, receipt)?;
        Ok(MarkdownRecoveryPreparation::Prepared(
            MarkdownRecoveryHandoff {
                canonical_root: self.canonical_root.clone(),
                source_relative_path: relative_path.to_owned(),
                record_digest: *digest,
                source_revision: document
                    .current_revision()
                    .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?,
                target_relative_path: target_relative_path.to_owned(),
                target_revision: receipt.revision,
                target_byte_count: receipt.byte_count,
                resolution_bytes,
            },
        ))
    }
}

/// Decodes exactly 64 lowercase hexadecimal characters into a fixed digest,
/// rejecting arbitrary lengths or ambiguous encodings.
fn decode_revision(revision: &str) -> Result<[u8; 32], MarkdownWorkspaceError> {
    if revision.len() != 64
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        // An external receipt must match this project's canonical revision encoding.
        return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in revision.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        // Length and ASCII domain are verified. Each pair yields one byte without
        // allocating an intermediate string.
        let nibble = |byte: u8| {
            if byte <= b'9' {
                // Verified ASCII decimal digits map directly to their low nibble.
                byte - b'0'
            } else {
                // Only verified lowercase a-f remain; uppercase and other encodings fail
                // the earlier canonical-domain check.
                byte - b'a' + 10
            }
        };
        digest[index] = nibble(pair[0]) * 16 + nibble(pair[1]);
    }
    Ok(digest)
}

/// Domain-separates names using the path and complete record digest, so a later
/// different record cannot inherit an earlier decision.
fn marker_path(directory: &Path, relative_path: &str, digest: &[u8; 32]) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"omni-md-recovery-resolution/v1\0");
    hasher.update((relative_path.len() as u64).to_be_bytes());
    hasher.update(relative_path.as_bytes());
    hasher.update(digest);
    let key = hasher.finalize();
    let mut name = String::with_capacity(64 + ".resolved".len());
    for byte in key {
        // Fixed ASCII hex has no directory separators, preventing a relative
        // source path from injecting components into the marker filename.
        use std::fmt::Write as _;
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".resolved");
    directory.join(name)
}

/// Encodes a fully bounded marker with filename-independent content validation.
/// No document text bytes enter this metadata.
fn encode_resolution(
    relative_path: &str,
    digest: &[u8; 32],
    receipt: &ResolutionReceipt<'_>,
) -> Result<Vec<u8>, MarkdownWorkspaceError> {
    validate_conflict_draft_relative_path(relative_path)?;
    if receipt.kind == 2 {
        // Save-as targets must also be valid bounded note identities within this vault.
        validate_conflict_draft_relative_path(receipt.target_path)?;
    } else if receipt.kind != 1 || !receipt.target_path.is_empty() {
        // Reload carries no alternate target. Unknown kinds cannot be encoded
        // into a decision that readers might accept.
        return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
    }
    let length = RESOLUTION_HEADER_BYTES
        + relative_path.len()
        + receipt.target_path.len()
        + RESOLUTION_CHECKSUM_BYTES;
    if length > MAX_RESOLUTION_BYTES || receipt.byte_count > MAX_DOCUMENT_BYTES {
        // Metadata and byte counts have explicit budgets. Vector growth or
        // truncation cannot redefine what decision was persisted.
        return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
    }
    let mut bytes = Vec::with_capacity(length);
    bytes.extend_from_slice(RESOLUTION_MAGIC);
    bytes.extend_from_slice(&(relative_path.len() as u32).to_be_bytes());
    bytes.extend_from_slice(digest);
    bytes.push(receipt.kind);
    bytes.extend_from_slice(&(receipt.target_path.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&receipt.revision);
    bytes.extend_from_slice(&receipt.byte_count.to_be_bytes());
    bytes.extend_from_slice(relative_path.as_bytes());
    bytes.extend_from_slice(receipt.target_path.as_bytes());
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    Ok(bytes)
}

/// Invalid format, binding, or trailing bytes cannot suppress the original record.
/// Only complete valid metadata has recovery-resolution authority.
fn validate_resolution(
    bytes: &[u8],
    relative_path: &str,
    digest: &[u8; 32],
) -> Result<(), MarkdownWorkspaceError> {
    let corrupt = MarkdownWorkspaceError::RecoveryResolutionCorrupted;
    if bytes.len() < RESOLUTION_HEADER_BYTES + RESOLUTION_CHECKSUM_BYTES
        || bytes.len() > MAX_RESOLUTION_BYTES
        || &bytes[..8] != RESOLUTION_MAGIC
    {
        // Verify the minimal fixed header before any later fixed-offset reads.
        return Err(corrupt);
    }
    let mut original_length = [0; 4];
    original_length.copy_from_slice(&bytes[8..12]);
    let original_length = u32::from_be_bytes(original_length) as usize;
    let mut target_length = [0; 4];
    target_length.copy_from_slice(&bytes[45..49]);
    let target_length = u32::from_be_bytes(target_length) as usize;
    let mut byte_count = [0; 8];
    byte_count.copy_from_slice(&bytes[81..89]);
    if original_length > MAX_RECOVERY_RELATIVE_PATH_BYTES
        || target_length > MAX_RECOVERY_RELATIVE_PATH_BYTES
        || u64::from_be_bytes(byte_count) > MAX_DOCUMENT_BYTES
        || RESOLUTION_HEADER_BYTES + original_length + target_length + RESOLUTION_CHECKSUM_BYTES
            != bytes.len()
    {
        // Bounded path lengths make the arithmetic finite. The entire file must
        // exactly cover the header, both paths, and checksum.
        return Err(corrupt);
    }
    let original_end = RESOLUTION_HEADER_BYTES + original_length;
    let target_end = original_end + target_length;
    if &bytes[12..44] != digest
        || &bytes[RESOLUTION_HEADER_BYTES..original_end] != relative_path.as_bytes()
        || bytes[target_end..] != Sha256::digest(&bytes[..target_end])[..]
    {
        // A hashed filename cannot replace complete binding and checksum checks.
        // Preserve invalid decisions as untrusted files rather than overwriting them.
        return Err(corrupt);
    }
    match bytes[44] {
        // Reload requires an empty target; revision and size describe disk facts
        // verified when the explicit choice was made.
        1 if target_length == 0 => Ok(()),
        2 => {
            // Save-as targets require UTF-8 and confined relative-note syntax.
            // Parsing neither opens nor rewrites the target path.
            let target = std::str::from_utf8(&bytes[original_end..target_end])
                .map_err(|_| corrupt.clone())?;
            validate_conflict_draft_relative_path(target).map_err(|_| corrupt)
        }
        // Unknown kinds cannot degrade into reload or an already-resolved decision.
        _ => Err(corrupt),
    }
}

/// Reads a complete marker within 16 KiB. Identity rechecks reject same-user
/// modification/replacement; a read failure grants no resolution authority.
fn read_resolution_marker(
    directory: &Path,
    relative_path: &str,
    digest: &[u8; 32],
) -> Result<Option<Vec<u8>>, MarkdownWorkspaceError> {
    let path = marker_path(directory, relative_path, digest);
    let metadata = match fs::symlink_metadata(&path) {
        // A present path is not yet a trusted private marker. Verify its real
        // descriptor and complete content below.
        Ok(metadata) => metadata,
        // Absence is an ordinary pending-recovery state.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // A lookup failure cannot become an accepted decision.
        Err(_) => return Err(MarkdownWorkspaceError::RecoveryResolutionCorrupted),
    };
    validate_conflict_draft_file(&metadata)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    let before = file
        .metadata()
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    validate_conflict_draft_file(&before)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    if before.len() > MAX_RESOLUTION_BYTES as u64 {
        // An oversized file cannot dictate the memory allocation size.
        return Err(MarkdownWorkspaceError::RecoveryResolutionCorrupted);
    }
    // A fixed buffer plus one growth sentinel prevents a growing file from
    // triggering automatic vector-capacity doubling.
    let mut bytes = vec![0_u8; MAX_RESOLUTION_BYTES + 1];
    let mut filled = 0;
    while filled < bytes.len() {
        // Every successful read advances within the fixed extent. EOF ends normal
        // files; at most 16 KiB plus one rejection sentinel is observed.
        let count = file
            .read(&mut bytes[filled..])
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
        if count == 0 {
            // Do not invent zero-filled marker bytes after EOF. Complete format
            // validation receives only the bytes actually read.
            break;
        }
        filled += count;
    }
    bytes.truncate(filled);
    let after = file
        .metadata()
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    let path_after = fs::symlink_metadata(&path)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    if FileIdentity::from(&before) != FileIdentity::from(&after)
        || FileIdentity::from(&before) != FileIdentity::from(&path_after)
        || path_after.file_type().is_symlink()
    {
        // Reject a marker changed during reading; the original record remains pending.
        return Err(MarkdownWorkspaceError::RecoveryResolutionCorrupted);
    }
    validate_resolution(&bytes, relative_path, digest)?;
    Ok(Some(bytes))
}

/// Fsyncs a complete temporary before exclusive publication. The final name
/// never exposes partial bytes, and failures after publication never roll it back.
fn publish_resolution_marker(
    directory: &Path,
    relative_path: &str,
    digest: &[u8; 32],
    bytes: &[u8],
    sequence: &AtomicU64,
    synchronize: impl FnOnce(&Path) -> Result<(), MarkdownWorkspaceError>,
) -> Result<MarkerPublication, MarkdownWorkspaceError> {
    ensure_conflict_recovery_directory(directory)?;
    let mut temporary = create_temporary_conflict_draft(directory, sequence)?;
    temporary
        .file
        .write_all(bytes)
        .and_then(|()| {
            // Only a completely written and synced bounded marker may become a
            // visible final decision.
            temporary.file.sync_all()
        })
        .map_err(|_| MarkdownWorkspaceError::ConflictRecoveryUnavailable)?;
    let destination = marker_path(directory, relative_path, digest);
    let outcome = match rename_without_replacement(&temporary.path, &destination) {
        Ok(()) => {
            // Disarm temporary rollback first. A later directory-sync failure must
            // preserve the already published complete marker.
            temporary.disarm();
            MarkerPublication::Written
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = read_resolution_marker(directory, relative_path, digest)?
                .ok_or(MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
            if existing == bytes {
                // An identical complete receipt is idempotent. Reconfirm directory
                // durability below to resolve an earlier uncertain publication.
                MarkerPublication::Idempotent
            } else {
                // Another valid decision for this record is neither corrupt nor
                // replaceable. The caller already verified its new draft copy.
                MarkerPublication::AlreadyResolved
            }
        }
        // Other publication errors leave this temporary guard to remove only its
        // own unpublished file, preserving records and other markers.
        Err(_) => return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
    };
    synchronize(directory)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionDurabilityUncertain)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests;
