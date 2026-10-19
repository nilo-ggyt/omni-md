//! 字体专用只读VM owner；不向Markdown正文提供全片借用，也不把Drop步骤当成归还成功。
use std::{
    io,
    os::fd::RawFd,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
    },
};

use crate::immutable_snapshot::FrozenFontFile;

/// 收据只有固定原子，不保descriptor、地址、映射或Font；最后Drop后仍能核真实内核结果。
#[derive(Default)]
pub(super) struct MappingFacts {
    pub(super) completed: AtomicBool,
    pub(super) unmapped: AtomicBool,
    pub(super) mapped_bytes: AtomicUsize,
    pub(super) backing_closed: AtomicBool,
    pub(super) source_closed: AtomicBool,
    errno: AtomicI32,
}
impl MappingFacts {
    fn record_error(&self, error: &io::Error) {
        let errno = error
            .raw_os_error()
            .filter(|errno| *errno != 0)
            .unwrap_or(libc::EIO);
        let _ = self
            .errno
            .compare_exchange(0, errno, Ordering::AcqRel, Ordering::Acquire);
    }
    pub(super) fn first_errno(&self) -> Option<i32> {
        match self.errno.load(Ordering::Acquire) {
            0 => None,
            errno => Some(errno),
        }
    }
    fn result(&self) -> io::Result<()> {
        self.first_errno()
            .map_or(Ok(()), |errno| Err(io::Error::from_raw_os_error(errno)))
    }
}

/// 只能从专用factory的opaque票据建立；没有允许调用者传任意可变File的safe构造器。
pub(super) struct ReadonlyFontMapping {
    address: NonNull<u8>,
    length: usize,
    facts: Arc<MappingFacts>,
    attempted: bool,
}
// SAFETY: factory的独立匿名inode在映射前已结束全部写入，消费票据后没有其他File句柄。
// PROT_READ映射不变，跨线程只传共享只读字节；最后owner Drop前所有借用由Rust生命周期约束。
unsafe impl Send for ReadonlyFontMapping {}
// SAFETY: 多线程只读同一固定地址和长度；可变收据全部是原子，munmap只在独占Drop时调用。
unsafe impl Sync for ReadonlyFontMapping {}

impl ReadonlyFontMapping {
    pub(super) fn map(file: FrozenFontFile) -> io::Result<Self> {
        Self::map_with_close(file, Arc::new(MappingFacts::default()), close_descriptor)
    }
    fn map_with_close(
        file: FrozenFontFile,
        facts: Arc<MappingFacts>,
        close: impl FnOnce(RawFd) -> io::Result<()>,
    ) -> io::Result<Self> {
        let length = file.len();
        let fd = file.into_raw_fd();
        // SAFETY: fd由已核验的冻结票据独占移交，extent非零且可组成slice；只读SHARED不复制字体。
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            let error = io::Error::last_os_error();
            facts.record_error(&error);
            // 未取得VM也必须关闭已消费的唯一FD；任何后继close错误不能覆盖首因。
            match close(fd) {
                Ok(()) => facts.backing_closed.store(true, Ordering::Release),
                Err(error) => facts.record_error(&error),
            }
            facts.completed.store(true, Ordering::Release);
            return Err(error);
        }
        facts.mapped_bytes.store(length, Ordering::Release);
        let Some(address) = NonNull::new(raw.cast::<u8>()) else {
            let error = io::Error::from_raw_os_error(libc::EINVAL);
            facts.record_error(&error);
            // SAFETY: 极端零地址映射尚未发布任何Rust引用；真实补偿结果仍需逐项登记。
            let status = unsafe { libc::munmap(raw, length) };
            record_unmap(&facts, status);
            match close(fd) {
                Ok(()) => facts.backing_closed.store(true, Ordering::Release),
                Err(error) => facts.record_error(&error),
            }
            facts.completed.store(true, Ordering::Release);
            return Err(error);
        };
        let mut mapping = Self {
            address,
            length,
            facts,
            attempted: false,
        };
        match close(fd) {
            Ok(()) => mapping.facts.backing_closed.store(true, Ordering::Release),
            Err(error) => {
                // close失败不能发布Blob；只尝试归还尚未借出的VM，不重试可能已复用的FD。
                mapping.facts.record_error(&error);
                let _ = mapping.release();
                return Err(error);
            }
        }
        Ok(mapping)
    }
    pub(super) fn len(&self) -> usize {
        self.length
    }
    pub(super) fn facts(&self) -> Arc<MappingFacts> {
        Arc::clone(&self.facts)
    }
    pub(super) fn source_closed(&self) {
        self.facts.source_closed.store(true, Ordering::Release);
    }
    pub(super) fn record_error(&self, error: &io::Error) {
        self.facts.record_error(error);
    }
    pub(super) fn close(mut self) -> io::Result<()> {
        self.release()
    }
    fn release(&mut self) -> io::Result<()> {
        self.release_with(|address, length| {
            // SAFETY: 调用者有独占Drop/consume权限，全部Font借用已结束，extent是本owner原实际mmap。
            unsafe { libc::munmap(address.cast(), length) }
        })
    }
    fn release_with(&mut self, unmap: impl FnOnce(*mut u8, usize) -> i32) -> io::Result<()> {
        if self.attempted {
            return self.facts.result();
        }
        self.attempted = true; // 一次尝试后不重认领地址；失败也不能在Drop重试并误解映射复用。
        record_unmap(&self.facts, unmap(self.address.as_ptr(), self.length));
        self.facts.completed.store(true, Ordering::Release);
        self.facts.result()
    }
}
impl AsRef<[u8]> for ReadonlyFontMapping {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: slice仅借用仍活owner；字体factory保证全部字节永久不变，长度不超过isize::MAX。
        unsafe { std::slice::from_raw_parts(self.address.as_ptr(), self.length) }
    }
}
impl Drop for ReadonlyFontMapping {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
fn record_unmap(facts: &MappingFacts, status: i32) {
    if status == 0 {
        facts.mapped_bytes.store(0, Ordering::Release);
        facts.unmapped.store(true, Ordering::Release);
    } else {
        // errno在另一系统调用之前捕获；保留未取得成功归还收据的extent，不假清零。
        facts.record_error(&io::Error::last_os_error());
    }
}
pub(super) fn close_descriptor(fd: RawFd) -> io::Result<()> {
    // SAFETY: 调用者先into_raw_fd转移此唯一关闭责任；仅调用一次，不盲目重试EINTR。
    if unsafe { libc::close(fd) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests;
