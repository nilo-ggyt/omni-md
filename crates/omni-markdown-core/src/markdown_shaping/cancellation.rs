//! 仅标量的跨线程取消请求；任务和分页数组始终由原worker独占。

use super::{
    MarkdownShapingError as Error, MarkdownShapingJob, MarkdownShapingStatus as Status, Result,
};

/// 请求停止不等于已退出；调用方必须join原worker并核真实资源收据。
#[derive(Clone, Default)]
pub struct MarkdownShapingCancellation(harfrust::CancellationToken);

impl MarkdownShapingCancellation {
    /// 永久请求原任务停止；没有复位、重采Source或签发Ready的权限。
    pub fn request(&self) {
        self.0.request();
    }
    /// 只读请求事实，不代表字形算法已经观察它。
    #[must_use]
    pub fn requested(&self) -> bool {
        self.0.requested()
    }
}

impl MarkdownShapingJob {
    /// 只交付原标量请求位，不让Native线程强持worker的Doc、Font或字形数组。
    #[must_use]
    pub fn cancellation(&self) -> MarkdownShapingCancellation {
        self.cancellation.clone()
    }

    pub(super) fn cancelled<T>(&mut self) -> Result<T> {
        let backing = self
            .payload
            .as_ref()
            .and_then(|payload| payload.source.original_backing_observer());
        self.status = Status::Cancelled;
        drop(self.payload.take()); // owner借用结束后真实归还所有映射，绝不发布部分glyph。
        if let Some(errno) = backing.as_ref().and_then(|observer| observer.first_errno()) {
            self.status = Status::Poisoned;
            return Err(
                super::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
            );
        } // 最后Source owner的真实munmap失败也不能伪装成干净取消。
        if let Some(error) = self.resources.paging.first_error() {
            self.status = Status::Poisoned;
            return Err(harfrust::ShapeError::Paging {
                operation: error.operation as u32,
                errno: error.errno,
            }
            .into());
        } // Buffer实际Drop的close/munmap错误保留原scope首因，不被Cancelled遮盖。
        Err(Error::Cancelled)
    }

    pub(super) fn cooperative_control(&self) -> Result<harfrust::CooperativeControl> {
        let payload = self.payload.as_ref().ok_or(Error::Poisoned)?;
        let observer = payload.source.original_backing_observer();
        #[cfg(test)]
        let hook = self.cancellation_hook.clone();
        // 只捕获原BackingFacts的Arc；不保活Source、原inode、mmap或任何Native对象。
        let health: harfrust::CooperativeSourceHealth = std::sync::Arc::new(move |point| {
            #[cfg(test)]
            if let Some(hook) = &hook {
                hook(point);
            }
            #[cfg(not(test))]
            let _ = point;
            observer
                .as_ref()
                .and_then(|observer| observer.first_errno())
        });
        Ok(harfrust::CooperativeControl::new(
            self.cancellation.0.clone(),
            payload.buffer.paging_resource_observer(),
            Some(health),
        ))
    }
}
