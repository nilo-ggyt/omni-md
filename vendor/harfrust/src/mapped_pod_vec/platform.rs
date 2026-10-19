//! 文件分页的平台原语；只负责真实预分配与回写撤驻留，不参与字形或Unicode语义。

use std::{fs::File, io, os::fd::AsRawFd};

/// 在发布更大的映射前预留其新增磁盘区间；失败由原owner转成永久失败。
#[cfg(target_os = "macos")]
pub(super) fn preallocate(file: &File, old_bytes: usize, new_bytes: i64) -> io::Result<()> {
    let old_bytes =
        i64::try_from(old_bytes).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    let growth = new_bytes
        .checked_sub(old_bytes)
        .filter(|growth| *growth > 0)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut store = libc::fstore_t {
        fst_flags: libc::F_ALLOCATEALL,
        fst_posmode: libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: growth,
        fst_bytesalloc: 0,
    };
    // SAFETY: 结构逐字段匹配Darwin公开F_PREALLOCATE ABI；FD由owner持有且已匿名。
    // 调用仅预留当前增长区间，不截短文件，不改变旧映射逻辑字节或引用地址。
    let status = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut store) };
    if status == 0 {
        Ok(())
    } else {
        // 普通磁盘不足必须在创建/发布新映射之前显式返回，不能伪装预留成功。
        Err(io::Error::last_os_error())
    }
}

/// Linux同样真实预留文件块；不以稀疏set_len或Vec替代实际分页输入。
#[cfg(target_os = "linux")]
pub(super) fn preallocate(file: &File, old_bytes: usize, new_bytes: i64) -> io::Result<()> {
    let old_bytes =
        i64::try_from(old_bytes).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    let growth = new_bytes
        .checked_sub(old_bytes)
        .filter(|growth| *growth > 0)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: 非负offset与正增长量均经检查，FD仍有效；公开API不读写Rust内存。
    // posix_fallocate返回errno本身，不能把它误读成当前线程last_os_error。
    let status = unsafe { libc::posix_fallocate(file.as_raw_fd(), old_bytes, growth) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status))
    }
}

/// 回写并释放有限页的物理驻留；地址和文件字节身份在整个调用期间保持不变。
///
/// # Safety
/// `address..address+length`必须是仍活着的、页对齐的MAP_SHARED可写映射范围。
/// 调用方必须持有映射owner，确保期间没有truncate、munmap、remap或并发写入。
/// 调用不会使Rust引用失效；硬件或文件系统错误由owner永久poison，不得提交部分结果。
pub(super) unsafe fn flush(address: *mut u8, length: usize) -> io::Result<()> {
    // SAFETY: 调用者保证地址/长度属于原活映射；MS_SYNC先把脏页写回其真实匿名文件。
    let status =
        unsafe { libc::msync(address.cast(), length, libc::MS_SYNC | libc::MS_INVALIDATE) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    {
        // Linux的MS_INVALIDATE不承诺像本机Darwin对照那样撤RSS；在同步成功后再用
        // 公开DONTNEED建议归还文件页。即使此调用成功，未实测的平台认证仍为false。
        // SAFETY: 原地址范围仍有效且已回写；DONTNEED不改变映射地址或文件逻辑字节。
        if unsafe { libc::madvise(address.cast(), length, libc::MADV_DONTNEED) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
