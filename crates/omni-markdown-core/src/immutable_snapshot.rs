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
#[cfg(feature = "native-markdown-candidate")]
use std::io::{Seek, SeekFrom};
#[cfg(target_os = "macos")]
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;

#[cfg(test)]
use memmap2::{Mmap, MmapOptions};
use thiserror::Error;

const SNAPSHOT_COPY_BYTES: usize = 64 * 1_024;
// Darwin sys/clonefile.h defines this stable flag since macOS 10.12; libc exposes the call but not the flag.
#[cfg(target_os = "macos")]
const CLONE_NOOWNERCOPY: u32 = 0x0002;

/// Acquisition failures distinguish unstable source extent and cancellation from storage failure.
#[derive(Debug, Error)]
pub(crate) enum SnapshotError {
    #[error("创建快照期间源文件大小发生变化")]
    SourceChanged,
    #[error("快照创建已取消")]
    Cancelled,
    #[error("快照存储不可用：{0}")]
    Storage(io::Error),
}

/// Capture one already size-checked descriptor into immutable, unnamed backing storage.
///
/// The caller rechecks its source descriptor and path identity before publishing derived results.
/// Cancellation is observed before acquisition and between fallback-copy chunks. Kernel clone,
/// read, and write calls are synchronous; cancellation cannot interrupt a blocked filesystem call.
#[cfg(test)]
pub(crate) fn snapshot_file(
    source: &File,
    expected_bytes: u64,
    is_current: impl Fn() -> bool,
) -> Result<Option<Mmap>, SnapshotError> {
    // Safety regressions exercise clone-first acquisition; production consumes the bounded backing API.
    snapshot_file_inner(source, expected_bytes, &is_current, true, None)
}

/// Frozen read-only file receipt consumed only by a font blob. No safe API
/// borrows, clones, or writes its private file.
#[cfg(feature = "paged-shaping")]
pub(crate) struct FrozenFontFile {
    file: File,
    bytes: usize,
}
#[cfg(feature = "paged-shaping")]
impl FrozenFontFile {
    pub(crate) fn len(&self) -> usize {
        self.bytes
    }
    /// Transfers the sole descriptor-close obligation to the font VM owner,
    /// retaining no second writable handle.
    pub(crate) fn into_raw_fd(self) -> std::os::fd::RawFd {
        use std::os::fd::IntoRawFd;
        self.file.into_raw_fd()
    }
}

/// Reuses private anonymous-inode acquisition without first creating an `Mmap`,
/// so the font owner does not acquire a second redundant mapping.
#[cfg(feature = "paged-shaping")]
pub(crate) fn snapshot_font_file(
    source: &File,
    expected_bytes: u64,
) -> Result<Option<FrozenFontFile>, SnapshotError> {
    if expected_bytes == 0 {
        return Ok(None); // The public FontBytes entry rejects EmptyFile; never map a zero extent.
    }
    let bytes = usize::try_from(expected_bytes)
        .ok()
        .filter(|bytes| *bytes <= isize::MAX as usize)
        .ok_or_else(|| SnapshotError::Storage(io::Error::from(io::ErrorKind::InvalidInput)))?;
    let mut directory = SnapshotDirectory::new()?;
    let file = directory.acquire_with_strategy(source, expected_bytes, &|| true, true)?;
    // The factory verified a private unnamed regular inode and its complete
    // extent; every fallback-copy writer has ended.
    Ok(file.map(|file| FrozenFontFile { file, bytes }))
}

/// Acquires canonical backing for controlled read leases, without
/// exposing or scanning an intermediate whole-document `Mmap` slice.
#[cfg(feature = "native-markdown-candidate")]
pub(crate) fn snapshot_canonical(
    source: &File,
    expected_bytes: u64,
    is_current: impl Fn() -> bool,
) -> Result<Option<crate::original_backing::OriginalBacking>, SnapshotError> {
    ensure_current(&is_current)?;
    if expected_bytes == 0 {
        // Empty input owns no text resource and needs no temporary directory or
        // descriptor merely to represent absence.
        return Ok(None);
    }

    let mut directory = SnapshotDirectory::new()?;
    directory.capture_backing(source, expected_bytes, &is_current)
}

