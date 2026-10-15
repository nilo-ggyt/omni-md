//! Private immutable file snapshots for every borrowed UTF-8 mapping.
//!
//! User files remain mutable outside this process. A read-only or MAP_PRIVATE mapping
//! does not freeze their inode. Only a distinct, unlinked inode owned during acquisition
//! may back an immutable Rust slice. Source identity validation remains the caller's
//! optimistic concurrency check; it is separate from this module's memory-safety boundary.

use std::fs::File;
#[cfg(target_os = "macos")]
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(target_os = "macos")]
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;

use memmap2::{Mmap, MmapOptions};
use thiserror::Error;

const SNAPSHOT_COPY_BYTES: usize = 64 * 1_024;
// Darwin sys/clonefile.h defines this stable flag since macOS 10.12; libc exposes the call but not the flag.
#[cfg(target_os = "macos")]
const CLONE_NOOWNERCOPY: u32 = 0x0002;

/// Acquisition failures distinguish unstable source extent and cancellation from storage failure.
#[derive(Debug, Error)]
pub(crate) enum SnapshotError {
    #[error("source extent changed during snapshot acquisition")]
    SourceChanged,
    #[error("snapshot acquisition was cancelled")]
    Cancelled,
    #[error("snapshot storage unavailable: {0}")]
    Storage(io::Error),
}

/// Capture one already size-checked descriptor into immutable, unnamed backing storage.
///
/// The caller rechecks its source descriptor and path identity before publishing derived results.
/// Cancellation is observed before acquisition and between fallback-copy chunks. Kernel clone,
/// read, and write calls are synchronous; cancellation cannot interrupt a blocked filesystem call.
pub(crate) fn snapshot_file(
    source: &File,
    expected_bytes: u64,
    is_current: impl Fn() -> bool,
) -> Result<Option<Mmap>, SnapshotError> {
    // Production always attempts the clone fast path; tests can independently force streaming below.
    snapshot_file_inner(source, expected_bytes, &is_current, true, None)
}

/// Query-local private namespace reused only while one foreground operation is active.
///
/// Each captured inode is unlinked immediately. Reusing the empty parent avoids creating a new
/// directory and opening another directory descriptor for every note in a vault search. Drop
/// releases the descriptor and removes the empty namespace when that explicit query returns.
pub(crate) struct SnapshotDirectory {
    // Fields drop in declaration order: close the directory descriptor before removing its name.
    #[cfg(target_os = "macos")]
    descriptor: File,
    directory: tempfile::TempDir,
}

impl SnapshotDirectory {
    /// Acquire a unique owner-private system temporary directory without touching the user vault.
    pub(crate) fn new() -> Result<Self, SnapshotError> {
        // Ordinary callers never select a parent; tests use a local parent only to observe cleanup.
        Self::in_parent(None)
    }

    /// Create an isolated private parent and retain its directory descriptor for clone calls.
    fn in_parent(parent: Option<&Path>) -> Result<Self, SnapshotError> {
        let mut builder = tempfile::Builder::new();
        builder.prefix(".omni-md-snapshot-");
        let directory = match parent {
            // Tests can assert their own scratch parent becomes empty without scanning shared /tmp.
            Some(parent) => builder.tempdir_in(parent),
            // System scratch allows opening read-only vaults and uses clone only when volumes match.
            None => builder.tempdir(),
        }
        .map_err(SnapshotError::Storage)?;
        #[cfg(target_os = "macos")]
        let descriptor = File::open(directory.path()).map_err(SnapshotError::Storage)?;
        Ok(Self {
            #[cfg(target_os = "macos")]
            descriptor,
            directory,
        })
    }

    /// Freeze one source in this operation's namespace; returned maps never retain names or descriptors.
    pub(crate) fn capture(
        &mut self,
        source: &File,
        expected_bytes: u64,
        is_current: &impl Fn() -> bool,
    ) -> Result<Option<Mmap>, SnapshotError> {
        // Exclusive access serializes the fixed private clone name while maps remain independently owned.
        self.capture_with_strategy(source, expected_bytes, is_current, true)
    }

    /// Capture with an explicit strategy only so tests can force the unsupported-filesystem fallback.
    fn capture_with_strategy(
        &mut self,
        source: &File,
        expected_bytes: u64,
        is_current: &impl Fn() -> bool,
        allow_clone: bool,
    ) -> Result<Option<Mmap>, SnapshotError> {
        ensure_current(is_current)?;
        if expected_bytes == 0 {
            // Empty sources require no inode or mapping, even when a query already owns a scratch directory.
            return Ok(None);
        }
        let backing = if allow_clone {
            // Same-volume APFS clones share blocks while acquiring a distinct session-owned inode.
            match clone_unlinked_source(source, self)? {
                // The clone's name was removed before this descriptor became usable by a mapper.
                Some(file) => file,
                // Unsupported or cross-volume cloning uses the same private-inode contract through copying.
                None => stream_unlinked_source(
                    source,
                    self.directory.path(),
                    expected_bytes,
                    is_current,
                )?,
            }
        } else {
            // This private test seam verifies fallback cancellation, byte bounds, and cleanup independently.
            stream_unlinked_source(source, self.directory.path(), expected_bytes, is_current)?
        };
        ensure_current(is_current)?;
        let metadata = backing.metadata().map_err(SnapshotError::Storage)?;
        if !metadata.is_file() || metadata.nlink() != 0 {
            // Never lend immutable slices over a special file or an inode still reachable by a filesystem name.
            return Err(SnapshotError::Storage(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot backing must be an unlinked regular file",
            )));
        }
        if metadata.len() != expected_bytes {
            // A clone from a changed extent cannot satisfy the caller's observed source baseline.
            return Err(SnapshotError::SourceChanged);
        }
        let length = usize::try_from(expected_bytes).map_err(|_| {
            // Unaddressable source extents cannot form a Rust slice on this target.
            SnapshotError::Storage(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot too large",
            ))
        })?;
        // SAFETY: backing is a distinct regular inode created in our private directory and already
        // unlinked, verified above. No handle escapes acquisition. All fallback writes have finished,
        // and no later operation writes or truncates this inode. Source mutation affects another inode.
        // The checked extent equals the private inode length; Mmap owns the resulting VM lifetime.
        let mapping = unsafe { MmapOptions::new().len(length).map(&backing) }
            .map_err(SnapshotError::Storage)?;
        // The mapping alone now retains the unnamed inode. Parent namespace reuse cannot mutate it.
        drop(backing);
        Ok(Some(mapping))
    }
}

