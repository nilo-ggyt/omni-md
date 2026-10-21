//! 光标属于原NSView的AppKit cursor rect，不另存URL、链接表或tracking area。
//! Ready仅授语义；登记还须同完整CPU发布与原Source/Font/Input，退场同步撤区。

use super::*;
use dispatch2::MainThreadBound;
use objc2_app_kit::NSCursor;
use objc2_foundation::NSIntersectionRect;

impl NativeMarkdownPagedTextView {
    /// Body接受关闭后先撤活动权限，再调用Native拆图，最后才封闭本View。
    /// 这段原生回调窗口只应撤光标，不能把没有Origin的装饰查询写成永久输入失败。
    fn link_cursor_origin(&self) -> Result<Option<Origin>> {
        match self.origin() {
            Ok(origin) => Ok(Some(origin)),
            Err(NativeMarkdownPagedTextViewError::Revoked) => {
                let delegate = self.ivars().delegate.borrow().clone();
                let delegate = delegate.and_then(|delegate| delegate.upgrade());
                // 槽借用已经归还；只向同一弱owner询问既有accepted-close事实。
                // 不以!current或Scene缺失代签退休，也不新增状态、后继Source或重试。
                let retired = delegate.is_some_and(|delegate| delegate.cursor_rects_retired());
                if retired {
                    Ok(None)
                } else {
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                }
            }
            // 只处理获取Origin的裸撤权；实际Input/Source错误和后面Scene/Font/Layer检查不变。
            Err(cause) => Err(cause),
        }
    }