/// Exclusive anonymous output for streaming transformations. Owns a file and
/// length only, without caching text or exposing a writable handle.
#[cfg(feature = "native-markdown-candidate")]
pub(crate) struct CanonicalSnapshotOutput {
    file: File,
    bytes: usize,
}

#[cfg(feature = "native-markdown-candidate")]
impl CanonicalSnapshotOutput {
    /// Creates an already unlinked private inode. Its sole `File` releases storage
    /// directly when preparation fails or is cancelled.
    pub(crate) fn new() -> Result<Self, SnapshotError> {
        let file = tempfile::tempfile().map_err(SnapshotError::Storage)?;
        Ok(Self { file, bytes: 0 })
    }

    /// Sequentially writes a caller-bounded chunk; the completed-byte measure
    /// advances only after the entire chunk is written.
    pub(crate) fn write_all(&mut self, bytes: &[u8]) -> Result<(), SnapshotError> {
        // The final candidate must be addressable in this process. Reject overflow
        // before writing rather than creating an unrepresentable extent.
        let next = self
            .bytes
            .checked_add(bytes.len())
            .filter(|bytes| *bytes <= isize::MAX as usize)
            .ok_or_else(|| {
                SnapshotError::Storage(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "匿名输出超出可寻址范围",
                ))
            })?;
        self.file.write_all(bytes).map_err(SnapshotError::Storage)?;
        self.bytes = next;
        Ok(())
    }

    /// Returns completely written bytes without reading text or allocating an
    /// equally sized memory copy.
    pub(crate) fn len(&self) -> usize {
        self.bytes
    }

    /// Removes only an output tail and positions the next write at its new end;
    /// global trimming uses this to discard trailing whitespace.
    pub(crate) fn truncate(&mut self, bytes: usize) -> Result<(), SnapshotError> {
        if bytes > self.bytes {
            // Truncation cannot invent unwritten text by sparse extension. Reject
            // before changing either the file or completed-byte measure.
            return Err(SnapshotError::Storage(io::Error::new(
                io::ErrorKind::InvalidInput,
                "匿名输出截尾位置超出已写范围",
            )));
        }
        self.file
            .set_len(bytes as u64)
            .map_err(SnapshotError::Storage)?;
        self.file
            .seek(SeekFrom::Start(bytes as u64))
            .map_err(SnapshotError::Storage)?;
        // Failure of truncate or seek makes the caller discard this candidate.
        // Publish the new sequential-write measure only after both succeed.
        self.bytes = bytes;
        Ok(())
    }

    /// Consumes the sole writer and freezes it through the bounded mapping owner.
    /// No writable file remains with the caller after success.
    pub(crate) fn freeze(
        self,
    ) -> Result<Option<crate::original_backing::OriginalBacking>, SnapshotError> {
        let Self { file, bytes } = self;
        let metadata = file.metadata().map_err(SnapshotError::Storage)?;
        if !metadata.is_file() || metadata.nlink() != 0 {
            // Immutable-byte authority requires a private unnamed regular inode. A
            // named file that another process can rewrite must never back a Rust slice.
            return Err(SnapshotError::Storage(io::Error::new(
                io::ErrorKind::InvalidData,
                "匿名输出必须是无文件名的普通文件",
            )));
        }
        if metadata.len() != bytes as u64 {
            // Reject extra bytes left by a failed write and every extent differing
            // from the completed-write measure.
            return Err(SnapshotError::SourceChanged);
        }
        if bytes == 0 {
            // Empty output closes its actual sole descriptor. It creates neither a
            // zero-length mapping nor an invented mapping-release receipt.
            use std::os::fd::IntoRawFd;
            let descriptor = file.into_raw_fd();
            // SAFETY: `into_raw_fd` transfers the sole descriptor. Close it exactly
            // once, leaving no writable file aliases.
            if unsafe { libc::close(descriptor) } != 0 {
                // No mapping owner exists to report this close result. Do not let
                // `File::drop` silently hide an actual descriptor failure.
                return Err(SnapshotError::Storage(io::Error::last_os_error()));
            }
            return Ok(None);
        }
        // SAFETY: The temporary inode is private and already unlinked; its `File`
        // has never been lent out or cloned.
        // All writes and truncations ended, and regular-file, anonymity, and extent
        // facts were just verified. Consuming this owner transfers its sole file
        // to the bounded mapping owner, which closes the descriptor immediately
        // after acquiring a mapping.
        unsafe { crate::original_backing::OriginalBacking::map(file, bytes) }
            .map(Some)
            .map_err(|error| {
                // Preserve mapping and descriptor-close errno. Never fall back to heap
                // text or acquire another snapshot to disguise a failed resource action.
                let errno = match error {
                    // Propagate the original owner's mmap, close, or page-query failure.
                    crate::original_backing::OriginalBackingError::Unavailable(errno) => errno,
                    // The factory excludes empty/out-of-range input; any other owner
                    // admission failure remains an invalid mapping request.
                    _ => libc::EINVAL,
                };
                SnapshotError::Storage(io::Error::from_raw_os_error(errno))
            })
    }
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

    /// Captures bounded backing in this query's private directory, using the same
    /// factory for cancellation, clone/copy fallback, and identity validation.
    pub(crate) fn capture_backing(
        &mut self,
        source: &File,
        expected_bytes: u64,
        is_current: &impl Fn() -> bool,
    ) -> Result<Option<crate::original_backing::OriginalBacking>, SnapshotError> {
        let Some(file) = self.acquire_with_strategy(source, expected_bytes, is_current, true)?
        else {
            // Empty input allocates no mapping, zero-address slice, or fabricated
            // VM-release receipt.
            return Ok(None);
        };
        let length = usize::try_from(expected_bytes).map_err(|_| {
            SnapshotError::Storage(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot too large",
            ))
        })?;
        // SAFETY: Acquisition verified a private, unnamed, fixed extent and ended
        // every fallback writer. The sole descriptor transfers to the mapping owner.
        unsafe { crate::original_backing::OriginalBacking::map(file, length) }
            .map(Some)
            .map_err(|error| {
                // Preserve this factory's error type and the real mapping errno; never
                // convert failure into success or sparse fallback storage.
                let errno = match error {
                    crate::original_backing::OriginalBackingError::Unavailable(errno) => errno,
                    _ => libc::EINVAL,
                };
                SnapshotError::Storage(io::Error::from_raw_os_error(errno))
            })
    }

    /// Map a finite safety-regression fixture; production never exposes a whole-document slice.
    #[cfg(test)]
    fn capture_with_strategy(
        &mut self,
        source: &File,
        expected_bytes: u64,
        is_current: &impl Fn() -> bool,
        allow_clone: bool,
    ) -> Result<Option<Mmap>, SnapshotError> {
        let Some(backing) =
            self.acquire_with_strategy(source, expected_bytes, is_current, allow_clone)?
        else {
            // Empty input owns neither an anonymous inode nor a mapping; fixture
            // callers observe absence rather than an invented resource.
            return Ok(None);
        };
        let length = usize::try_from(expected_bytes).map_err(|_| {
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

    /// Acquire the frozen unnamed inode; each consumer separately owns its bounded mapping lifetime.
    fn acquire_with_strategy(
        &mut self,
        source: &File,
        expected_bytes: u64,
        is_current: &impl Fn() -> bool,
        allow_clone: bool,
    ) -> Result<Option<File>, SnapshotError> {
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
        // No text is read here. All writers have ended; the caller receives only
        // the exclusive descriptor of this private inode.
        Ok(Some(backing))
    }
}

/// Own the complete private namespace for one standalone capture or forced-strategy regression.
#[cfg(test)]
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
