//! Own one anonymous FD without user mappings, lent Files, or cross-thread mutable cursors; sealed reads use bounded fixed-record pread.
use super::{MARKDOWN_SHAPED_SPOOL_STEP, RECORD};
use std::{
    fs::File,
    io::{self, Write},
    os::{fd::IntoRawFd, unix::fs::FileExt},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MarkdownShapedSpoolIoOperation {
    Create = 1,
    Write,
    Sync,
    Inspect,
    Read,
    Close,
    InvalidState,
}
impl std::fmt::Display for MarkdownShapedSpoolIoOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Keep enum/record codes stable while user-visible operation descriptions remain Simplified Chinese.
        formatter.write_str(match self {
            Self::Create => "创建匿名字形文件",
            Self::Write => "写入字形批次",
            Self::Sync => "同步字形文件",
            Self::Inspect => "核对字形文件",
            Self::Read => "读取字形批次",
            Self::Close => "关闭字形文件",
            Self::InvalidState => "字形文件协议校验",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{operation}失败（errno={errno}）")]
pub struct MarkdownShapedSpoolIoError {
    pub operation: MarkdownShapedSpoolIoOperation,
    pub errno: i32,
}
type Result<T> = std::result::Result<T, MarkdownShapedSpoolIoError>;
#[derive(Default)]
struct Facts {
    open: AtomicBool,
    closed: AtomicBool,
    cleanup_failed: AtomicBool,
    first: AtomicU64,
}
#[derive(Clone)]
pub(super) struct Observer(Arc<Facts>);
pub(super) struct Snapshot {
    pub file_open: bool,
    pub close_succeeded: bool,
    pub cleanup_failed: bool,
    pub first_error: Option<MarkdownShapedSpoolIoError>,
}
impl Observer {
    pub(super) fn snapshot(&self) -> Snapshot {
        use MarkdownShapedSpoolIoOperation as Operation;
        let packed = self.0.first.load(Ordering::Acquire);
        let first_error = if packed == 0 {
            None
        } else {
            let operation = match (packed >> 32) as u32 {
                1 => Operation::Create,
                2 => Operation::Write,
                3 => Operation::Sync,
                4 => Operation::Inspect,
                5 => Operation::Read,
                6 => Operation::Close,
                _ => Operation::InvalidState,
            }; // Decode original fixed tags explicitly rather than unsafely treating arbitrary integers as enum variants.
            Some(MarkdownShapedSpoolIoError {
                operation,
                errno: packed as u32 as i32,
            })
        };
        Snapshot {
            file_open: self.0.open.load(Ordering::Acquire),
            close_succeeded: self.0.closed.load(Ordering::Acquire),
            cleanup_failed: self.0.cleanup_failed.load(Ordering::Acquire),
            first_error,
        }
    }
}

pub(super) struct Store {
    file: Option<File>,
    facts: Arc<Facts>,
    sealed: bool,
    #[cfg(test)]
    fail_write: bool,
    #[cfg(test)]
    fail_sync: bool,
    #[cfg(test)]
    fail_close: bool,
}
impl Store {
    pub(super) fn new() -> Result<Self> {
        let file = tempfile::tempfile().map_err(|error| MarkdownShapedSpoolIoError {
            operation: MarkdownShapedSpoolIoOperation::Create,
            errno: error.raw_os_error().unwrap_or(libc::EIO),
        })?;
        let facts = Arc::new(Facts::default());
        facts.open.store(true, Ordering::Release);
        Ok(Self {
            file: Some(file),
            facts,
            sealed: false,
            #[cfg(test)]
            fail_write: false,
            #[cfg(test)]
            fail_sync: false,
            #[cfg(test)]
            fail_close: false,
        })
    }
    pub(super) fn observer(&self) -> Observer {
        Observer(Arc::clone(&self.facts))
    }
    fn record(
        &self,
        operation: MarkdownShapedSpoolIoOperation,
        error: io::Error,
    ) -> MarkdownShapedSpoolIoError {
        let errno = error
            .raw_os_error()
            .filter(|&value| value != 0)
            .unwrap_or(libc::EIO);
        let packed = (u64::from(operation as u32) << 32) | u64::from(errno as u32);
        let _ = self
            .facts
            .first
            .compare_exchange(0, packed, Ordering::AcqRel, Ordering::Acquire);
        // Return the same permanent first fact; cleanup failure cannot replace the original write/read/sync cause.
        self.observer()
            .snapshot()
            .first_error
            .expect("刚记录非零operation")
    }
    fn invalid(&self) -> MarkdownShapedSpoolIoError {
        self.record(
            MarkdownShapedSpoolIoOperation::InvalidState,
            io::Error::from_raw_os_error(libc::EINVAL),
        )
    }
    pub(super) fn append(&mut self, bytes: &[u8]) -> Result<()> {
        if self.sealed
            || bytes.len() > RECORD * MARKDOWN_SHAPED_SPOOL_STEP
            || !bytes.len().is_multiple_of(RECORD)
        {
            return Err(self.invalid()); // A concrete spool-contract error must not pretend a system I/O operation actually ran.
        }
        let result = match self.file.as_mut() {
            Some(file) => file.write_all(bytes),
            None => return Err(self.invalid()),
        };
        result.map_err(|error| self.record(MarkdownShapedSpoolIoOperation::Write, error))?;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_write) {
            return Err(self.record(
                MarkdownShapedSpoolIoOperation::Write,
                io::Error::from_raw_os_error(libc::EIO),
            ));
        } // Existing injection follows actual write, verifying that a written candidate prefix still cannot publish.
        Ok(())
    }
    pub(super) fn seal(&mut self, expected: usize) -> Result<()> {
        if self.sealed {
            return Err(self.invalid());
        }
        let file = self.file.as_ref().ok_or_else(|| self.invalid())?;
        let actual = file
            .metadata()
            .map_err(|error| self.record(MarkdownShapedSpoolIoOperation::Inspect, error))?
            .len();
        if u64::try_from(expected).ok() != Some(actual) {
            return Err(self.invalid());
        }
        file.sync_data()
            .map_err(|error| self.record(MarkdownShapedSpoolIoOperation::Sync, error))?;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_sync) {
            return Err(self.record(
                MarkdownShapedSpoolIoOperation::Sync,
                io::Error::from_raw_os_error(libc::EIO),
            ));
        }
        self.sealed = true; // Revoke write permission only after complete writes, actual extent verification, and successful sync.
        Ok(())
    }
    pub(super) fn read_into(&self, offset: usize, bytes: &mut [u8]) -> Result<()> {
        if !self.sealed
            || bytes.len() > RECORD * MARKDOWN_SHAPED_SPOOL_STEP
            || !bytes.len().is_multiple_of(RECORD)
        {
            return Err(self.invalid()); // Callers read bounded fixed batches, without borrowing File to read the whole run.
        }
        let file = self.file.as_ref().ok_or_else(|| self.invalid())?;
        let offset = u64::try_from(offset).map_err(|_| self.invalid())?;
        file.read_exact_at(bytes, offset)
            .map_err(|error| self.record(MarkdownShapedSpoolIoOperation::Read, error))?;
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        let descriptor = file.into_raw_fd();
        // SAFETY: Consume unique File ownership with no lent/cloned FD; close exactly once, never retrying a possibly reused descriptor.
        let status = unsafe { libc::close(descriptor) };
        if status == 0 {
            self.facts.open.store(false, Ordering::Release);
            self.facts.closed.store(true, Ordering::Release);
        } else {
            self.facts.cleanup_failed.store(true, Ordering::Release);
            return Err(self.record(
                MarkdownShapedSpoolIoOperation::Close,
                io::Error::last_os_error(),
            ));
        }
        #[cfg(test)]
        if std::mem::take(&mut self.fail_close) {
            self.facts.cleanup_failed.store(true, Ordering::Release);
            return Err(self.record(
                MarkdownShapedSpoolIoOperation::Close,
                io::Error::from_raw_os_error(libc::EIO),
            ));
        } // Retain actual close success separately from the injected permanent negative fact, without pretending the FD remains open.
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn fail_write_after_real(&mut self) {
        self.fail_write = true;
    }
    #[cfg(test)]
    pub(super) fn fail_sync_after_real(&mut self) {
        self.fail_sync = true;
    }
    #[cfg(test)]
    pub(super) fn fail_close_after_real(&mut self) {
        self.fail_close = true;
    }
    #[cfg(test)]
    pub(super) fn truncate_after_seal(&self) {
        self.file.as_ref().unwrap().set_len(0).unwrap();
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.close();
    } // Fallible facts are stored atomically; Drop neither panics nor clears the first error.
}