    /// 先读本次有限sealed几何，再结束全部Scene/Reader强借用；Native重入可以真正退休旧图。
    /// 临时Vec仅含Copy矩形，数量不超过当前Scene已有LINK part数，不是第二份链接语义或缓存。
    pub(super) fn install_link_cursor_rects(&self) -> Result<()> {
        let (ticket, key, weak_scene, mut areas) = {
            let Some(origin) = self.link_cursor_origin()? else {
                return Ok(());
            };
            let ticket = cpu::candidate::Ticket::new(&origin);
            let scene = match self.scene_for_origin(origin) {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(()),
                Err(NativeMarkdownPagedTextViewError::Revoked) if ticket.superseded(self)? => {
                    return Ok(());
                }
                Err(cause) => return Err(cause),
            };
            scene.scene.validate_ready(&|| scene.current(self))?;
            let mut areas = Vec::new();
            let mut allocation_failure = false;
            scene.scene.visit_link_cursor_rects(&mut |area| {
                if allocation_failure {
                    return;
                }
                if areas.try_reserve(1).is_err() {
                    allocation_failure = true;
                } else {
                    areas.push(area);
                }
            })?;
            scene.scene.validate_ready(&|| scene.current(self))?;
            if allocation_failure {
                return Err(NativeMarkdownPagedTextViewError::Raster(
                    "无法分配当前可见链接矩形".into(),
                ));
            }
            let key = scene.key.clone();
            let weak_scene = Rc::downgrade(&scene.scene);
            drop(scene); // 包括Origin里的Body/Input强claim；后面所有AppKit调用均不持原Reader。
            (ticket, key, weak_scene, areas)
        };
        // 正文和各格都已投到同一View坐标；BiDi/负advance的源序不保证按x递增。
        // 在原必要的Copy集合上原地排序，不再借Scene反复扫描part，也不分配第二份集合。
        areas.sort_unstable_by(|left, right| {
            left.origin
                .y
                .total_cmp(&right.origin.y)
                .then_with(|| left.size.height.total_cmp(&right.size.height))
                .then_with(|| left.origin.x.total_cmp(&right.origin.x))
        });
        let mut used = 0;
        for index in 0..areas.len() {
            let area = areas[index];
            if used != 0 {
                let previous = &mut areas[used - 1];
                // 只合并完全相同y域中实际相交或接边的x；跨行、不同格裁剪后的
                // 高度或纵坐标不同就保持独立，不把链接间的普通文字空隙变成手形。
                if previous.origin.y == area.origin.y
                    && previous.size.height == area.size.height
                    && area.origin.x <= previous.origin.x + previous.size.width
                {
                    let right = (previous.origin.x + previous.size.width)
                        .max(area.origin.x + area.size.width);
                    previous.size.width = right - previous.origin.x;
                    continue;
                }
            }
            areas[used] = area;
            used += 1;
        }
        areas.truncate(used);
        let current = || ticket.current(self, &key);
        let validate = || {
            let scene = weak_scene
                .upgrade()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
            scene.validate_ready(&current)
        }; // 每次调用返回前归还临时Scene，绝不把它捕获进Native操作闭包。
        let result = (|| {
            validate()?;
            let cpu = self.ivars().cpu.borrow().clone();
            let Some(cpu) = cpu else { return Ok(()) };
            let published = cpu.cursor_publication_current(self, &key, &current);
            validate()?; // 原Source/Font首错优先于Native图关系的撤权。
            if !published? {
                return Ok(());
            }
            let clip = key.request.viewport;
            if !finite_rect(clip, true) {
                return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
            }
            let text = cursor_step(&validate, &current, NSCursor::IBeamCursor)?;
            cursor_step(&validate, &current, || {
                self.addCursorRect_cursor(clip, &text)
            })?;
            let hand = cursor_step(&validate, &current, NSCursor::pointingHandCursor)?;
            // IBeam底区完全包含各手形子区；每行已合并相交段，各格又按原clip分隔。
            // 不能将行尾空白或隐藏Markdown语法补为链接，也不按URL相同跨行合并。
            for area in areas {
                let area = NSIntersectionRect(area, clip);
                if area.size.width > 0.0 && area.size.height > 0.0 {
                    cursor_step(&validate, &current, || {
                        self.addCursorRect_cursor(area, &hand)
                    })?;
                }
            }
            let published = cpu.cursor_publication_current(self, &key, &current);
            validate()?;
            if !published? {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            Ok(())
        })();
        match result {
            Err(NativeMarkdownPagedTextViewError::Revoked) => {
                // 与CPU旧候选共享同一严格弱票：普通bool拒绝不等于正常退休。
                // 保原资源首因后，只认真实客户/Input/几何换代，不让cleanup后的空serial冒充权限。
                match validate() {
                    Ok(()) | Err(NativeMarkdownPagedTextViewError::Revoked) => {}
                    Err(cause) => return Err(cause),
                }
                self.discardCursorRects();
                if ticket.superseded(self)? {
                    Ok(())
                } else {
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                }
            }
            other => other,
        }
    }

    /// 原CPU同步栈仍持Source；沿已有主队列只交弱View与弱key，退栈后才进入AppKit。
    /// 不增加serial、观察者或另一条队列；过时通知只结束，不为后继Input补签旧光标。
    pub(super) fn queue_link_cursor_refresh(&self, key: &NativeMarkdownPagedSceneKey) {
        let mtm = self.mtm();
        let weak = MainThreadBound::new(objc2::rc::Weak::from(self), mtm);
        let key = MainThreadBound::new(key.clone(), mtm);
        crate::native_surface::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("光标刷新只沿原AppKit主队列");
            let Some(view) = weak.get(mtm).load() else {
                return;
            };
            if view.ivars().closed.get() {
                return;
            }
            let result = view.refresh_link_cursor_rects(key.get(mtm));
            view.record(result);
        });
    }

    /// 通知只针对原实际完整图；没有额外paint或强制reset/set cursor。
    /// 先结束Scene强借用再查询窗口，Source仍由原Weak在Native操作前后逐次短借核验。
    fn refresh_link_cursor_rects(&self, key: &NativeMarkdownPagedSceneKey) -> Result<()> {
        let (ticket, weak_scene) = {
            let Some(origin) = self.link_cursor_origin()? else {
                return Ok(());
            };
            if origin.state != key.request.input
                || origin.geometry_epoch != key.request.geometry_epoch
                || !origin.delegate.scene_current(key)
            {
                return Ok(());
            }
            let ticket = cpu::candidate::Ticket::new(&origin);
            let scene = match self.scene_for_origin(origin) {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(()),
                Err(NativeMarkdownPagedTextViewError::Revoked) if ticket.superseded(self)? => {
                    return Ok(());
                }
                Err(cause) => return Err(cause),
            };
            if !scene.key.same(key) {
                return Ok(());
            }
            scene.scene.validate_ready(&|| scene.current(self))?;
            let weak_scene = Rc::downgrade(&scene.scene);
            drop(scene);
            (ticket, weak_scene)
        };
        let current = || ticket.current(self, key);
        let validate = || {
            let scene = weak_scene
                .upgrade()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
            scene.validate_ready(&current)
        };
        let result = (|| {
            let window = cursor_step(&validate, &current, || self.window())?;
            if let Some(window) = window {
                cursor_step(&validate, &current, || {
                    window.invalidateCursorRectsForView(self)
                })?;
            }
            Ok(())
        })();
        match result {
            Err(NativeMarkdownPagedTextViewError::Revoked) => {
                // AppKit通知也可交付后继编辑；先保原资源错误，再只退休真实完整票的换代。
                match validate() {
                    Ok(()) | Err(NativeMarkdownPagedTextViewError::Revoked) => {}
                    Err(cause) => return Err(cause),
                }
                if ticket.superseded(self)? {
                    Ok(())
                } else {
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                }
            }
            other => other,
        }
    }
}

/// validate只在各次Native调用前后短借原Source/Font；operation执行时不持这些资源。
/// 后验先交真实Source首错，才交普通撤权，避免悬停把磁盘/字体失败洗成空光标。
fn cursor_step<T>(
    validate: &impl Fn() -> Result<()>,
    current: &impl Fn() -> bool,
    operation: impl FnOnce() -> T,
) -> Result<T> {
    validate()?;
    if !current() {
        return Err(NativeMarkdownPagedTextViewError::Revoked);
    }
    let value = operation();
    validate()?;
    if !current() {
        return Err(NativeMarkdownPagedTextViewError::Revoked);
    }
    Ok(value)
}
