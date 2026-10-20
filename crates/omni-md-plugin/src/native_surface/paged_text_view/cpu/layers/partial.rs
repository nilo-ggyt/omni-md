//! 同几何完整候选只替换实际变化的槽；未变角色保持原父层和原CGImage，不重新提交contents。

use super::*;

/// 新槽先归原group的有限pending拥有；安装失败也逐项撤图，不只Drop一个本地Rc。
struct PendingSlots<'a>(&'a LayerGroup);
impl Drop for PendingSlots<'_> {
    fn drop(&mut self) {
        self.0.clear_pending();
    }
}

/// 首个replace即是实际副作用；提交前失败在原事务内撤真实partial图，
/// commit后校验失败则撤已完成的整图。外层原补偿票只退休同serial的Published元数据。
struct Replacement<'a> {
    pool: &'a LayerPool,
    attempt: &'a Attempt,
    backing: &'a CALayer,
    committed: bool,
}
impl Drop for Replacement<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.pool.revoke_attempt(self.attempt, self.backing);
        }
    }
}

impl LayerPool {
    /// 调用者仍须完整paint并核新旧Geometry相同；本入口再核真实发布、像素尺寸和逐槽位置。
    /// None只表示不适用，原图及Attempt均不变；任何已有Native图失配仍真实返回错误。
    pub(in super::super) fn publish_partial(
        &self,
        backing: &CALayer,
        frame: &TileFrame,
        previous: &Publication,
        attempt: &mut Attempt,
        current: &impl Fn() -> bool,
    ) -> Result<Option<Publication>> {
        if !std::ptr::eq(attempt.pool, self)
            || self.serial.get() != attempt.before
            || previous.serial != attempt.before
            || self.active.get() != Some(previous.index)
        {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if frame.tiles.is_empty()
            || frame.tiles.len() > MAX_TILE_SLOTS
            || dimension(previous.bounds.size.width, previous.scale)? != frame.width
            || dimension(previous.bounds.size.height, previous.scale)? != frame.height
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if previous.tiles.len() != frame.tiles.len()
            || previous
                .tiles
                .iter()
                .zip(&frame.tiles)
                .any(|(old, image)| old.tile != image.tile)
        {
            return Ok(None); // 分区、位置或槽数变化沿原双组完整挂载，不在这里搬动旧角色。
        }
        let unchanged = previous
            .tiles
            .iter()
            .zip(&frame.tiles)
            .map(|(old, image)| {
                *old == slot::Receipt::new(
                    image.tile,
                    &image.image,
                    image.secondary_image.as_deref(),
                    image.presentation,
                )
            })
            .collect::<Vec<_>>();
        if unchanged.iter().all(|same| *same) || unchanged.iter().all(|same| !*same) {
            return Ok(None); // 整帧相同已有无setter复用；没有原槽可留则保持原完整发布。
        }
        self.validate(backing, previous, current)?;
        let group = &self.groups[previous.index];
        if !group.pending.borrow().is_empty() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let old_slots = group.slots.borrow().clone();
        if old_slots.len() != frame.tiles.len() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // 原显式事务覆盖新槽安装、全部replace与失败补偿；不强制flush，也不提交半幅像素。
        let transaction = checked(current, || {
            CATransaction::begin();
            Transaction
        })?;
        checked(current, || CATransaction::setDisableActions(true))?;
        let pending = PendingSlots(group);
        let mut next = Vec::with_capacity(frame.tiles.len());
        for ((old, image), same) in old_slots.iter().zip(&frame.tiles).zip(&unchanged) {
            if *same {
                next.push(Rc::clone(old));
                continue; // 不触碰原slot的parent、contents、mask、颜色或几何，CA可保其原准备结果。
            }
            let fresh = Rc::new(slot::Slot::new(image.presentation, current)?);
            group.pending.borrow_mut().push(Rc::clone(&fresh));
            let tile = image.tile;
            let rect = NSRect::new(
                NSPoint::new(
                    tile.x as f64 / previous.scale,
                    tile.bottom as f64 / previous.scale,
                ),
                NSSize::new(
                    tile.width as f64 / previous.scale,
                    tile.height as f64 / previous.scale,
                ),
            );
            fresh.install(
                &image.image,
                image.secondary_image.as_ref(),
                slot::Layout {
                    rect,
                    scale: previous.scale,
                    presentation: image.presentation,
                },
                &group.palette,
                current,
            )?;
            next.push(fresh);
        }
        // 安装过程可能Native重入；第一次替换之前仍要求原完整发布图一项不差。
        // pending不改变原slots，验证不会看到尚未接管的新角色或漏掉将退休的旧槽。
        self.validate(backing, previous, current)?;
        attempt.index = previous.index;
        let mut replacement = Replacement {
            pool: self,
            attempt,
            backing,
            committed: false,
        };
        for ((old, fresh), same) in old_slots.iter().zip(&next).zip(&unchanged) {
            if *same {
                continue;
            }
            let replaced = checked(current, || {
                let parents = (old.container().superlayer(), fresh.container().superlayer());
                let valid = parents
                    .0
                    .as_deref()
                    .is_some_and(|parent| std::ptr::eq::<CALayer>(parent, &*group.layer))
                    && parents.1.is_none();
                drop(parents);
                if !valid {
                    return false;
                }
                // SAFETY: 紧邻核原old.superlayer正是group，新槽没有父层。公开replace不要求
                // 将未变层先detach，也不把已有parent的层重新交给setSublayers产生歧义。
                // 检查和replace之间不再调用current，避免它真实重入后使用失效父关系。
                unsafe {
                    group
                        .layer
                        .replaceSublayer_with(old.container(), fresh.container())
                };
                // setter已真实执行，先登记实际active组/serial，再允许后置current返回撤权。
                // 尚未发生replace时serial仍是before，失败只清pending而保留原完整画面。
                self.active.set(Some(replacement.attempt.index));
                self.serial.set(replacement.attempt.serial);
                true
            })?;
            if !replaced {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
        }
        // 全部替换后才交接长期槽；这里只移动Rust所有权，借用均在Native清理前结束。
        let displaced = group.slots.replace(next);
        drop(std::mem::take(&mut *group.pending.borrow_mut()));
        drop(old_slots);
        let removed = displaced
            .into_iter()
            .zip(&unchanged)
            .filter_map(|(slot, same)| (!*same).then_some(slot))
            .collect();
        if !group.retire_slots(removed) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let publication = Publication {
            index: replacement.attempt.index,
            serial: replacement.attempt.serial,
            bounds: previous.bounds,
            scale: previous.scale,
            tiles: frame
                .tiles
                .iter()
                .map(|image| {
                    slot::Receipt::new(
                        image.tile,
                        &image.image,
                        image.secondary_image.as_deref(),
                        image.presentation,
                    )
                })
                .collect(),
        };
        self.validate(backing, &publication, current)?;
        drop(pending); // 成功交接后pending已空，不对未变slot执行任何清理或重新安装。
        drop(transaction);
        // commit也是真实Native边界，可能重入并撤销原图。完整替换已经结束，
        // 后置失败仍由同Attempt撤实际整图；不能先解除补偿再把失效Publication交回CPU。
        self.validate(backing, &publication, current)?;
        replacement.committed = true;
        drop(replacement);
        Ok(Some(publication))
    }
}
