//! 仅诊断构建的单次 current 回调重入；动作仍调用真实 Native 输入或窗口 API。

use std::{cell::RefCell, rc::Weak};

use omni_markdown_core::{MarkdownLayoutTextSource, MarkdownLineComposer};

use super::{
    NativeMarkdownPagedBody, NativeMarkdownPagedBodyError, NativeMarkdownPagedSceneRequest, Result,
};

/// 全部是原构造资源的观察者；没有 Source、Composer、文件或 Native 强边。
pub struct NativeMarkdownPagedBodyPreparationResources {
    pub source_authority: omni_markdown_core::MarkdownLayoutTextAuthority,
    pub source: omni_markdown_core::MarkdownLayoutTextSourceObserver,
    pub writer: omni_markdown_core::MarkdownComposedLineResourceObserver,
    pub worker: omni_markdown_core::MarkdownLineComposerWorkerResourceObserver,
}

/// 只在显式 probe feature 编译；不增加默认产品交互或当前请求的成功权限。
pub trait NativeMarkdownPagedBodyPreparationProbe {
    fn native_check(
        &self,
        original: &NativeMarkdownPagedSceneRequest,
        resources: NativeMarkdownPagedBodyPreparationResources,
    );
}

pub(super) struct Delivery {
    observer: Weak<dyn NativeMarkdownPagedBodyPreparationProbe>,
    original: NativeMarkdownPagedSceneRequest,
    resources: NativeMarkdownPagedBodyPreparationResources,
}

impl NativeMarkdownPagedBody {
    /// 下一次真实 Font current 回调消费一次弱观察；不会制造请求、调用布局或改变输入。
    pub fn probe_next_native_preparation(
        &self,
        observer: Weak<dyn NativeMarkdownPagedBodyPreparationProbe>,
    ) -> Result<()> {
        if !self.current_body() {
            return Err(NativeMarkdownPagedBodyError::Closed);
        }
        if self.preparation_probe.borrow().is_some() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        *self.preparation_probe.borrow_mut() = Some(observer);
        Ok(())
    }

    pub(super) fn take_preparation_probe(
        &self,
        original: &NativeMarkdownPagedSceneRequest,
        source: &MarkdownLayoutTextSource,
        composer: Option<&MarkdownLineComposer>,
    ) -> Result<RefCell<Option<Delivery>>> {
        let observer = self.preparation_probe.borrow_mut().take();
        let Some(observer) = observer else {
            return Ok(RefCell::new(None)); // 无诊断钩子时不创建原资源观察包。
        };
        let composer = composer.ok_or(NativeMarkdownPagedBodyError::Closed)?;
        Ok(RefCell::new(Some(Delivery {
            observer,
            original: original.clone(),
            resources: NativeMarkdownPagedBodyPreparationResources {
                source_authority: source.authority(),
                source: source.resource_observer(),
                writer: composer.resource_observer()?,
                worker: composer.worker_resource_observer(),
            },
        })))
    }
}

pub(super) fn deliver(delivery: &RefCell<Option<Delivery>>) {
    let delivered = delivery.borrow_mut().take();
    if let Some(delivered) = delivered
        && let Some(observer) = delivered.observer.upgrade()
    {
        // 所有槽借用已经结束，原 callback 可真实重入输入或resize；它没有重签旧票的权利。
        observer.native_check(&delivered.original, delivered.resources);
    }
}
