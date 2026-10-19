//! 原匿名索引的不可变提交前缀；每份快照独占一个FD，不复制record或保留可写映射。

use super::*;

const MAX_PREFIX_READERS: usize = 2;

/// 背压只属于前缀发布；普通GCB/断行索引不会产生此状态，不能污染它们的IO错误协议。
#[derive(Debug, thiserror::Error)]
pub(crate) enum LayoutIndexSnapshotError {
    #[error("原索引的两份在用前缀快照尚未归还")]
    ReadersBusy,
    #[error("原前缀索引发布失败：{0}")]
    Index(#[from] LayoutIndexError),
}
type SnapshotResult<T> = std::result::Result<T, LayoutIndexSnapshotError>;

/// 先预留真实FD名额；失败仍归还计数，只有移交给实际Reader后才由其close收据结算。
struct Reservation {
    resources: Arc<ResourceState>,
    transferred: bool,
}
impl Reservation {
    fn new(resources: &Arc<ResourceState>) -> SnapshotResult<Self> {
        let mut count = resources.prefix_readers.load(Ordering::Acquire);
        loop {
            if count >= MAX_PREFIX_READERS {
                return Err(LayoutIndexSnapshotError::ReadersBusy);
            } // 有界Current与candidate尚未归还时停止发布，不能制造第三份FD或丢弃旧帧。
            match resources.prefix_readers.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => count = actual, // 只重试原名额的原子竞争，不轮询Native释放或重做排版。
            }
        }
        Ok(Self {
            resources: Arc::clone(resources),
            transferred: false,
        })
    }
    fn transfer(mut self) -> Arc<ResourceState> {
        self.transferred = true;
        Arc::clone(&self.resources)
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.transferred {
            self.resources.prefix_readers.fetch_sub(1, Ordering::AcqRel);
        } // 未构造Reader的失败没有真实FD待关，名额必须同步归还。
    }
}

impl<const N: usize> PagedLayoutIndex<N> {
    /// 只有上层已提交的LineEnd上界可移交；索引层不签发任何正文或行语义。
    pub(crate) fn snapshot_reader(
        &mut self,
        committed_records: usize,
    ) -> SnapshotResult<LayoutIndexReader<N>> {
        self.healthy()?;
        if committed_records == 0 || committed_records > self.count || self.sealed {
            return Err(Error::InvalidRecord.into());
        } // 非法范围不调用任何IO，也不会把已封存索引重新变成可写状态。
        committed_records
            .checked_mul(N)
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(Error::Overflow)?;
        let reservation = Reservation::new(&self.resources)?;
        let result: Result<LayoutIndexReader<N>> = (|| {
            // 仅写到本次已提交上界；后继继续用同一buffer追加，绝不重写旧prefix字节。
            self.flush_through(committed_records)?;
            let file = self.file.as_ref().ok_or(Error::Poisoned)?;
            let prefix_file = file.try_clone()?;
            // Reader只用pread且有自己的逻辑EOF；共享文件游标与后继文件增长不改变快照边界。
            Ok(LayoutIndexReader::from_prefix(
                prefix_file,
                committed_records,
                reservation.transfer(),
            ))
        })();
        if result.is_err() {
            self.mark_failed();
        } // 真实写入/复制失败永久撤原producer；quota不足在进入本块前已经单独返回。
        result.map_err(Into::into)
    }
}
