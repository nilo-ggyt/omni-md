//! 封存后移交原唯一文件的只读入口；不持映射、页面缓存或正文副本。

use std::{fs::File, ops::Range, os::unix::fs::FileExt, sync::Arc};

use super::{Error, LayoutIndexObserver, PAGE, ResourceState, Result, close_file, read_exact_at};

/// 可自然跨线程移交的只读索引；它保留原文件与固定原子收据，不借用构建器。
pub(crate) struct LayoutIndexReader<const N: usize> {
    file: Option<File>,
    count: usize,
    poisoned: bool,
    resources: Arc<ResourceState>,
    #[cfg(feature = "paged-shaping-frontier")]
    upstream: Option<Arc<ResourceState>>,
    #[cfg(feature = "paged-shaping-frontier")]
    prefix_claim: bool,
}

impl<const N: usize> LayoutIndexReader<N> {
    pub(super) fn from_sealed(file: File, count: usize, resources: Arc<ResourceState>) -> Self {
        // 最终移交的原FD也观察同scope首错；此前发布的prefix仍可能报告实际底层IO故障。
        #[cfg(feature = "paged-shaping-frontier")]
        let upstream = Some(Arc::clone(&resources));
        Self {
            file: Some(file),
            count,
            poisoned: false,
            resources,
            #[cfg(feature = "paged-shaping-frontier")]
            upstream,
            #[cfg(feature = "paged-shaping-frontier")]
            prefix_claim: false,
        }
    }

    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn from_prefix(file: File, count: usize, upstream: Arc<ResourceState>) -> Self {
        let resources = Arc::new(ResourceState::default());
        resources.file_open.store(true, super::Ordering::Release);
        Self {
            file: Some(file),
            count,
            poisoned: false,
            resources,
            upstream: Some(upstream),
            prefix_claim: true,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.count
    }

    pub(crate) fn resource_observer(&self) -> LayoutIndexObserver {
        LayoutIndexObserver {
            own: Arc::clone(&self.resources),
            #[cfg(feature = "paged-shaping-frontier")]
            upstream: self.upstream.clone(),
        }
    }

    fn healthy(&self) -> Result<()> {
        if self.poisoned
            || self.file.is_none()
            || self.resources.reader_failed.load(super::Ordering::Acquire)
            || self.resources.cleanup_failed.load(super::Ordering::Acquire)
            || self.upstream_has_failed()
        {
            // 首个失败已经撤销整份输出；后继读取不能把剩余正确记录重新签成完整结果。
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    fn upstream_has_failed(&self) -> bool {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            self.upstream.as_ref().is_some_and(|upstream| {
                upstream.producer_failed.load(super::Ordering::Acquire)
                    || upstream.cleanup_failed.load(super::Ordering::Acquire)
            })
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        {
            false
        }
    }

    pub(crate) fn read(&mut self, record: usize) -> Result<[u8; N]> {
        self.healthy()?;
        let end = match record.checked_add(1) {
            Some(end) => end,
            None => {
                // 单记录入口同样不能让坐标溢出绕过永久撤权与资源观察。
                self.fail();
                return Err(Error::Overflow);
            }
        };
        let mut output = [0; N];
        self.read_batch(record..end, std::slice::from_mut(&mut output))?;
        Ok(output)
    }

    /// 一次最多读取256条且不超过16KiB，调用方必须提供恰好覆盖原范围的输出。
    ///
    /// IO错误可能已写入输出前缀；调用方必须丢弃本次完整候选，不能发布部分结果。
    pub(crate) fn read_batch(&mut self, range: Range<usize>, output: &mut [[u8; N]]) -> Result<()> {
        self.healthy()?;
        let result = self.read_batch_inner(range, output);
        #[cfg(feature = "paged-shaping-frontier")]
        if matches!(&result, Err(Error::Io(_)))
            && let Some(upstream) = &self.upstream
        {
            upstream
                .producer_failed
                .store(true, super::Ordering::Release);
        } // 真实底层文件失败影响同inode的所有前缀；调用者无效范围只撤销其自己的Reader。
        if result.is_err() {
            // 校验失败与真实IO失败都撤销原只读输出权限；原cause仍由本次Result返回。
            self.fail();
        }
        result
    }

    fn read_batch_inner(&self, range: Range<usize>, output: &mut [[u8; N]]) -> Result<()> {
        let records = range
            .end
            .checked_sub(range.start)
            .filter(|&records| records == output.len() && records <= 256)
            .ok_or(Error::InvalidRecord)?;
        let bytes = records.checked_mul(N).ok_or(Error::Overflow)?;
        if range.end > self.count || bytes > PAGE {
            // 上限约束是本次真实读取预算；不能截短范围后伪称整个调用已经完成。
            return Err(Error::InvalidRecord);
        }
        let start = range.start.checked_mul(N).ok_or(Error::Overflow)?;
        let offset = u64::try_from(start).map_err(|_| Error::Overflow)?;
        offset
            .checked_add(u64::try_from(bytes).map_err(|_| Error::Overflow)?)
            .ok_or(Error::Overflow)?;
        let file = self.file.as_ref().ok_or(Error::Poisoned)?;
        // 同构建器固定record读取共用完整区间协议；失败仍由外层撤销整份Reader并保首因。
        read_exact_at(output.as_flattened_mut(), offset, |output, position| {
            file.read_at(output, position)
        })
    }

    fn fail(&mut self) {
        self.poisoned = true;
        self.resources
            .reader_failed
            .store(true, super::Ordering::Release);
    }

    fn close_resources(&mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            // close_file执行原唯一FD的真实close；不创建第二File，也不在失败后猜测FD可重试。
            let closed = close_file(file, &self.resources);
            #[cfg(feature = "paged-shaping-frontier")]
            if let Some(upstream) = &self.upstream {
                if closed.is_ok() && self.prefix_claim {
                    upstream
                        .prefix_readers
                        .fetch_sub(1, super::Ordering::AcqRel);
                    self.prefix_claim = false;
                } else if closed.is_err() {
                    upstream
                        .producer_failed
                        .store(true, super::Ordering::Release);
                }
            } // 只有真实close成功才归还名额；失败保留负面计数，不能据此再开新FD。
            closed?;
        }
        if self.resources.cleanup_failed.load(super::Ordering::Acquire) {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    pub(crate) fn close(mut self) -> Result<()> {
        self.close_resources()?;
        if self.poisoned
            || self.resources.reader_failed.load(super::Ordering::Acquire)
            || self.upstream_has_failed()
        {
            // 资源已真实归还仍不能洗掉先前的读取失败；观察者可分别核对IO与清理事实。
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
}

impl<const N: usize> Drop for LayoutIndexReader<N> {
    fn drop(&mut self) {
        // 只补偿尚未归还的唯一FD；读取撤权本身不冒充内核close失败。
        if self.close_resources().is_err() {
            self.resources
                .cleanup_failed
                .store(true, super::Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests;
