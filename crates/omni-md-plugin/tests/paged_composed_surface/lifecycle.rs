//! 独立实窗生命周期负控；沿原Owner、原Input和真实队列，不给产品增加控件或输入动作。

use super::super::*;
use objc2::rc::Retained;
use objc2_quartz_core::CALayer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Normal,
    PendingClose,
    ReplacedBacking,
    ReparentedMount,
    PostCommitRevoked,
}

pub struct Case {
    mode: Mode,
    backing: Retained<CALayer>,
    mount: Retained<CALayer>,
    successor: Option<Retained<CALayer>>,
    steps: usize,
    passed: bool,
    pending_at_close: Cell<bool>,
}
/// 只借原harness已存在的真实对象，控制不会创建第二套生产owner。
pub struct Context<'a> {
    pub body: &'a NativeMarkdownPagedTextView,
    pub window: &'a NSWindow,
    pub viewport: &'a Rc<NativeMarkdownPagedViewport>,
    pub owner: &'a Rc<scene::Owner>,
    pub font: &'a Rc<NativeMarkdownLineFont>,
    pub paints: &'a Rc<scene::PaintFacts>,
    pub pixels:
        &'a omni_md_plugin::native_surface::paged_text_view::NativeMarkdownViewportPixelObserver,
}
impl Case {
    pub fn start(
        mode: Mode,
        context: Context<'_>,
        output: Rc<RefCell<MarkdownComposedLines>>,
    ) -> Self {
        let Context {
            body,
            window,
            viewport,
            owner,
            font,
            paints,
            pixels,
        } = context;
        let backing = body.layer().unwrap();
        // SAFETY: 只读真实CPU独占backing的有限mount，后继所有者由本显式控制自建。
        let mounts = unsafe { backing.sublayers() }.unwrap();
        assert_eq!(mounts.len(), 1);
        let mount = mounts.objectAtIndex(0);
        drop(mounts);
        let mut successor = None;
        let mut passed = true;
        match mode {
            Mode::PendingClose | Mode::PostCommitRevoked => {
                if mode == Mode::PostCommitRevoked {
                    // SAFETY: 旧完整group来自实际已发布画面；只保存地址比较，固定pool仍保活原对象防ABA。
                    let groups = unsafe { mount.sublayers() }.unwrap();
                    assert_eq!(groups.len(), 1);
                    owner
                        .old_group
                        .set(std::ptr::from_ref(&*groups.objectAtIndex(0)) as usize);
                    *owner.watched.borrow_mut() = Some(Weak::from(body));
                    owner.revoke_after_mount.set(true);
                }
                let published = body.cpu_paints().unwrap();
                let next = publish_scene(output, body, viewport, owner, font, paints);
                let next_current = next.current();
                passed &= next_current;
                body.displayIfNeeded();
                let pending = body.cpu_paint_pending();
                let unchanged_publish = body.cpu_paints() == Some(published);
                let live_bytes = pixels.snapshot().live_pixel_bytes;
                passed &= pending == Some(true) && unchanged_publish && live_bytes > 0;
                println!(
                    "paged_composed_lifecycle mode={mode:?} setup_scene_current={next_current} setup_actual_pending={pending:?} setup_unchanged_publish={unchanged_publish} setup_live_pixel_bytes={live_bytes} formal=false"
                ); // 每条已有前置事实单独记录，不能以最终清理成功掩盖负控从未进入目标边界。
                if mode == Mode::PostCommitRevoked {
                    window.displayIfNeeded();
                    deliver_once(); // 唯一新候选实际走到mount setter；随后原current主动撤权。
                    let fired = owner.revoke_fired.get();
                    let actual = body.take_failure();
                    let original_error = matches!(actual, Some(Error::Revoked));
                    let no_new_ready = body.cpu_paints() == Some(published)
                        && body.cpu_paint_pending() == Some(false);
                    // SAFETY: 只读唯一真实mount的后果；失败保持sticky，仍继续原资源close。
                    let removed =
                        unsafe { mount.sublayers() }.is_none_or(|groups| groups.is_empty());
                    passed &= fired && original_error && no_new_ready && removed;
                    println!(
                        "paged_composed_lifecycle mode={mode:?} revoked_after_actual_mount={fired} returned_original_revoked={original_error} actual_error={actual:?} source_eio_not_injected=true no_new_ready_publish={no_new_ready} failed_owned_group_removed={removed} formal=false"
                    );
                }
            }
            Mode::ReplacedBacking => {
                let next = CALayer::new();
                next.setOpacity(0.73); // 后继标量保持原值，清理不能用新backing当旧owner目标。
                body.setLayer(Some(&next));
                passed &= body
                    .layer()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq(actual, &*next));
                successor = Some(next);
            }
            Mode::ReparentedMount => {
                let next = CALayer::new();
                next.addSublayer(&mount); // 真实迁移原mount；原close只能清自己的业务图片，不能夺这条parent。
                successor = Some(next);
            }
            Mode::Normal => panic!("普通验收不能伪装异常生命周期控制"),
        }
        Self {
            mode,
            backing,
            mount,
            successor,
            steps: body.cpu_paint_steps().unwrap(),
            passed,
            pending_at_close: Cell::new(false),
        }
    }
    pub fn before_close(&self, body: &NativeMarkdownPagedTextView) -> bool {
        let actual_pending = body.cpu_paint_pending() == Some(true);
        self.pending_at_close
            .set(self.mode == Mode::PendingClose && actual_pending);
        println!(
            "paged_composed_lifecycle mode={:?} pending_at_actual_view_close={actual_pending} setup_checks_passed={} formal=false",
            self.mode, self.passed
        );
        self.passed && (self.mode != Mode::PendingClose || actual_pending)
    }
    pub fn weaks(&self) -> Vec<(&'static str, Weak<AnyObject>)> {
        let mut observed = vec![
            ("lifecycle_original_backing", weak(&self.backing)),
            ("lifecycle_actual_mount", weak(&self.mount)),
        ];
        if let Some(successor) = &self.successor {
            observed.push(("lifecycle_successor", weak(successor)));
        }
        observed
    }
    pub fn after_close(
        self,
        body: &NativeMarkdownPagedTextView,
        pixels: &omni_md_plugin::native_surface::paged_text_view::NativeMarkdownViewportPixelObserver,
        receipt: &omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedCloseReceipt,
    ) -> bool {
        let result = receipt.verify_released();
        let replaced = matches!(self.mode, Mode::ReplacedBacking | Mode::ReparentedMount);
        let expected_result = if replaced {
            matches!(receipt.cleanup_failure(), Some(omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedCloseFailure::View(Error::Revoked))) && result.is_err()
        } else {
            result.is_ok()
        };
        let raw_zero = pixels.is_released();
        let cpu_removed = body.cpu_paint_pending().is_none();
        // SAFETY: 原body仍由harness主线程强持活；只读公开父子关系，不跨借用或保留裸父指针。
        let view_detached = unsafe { body.superview() }.is_none();
        println!(
            "paged_composed_lifecycle mode={:?} actual_close_result={result:?} expected_close_result={expected_result} own_pixels_zero={raw_zero} actual_cpu_owner_removed={cpu_removed} closed_view_detached={view_detached} formal=false",
            self.mode
        ); // 所有模式都记录真实返回；不以资源释放成功掩盖关闭合同或前置负控失败。
        let mut successor_preserved = true;
        if let Some(successor) = &self.successor {
            match self.mode {
                Mode::ReplacedBacking => {
                    let actual = body.layer();
                    let same = actual
                        .as_deref()
                        .is_some_and(|actual| std::ptr::eq(actual, &**successor));
                    drop(actual);
                    successor_preserved = same
                        && body.wantsLayer()
                        && successor.opacity() == 0.73
                        && self.mount.superlayer().is_none();
                    if same {
                        body.setWantsLayer(false);
                        body.setLayer(None); // 外方先核后自行归还；此清理不是被测旧owner的close动作。
                    }
                }
                Mode::ReparentedMount => {
                    successor_preserved = self
                        .mount
                        .superlayer()
                        .as_deref()
                        .is_some_and(|parent| std::ptr::eq(parent, &**successor));
                    // SAFETY: 外方只拥有本控制迁入的空mount；在记录被测关系之后自行归还。
                    unsafe {
                        successor.setSublayers(None);
                    }
                }
                _ => successor_preserved = false,
            }
            let external_alive =
                weak(&self.mount).load().is_some() && weak(successor).load().is_some();
            println!(
                "paged_composed_lifecycle mode={:?} actual_close_result={result:?} expected_close_result={expected_result} successor_preserved={successor_preserved} externally_held_native_still_alive={external_alive} own_pixels_zero={raw_zero} formal=false",
                self.mode
            );
        }
        deliver_once(); // 真实交付旧Weak队列；任何失败仍等最终Native/FD/映射收据打印后拒收。
        let no_replay = pixels.is_released() && body.cpu_paints().is_none();
        let passed = self.passed
            && expected_result
            && view_detached
            && raw_zero
            && cpu_removed
            && successor_preserved
            && no_replay;
        println!(
            "paged_composed_lifecycle mode={:?} actual_close_during_pending={} old_queue_after_close_no_replay={no_replay} before_close_steps={} actual_pixels_zero={raw_zero} lifecycle_checks_passed={passed} final_native_nil_check_deferred_until_external_owner_drop=true formal=false",
            self.mode,
            self.pending_at_close.get(),
            self.steps
        );
        passed
    }
}
