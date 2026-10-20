//! 诊断观察真实完整帧的单次交付；不以 Ready、排队次数或单 tile 完成代替像素发布。

use std::rc::Weak;

use dispatch2::MainThreadBound;
use objc2_foundation::MainThreadMarker;

use super::{
    NativeMarkdownPagedBody, NativeMarkdownPagedBodyError, NativeMarkdownPagedSceneKey,
    NativeMarkdownPagedTextViewDelegate, Result, unsupported,
};

/// 调用方只接收仍属原 key 的真实完整帧；弱观察不延长界面、正文或测试 runloop。
pub trait NativeMarkdownPagedBodyPaintObserver {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize);
}

pub(super) struct PaintObservation {
    observer: Weak<dyn NativeMarkdownPagedBodyPaintObserver>,
    baseline: usize,
}

impl NativeMarkdownPagedBody {
    /// 在原生动作之前登记一次观察，基线从实际 surface 捕获，不能由调用者自报。
    /// 这不会使视图标脏、执行绘图或补发已完成帧；没有新发布就保持真实未完成。
    pub fn observe_next_cpu_frame(
        &self,
        observer: Weak<dyn NativeMarkdownPagedBodyPaintObserver>,
    ) -> Result<()> {
        if !self.current_body() {
            return Err(NativeMarkdownPagedBodyError::Closed);
        }
        if self.paint_observation.borrow().is_some() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let baseline = self.view()?.cpu_paints().ok_or_else(|| unsupported())?;
        *self.paint_observation.borrow_mut() = Some(PaintObservation { observer, baseline });
        Ok(())
    }

    pub(super) fn queue_cpu_publication(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        if self.paint_observation.borrow().is_none() {
            return; // 无诊断观察者时不增加产品主队列工作，也不保存已发布帧历史。
        }
        let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
        // 原 key 含主线程 Rc::Weak；把同一弱票放入 MainThreadBound 再过 Send 队列，
        // 不做 unsafe Send，也不增加 Scene、View、Source 或像素强边。
        let key = MainThreadBound::new(key.clone(), self.mtm);
        super::super::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("完整帧观察只在原主队列交付");
            let key = key.get(mtm);
            let Some(owner) = weak.get(mtm).upgrade() else {
                return;
            };
            let Some(baseline) = owner
                .paint_observation
                .borrow()
                .as_ref()
                .map(|observation| observation.baseline)
            else {
                return;
            };
            if !owner.scene_current(key) {
                return; // 原帧已被输入或几何撤销，不能捕获后继 key 给这个通知补签。
            }
            let Ok(view) = owner.view() else {
                return;
            };
            if view.cpu_paint_pending() != Some(false)
                || view.cpu_paints() != Some(paints)
                || paints <= baseline
                || !owner.scene_current(key)
            {
                return; // 真实未完成、被后继取代或计数未增长都不是这次观察的成功。
            }
            let observation = owner.paint_observation.borrow_mut().take();
            drop(view); // 结束 Native 强 claim 和所有槽借用，再进入可能停止 runloop 的外方回调。
            if let Some(observer) =
                observation.and_then(|observation| observation.observer.upgrade())
            {
                observer.frame_published(key, paints);
            }
        });
    }
}