/// Own the complete private namespace for one standalone capture or forced-strategy regression.
fn snapshot_file_inner(
    source: &File,
    expected_bytes: u64,
    is_current: &impl Fn() -> bool,
    allow_clone: bool,
    scratch_parent: Option<&Path>,
) -> Result<Option<Mmap>, SnapshotError> {
    // Empty or already-cancelled calls never acquire temporary names or descriptors.
    ensure_current(is_current)?;
    if expected_bytes == 0 {
        // The caller still revalidates source identity, but zero bytes need no snapshot backing.
        return Ok(None);
    }
    let mut directory = SnapshotDirectory::in_parent(scratch_parent)?;
    directory.capture_with_strategy(source, expected_bytes, is_current, allow_clone)
}

/// Preserve cancellation as a separate outcome rather than a generic I/O failure.
fn ensure_current(is_current: &impl Fn() -> bool) -> Result<(), SnapshotError> {
    if is_current() {
        // The foreground owner still permits acquisition or the next bounded copy chunk.
        Ok(())
    } else {
        // Local guards release descriptors, mappings, and private names before cancellation returns.
        Err(SnapshotError::Cancelled)
    }
}

/// Clone the opened source descriptor without resolving its mutable source path a second time.
#[cfg(target_os = "macos")]
fn clone_unlinked_source(
    source: &File,
    directory: &SnapshotDirectory,
) -> Result<Option<File>, SnapshotError> {
    let name = c"snapshot";
    // SAFETY: both descriptors remain live throughout this synchronous call; name is a valid NUL-
    // terminated single component. The destination is within a unique directory owned by this call.
    // fclonefileat creates a distinct inode exclusively and does not transfer descriptor ownership.
    let status = unsafe {
        libc::fclonefileat(
            source.as_raw_fd(),
            directory.descriptor.as_raw_fd(),
            name.as_ptr(),
            CLONE_NOOWNERCOPY,
        )
    };
    if status != 0 {
        // Only an unavailable clone capability selects copying; permissions, capacity, and I/O errors
        // remain explicit failures instead of retrying a different operation against uncertain state.
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            // Different volumes and filesystems without clone support still permit safe streamed snapshots.
            Some(libc::EXDEV | libc::ENOTSUP | libc::ENOSYS) => Ok(None),
            // Unexpected clone failures prevent publication; TempDir rolls back any private residue.
            _ => Err(SnapshotError::Storage(error)),
        };
    }

    // Open the new inode without following links, then remove its name before mapping any bytes.
    let path = directory.directory.path().join("snapshot");
    let backing = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(SnapshotError::Storage)?;
    fs::remove_file(&path).map_err(SnapshotError::Storage)?;
    Ok(Some(backing))
}

/// Non-macOS test/build targets use the same immutable-inode contract through streaming.
#[cfg(not(target_os = "macos"))]
fn clone_unlinked_source(
    _source: &File,
    _directory: &SnapshotDirectory,
) -> Result<Option<File>, SnapshotError> {
    // No platform clone primitive is assumed; the portable fallback still never maps the source inode.
    Ok(None)
}

/// Populate an already unlinked private inode using a fixed stack buffer and positioned source reads.
fn stream_unlinked_source(
    source: &File,
    directory: &Path,
    expected_bytes: u64,
    is_current: &impl Fn() -> bool,
) -> Result<File, SnapshotError> {
    // tempfile_in returns an unnamed Unix file; no external document path can refer to its inode.
    let mut backing = tempfile::tempfile_in(directory).map_err(SnapshotError::Storage)?;
    let mut buffer = [0_u8; SNAPSHOT_COPY_BYTES];
    let mut offset = 0_u64;
    while offset < expected_bytes {
        // Bound each cancellation interval and read extent; the source descriptor's seek cursor is untouched.
        ensure_current(is_current)?;
        let requested = (expected_bytes - offset).min(buffer.len() as u64) as usize;
        let count = source
            .read_at(&mut buffer[..requested], offset)
            .map_err(SnapshotError::Storage)?;
        if count == 0 {
            // Early EOF means the source shrank; the unnamed partial destination closes on this return.
            return Err(SnapshotError::SourceChanged);
        }
        backing
            .write_all(&buffer[..count])
            .map_err(SnapshotError::Storage)?;
        offset += count as u64;
    }

    // A byte beyond the observed extent indicates growth. Same-size mutations are detected by the
    // caller's source identity recheck; either way only the private copy will ever be mapped.
    ensure_current(is_current)?;
    let mut trailing = [0_u8; 1];
    if source
        .read_at(&mut trailing, expected_bytes)
        .map_err(SnapshotError::Storage)?
        != 0
    {
        // Do not silently accept a truncated snapshot of a source that grew during acquisition.
        return Err(SnapshotError::SourceChanged);
    }
    Ok(backing)
}

#[cfg(test)]
mod tests;
