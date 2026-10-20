//! 双组只为实际瓦片建立图层；挂载点原子切换完整候选，旧组立即归还全部角色。

use super::*;
use crate::native_surface::viewport_pixels::MAX_TILE_SLOTS;
use objc2::rc::Weak;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::NSSize;
use objc2_quartz_core::{kCAContentsFormatGray8Uint, kCAContentsFormatRGBA8Uint, kCAGravityResize};

#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) mod probe;
mod slot;

pub(super) struct LayerPool {
    mount: Retained<CALayer>,
    groups: [LayerGroup; 2],
    active: Cell<Option<usize>>,
    serial: Cell<usize>,
}

/// 只记录本池实际发布的有限身份，不保CGImage、正文、Source或Scene强边。
#[derive(Clone)]
pub(super) struct Publication {
    index: usize,
    serial: usize,
    bounds: NSRect,
    scale: f64,
    tiles: Box<[slot::Receipt]>,
}
struct LayerGroup {
    layer: Retained<CALayer>,
    slots: RefCell<Vec<Rc<slot::Slot>>>,
    retired: RefCell<Vec<Weak<AnyObject>>>,
    palette: slot::Palette,
}

/// 仅同pool、原前序和后继serial可以归属一次真实发布；不保Source或image。
pub(super) struct Attempt {
    pool: *const LayerPool,
    before: usize,
    serial: usize,
    index: usize,
}

/// 失败时只清理已经分离的候选；若setter已经挂载，仍保留完整图组供真实后果核对。
struct Candidate<'a>(&'a LayerGroup);
impl Drop for Candidate<'_> {
    fn drop(&mut self) {
        if self.0.layer.superlayer().is_none() {
            self.0.clear();
        } // 此处不重新领取Scene；清理只操作本owner已经分离的空池。
    }
}

impl LayerPool {
    pub(super) fn new(current: &impl Fn() -> bool) -> Result<Self> {
        let mount = checked(current, CALayer::new)?;
        // 最后一个设备像素可能跨出分数point边界；只在原正文实际bounds裁边，绝不压缩整图。
        checked(current, || mount.setMasksToBounds(true))?;
        let first = LayerGroup::new(current)?;
        let second = LayerGroup::new(current)?;
        Ok(Self {
            mount,
            groups: [first, second],
            active: Cell::new(None),
            serial: Cell::new(0),
        })
    }
    pub(super) fn mount(&self) -> &CALayer {
        &self.mount
    }

    pub(super) fn mount_claim(&self) -> Retained<CALayer> {
        Retained::clone(&self.mount)
    }

    /// 只登记当前真实图中的角色；记录不保活图层，也不为未用瓦片建立空对象。
    pub(super) fn weaks(&self) -> Vec<Weak<AnyObject>> {
        let mut values = vec![layer_weak(&self.mount)];
        for group in &self.groups {
            values.push(layer_weak(&group.layer));
            values.extend(group.retired.borrow().iter().cloned());
            let slots = group.slots.borrow().clone();
            for slot in slots {
                slot.add_weaks(&mut values);
            }
        }
        values
    }

    pub(super) fn attempt(&self) -> Result<Attempt> {
        let before = self.serial.get();
        Ok(Attempt {
            pool: self,
            before,
            serial: before
                .checked_add(1)
                .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?,
            index: self.active.get().map_or(0, |old| 1 - old),
        })
    }
    /// 只撤销本次已经实际挂载、仍是原mount/group的图片；不能回滚成“从未提交”。
    pub(super) fn revoke_attempt(&self, attempt: &Attempt, backing: &CALayer) -> bool {
        if !std::ptr::eq(attempt.pool, self)
            || self.serial.get() != attempt.serial
            || self.active.get() != Some(attempt.index)
        {
            return false;
        }
        let group = &self.groups[attempt.index];
        let parent = self.mount.superlayer();
        // SAFETY: 只读原标准CALayer数组，比较原保活对象身份，不发送正文回调。
        let children = unsafe { self.mount.sublayers() };
        let same = parent
            .as_deref()
            .is_some_and(|parent| std::ptr::eq(parent, backing))
            && children.as_ref().is_some_and(|children| {
                children.len() == 1
                    && std::ptr::eq::<CALayer>(&*children.objectAtIndex(0), &*group.layer)
            });
        drop((parent, children));
        if same {
            CATransaction::begin();
            let transaction = Transaction;
            CATransaction::setDisableActions(true);
            if parent_is(&group.layer, &self.mount) {
                group.layer.removeFromSuperlayer();
            }
            group.clear();
            self.active.set(None);
            drop(transaction);
        } else {
            // 外方只夺走parent关系，原serial仍归本attempt；不能先丢claim再把业务图片藏在外国图中。
            // clear逐项只清仍等于原claim的图片/颜色/own child，保留外方parent和后继替换内容。
            group.clear();
            self.active.set(None);
        }
        true // private serial证明本次提交发生过；调用者必须废除旧weakMetadata。
    }

    /// 新候选已经完成真实绘图；只有全部瓦片仍是当前原图，才沿用现存 Native 图。
    /// 收据最多复制64项身份标量，不新增图层、图像或历史帧；任一表示/位置/主次图不同
    /// 都交回正常发布。像素相等由上游封存器逐字节证明，这里只消费其真实复用身份。
    pub(super) fn reuse_publication(
        &self,
        backing: &CALayer,
        frame: &TileFrame,
        previous: &Publication,
        bounds: NSRect,
        scale: f64,
        current: &impl Fn() -> bool,
    ) -> Result<Option<Publication>> {
        if frame.tiles.is_empty()
            || frame.tiles.len() > MAX_TILE_SLOTS
            || dimension(bounds.size.width, scale)? != frame.width
            || dimension(bounds.size.height, scale)? != frame.height
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if previous.bounds != bounds
            || previous.scale != scale
            || previous.tiles.len() != frame.tiles.len()
            || previous.tiles.iter().zip(&frame.tiles).any(|(old, image)| {
                *old != slot::Receipt::new(
                    image.tile,
                    &image.image,
                    image.secondary_image.as_deref(),
                    image.presentation,
                )
            })
        {
            return Ok(None);
        }
        // 相同 CGImage 不代表它仍装在原槽中。完整核原 mount、serial、各层关系、
        // 颜色和主次图片；外方修改或 Native/current 重入失权必须失败，不能降成 miss。
        self.validate(backing, previous, current)?;
        Ok(Some(previous.clone()))
    }

    pub(super) fn publish(
        &self,
        backing: &CALayer,
        frame: &TileFrame,
        attempt: &Attempt,
        bounds: NSRect,
        scale: f64,
        current: &impl Fn() -> bool,
    ) -> Result<Publication> {
        if !std::ptr::eq(attempt.pool, self) || self.serial.get() != attempt.before {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let serial = attempt.serial;
        if frame.tiles.is_empty()
            || frame.tiles.len() > MAX_TILE_SLOTS
            || dimension(bounds.size.width, scale)? != frame.width
            || dimension(bounds.size.height, scale)? != frame.height
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        self.require_mount(backing, current)?;
        let index = attempt.index;
        let group = &self.groups[index];
        if checked(current, || group.layer.superlayer())?.is_some() {
            return Err(NativeMarkdownPagedTextViewError::Revoked); // 仍挂载的旧组绝不能当可写候选复用。
        }
        if !group.clear() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // 已分离槽只归还本owner旧image；外方改过槽则拒绝复用，不覆盖后继内容。
        let _candidate = Candidate(group);
        checked(current, CATransaction::begin)?;
        let transaction = Transaction;
        checked(current, || CATransaction::setDisableActions(true))?;
        let local = NSRect::new(NSPoint::new(0.0, 0.0), bounds.size);
        checked(current, || group.layer.setFrame(local))?;
        checked(current, || group.layer.setBounds(local))?;
        // 每次候选只建立本帧所需角色；先交本组接管，重入撤权能看见并清理已建的原关系。
        let slots = frame
            .tiles
            .iter()
            .map(|image| slot::Slot::new(image.presentation, current).map(Rc::new))
            .collect::<Result<Vec<_>>>()?;
        group.slots.replace(slots.clone());
        for (slot, image) in slots.iter().zip(&frame.tiles) {
            let tile = image.tile;
            // tile的frame仍是精确像素数/scale，contents与mask保持1:1；尾部余像素由mount裁掉。
            let rect = NSRect::new(
                NSPoint::new(tile.x as f64 / scale, tile.bottom as f64 / scale),
                NSSize::new(tile.width as f64 / scale, tile.height as f64 / scale),
            );
            slot.install(
                &image.image,
                image.secondary_image.as_ref(),
                slot::Layout {
                    rect,
                    scale,
                    presentation: image.presentation,
                },
                &group.palette,
                current,
            )?;
        }
        let containers = slots
            .iter()
            .map(|slot| &**slot.container())
            .collect::<Vec<_>>();
        let tiles = checked(current, || NSArray::from_slice(&containers))?;
        drop(containers); // 固定最多64个借用身份，原Native数组已持各container；不延长业务资源。
        // SAFETY: 数组只包含本组独占的有限CALayer；组尚未挂载，因此这些setter不显示部分候选。
        checked(current, || unsafe {
            group.layer.setSublayers(Some(&tiles))
        })?;
        drop(tiles);
        self.require_mount(backing, current)?;
        checked(current, || self.mount.setFrame(bounds))?;
        checked(current, || self.mount.setBounds(local))?;
        let next = checked(current, || {
            NSArray::from_retained_slice(std::slice::from_ref(&group.layer))
        })?;
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: mount只属于本owner，数组只有完整候选group；一个setter是唯一可见帧提交点。
        unsafe { self.mount.setSublayers(Some(&next)) };
        self.active.set(Some(index)); // Native提交已发生即记录事实；后置撤权不得把它误称从未提交。
        self.serial.set(serial);
        drop(next);
        // 同一显式事务里断开并清空旧组，避免另一次隐式事务把上帧像素额外保留到下一帧。
        // 非活跃组即使后置Source票失效也能安全清理：先核真实detached，绝不清当前已挂载帧。
        let old = &self.groups[1 - index];
        let old_intact = if old.layer.superlayer().is_none() {
            old.clear()
        } else {
            false
        };
        drop(transaction);
        if !old_intact {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let publication = Publication {
            index,
            serial,
            bounds,
            scale,
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
        Ok(publication)
    }

    /// 原组、slot顺序、精确几何和image身份都必须仍相同；不能以active布尔值替代实际Native图。
    pub(super) fn validate(
        &self,
        backing: &CALayer,
        publication: &Publication,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        self.require_mount(backing, current)?;
        if self.active.get() != Some(publication.index) || self.serial.get() != publication.serial {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let group = &self.groups[publication.index];
        empty_container(&self.mount, current)?;
        empty_container(&group.layer, current)?;
        let local = NSRect::new(NSPoint::ZERO, publication.bounds.size);
        let geometries = checked(current, || {
            self.mount.frame() == publication.bounds
                && self.mount.bounds() == local
                && group.layer.frame() == local
                && group.layer.bounds() == local
        })?;
        if !geometries {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: mount只由本owner设置为一个类型正确的CALayer组，读取只比较实际身份。
        let actual = checked(current, || unsafe { self.mount.sublayers() })?;
        let matches = actual.as_ref().is_some_and(|layers| {
            layers.len() == 1 && std::ptr::eq::<CALayer>(&*layers.objectAtIndex(0), &*group.layer)
        });
        drop(actual);
        if !matches || !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: 原组的有限数组只包含本池slots；完整有序关系同样属于原发布事实。
        let actual_tiles = checked(current, || unsafe { group.layer.sublayers() })?;
        let Some(actual_tiles) = actual_tiles else {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        };
        if actual_tiles.len() != publication.tiles.len() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // 快照只保本次有限槽；Native/current 调用前归还 RefCell 借用，允许真实关闭重入。
        let slots = group.slots.borrow().clone();
        if slots.len() != publication.tiles.len() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        for (index, receipt) in publication.tiles.iter().enumerate() {
            let tile = receipt.tile;
            let actual_slot = checked(current, || actual_tiles.objectAtIndex(index))?;
            let expected = NSRect::new(
                NSPoint::new(
                    tile.x as f64 / publication.scale,
                    tile.bottom as f64 / publication.scale,
                ),
                NSSize::new(
                    tile.width as f64 / publication.scale,
                    tile.height as f64 / publication.scale,
                ),
            );
            let matches = checked(current, || {
                std::ptr::eq::<CALayer>(&*actual_slot, &**slots[index].container())
            })?;
            if !matches {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            drop(actual_slot);
            slots[index].validate(
                receipt,
                expected,
                publication.scale,
                &group.palette,
                current,
            )?;
        }
        drop(actual_tiles);
        self.require_mount(backing, current)?;
        if self.active.get() != Some(publication.index) || self.serial.get() != publication.serial {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    fn require_mount(&self, backing: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
        let parent = checked(current, || self.mount.superlayer())?;
        let matches = parent
            .as_deref()
            .is_some_and(|layer| std::ptr::eq::<CALayer>(layer, backing));
        drop(parent);
        if !matches || !checked(current, || self.mount.masksToBounds())? {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    pub(super) fn detach(&self, backing: &CALayer) -> bool {
        let parent = self.mount.superlayer();
        let foreign = parent
            .as_deref()
            .is_some_and(|parent| !std::ptr::eq(parent, backing));
        drop(parent);
        if parent_is(&self.mount, backing) {
            self.mount.removeFromSuperlayer();
        }
        // 外方重挂的mount保留那条parent关系；内部业务图片和本owner子关系仍必须归还。
        // mount的外方显示属性也必须保留并报告撤权；移除挂载关系不能洗掉整图隐藏或变透明。
        let mut intact = !foreign
            && !self.mount.isHidden()
            && self.mount.opacity() == 1.0
            && self.mount.zPosition() == 0.0
            && self.mount.masksToBounds();
        for group in &self.groups {
            let parent = group.layer.superlayer();
            let foreign_group = parent
                .as_deref()
                .is_some_and(|parent| !std::ptr::eq::<CALayer>(parent, &*self.mount));
            if foreign_group {
                intact = false;
            }
            drop(parent);
            if parent_is(&group.layer, &self.mount) {
                group.layer.removeFromSuperlayer();
            }
            let slot_clear = group.clear();
            intact &= slot_clear;
        }
        self.active.set(None);
        intact
    }
}

impl LayerGroup {
    fn new(current: &impl Fn() -> bool) -> Result<Self> {
        let layer = checked(current, CALayer::new)?;
        // 空组只有一个结构层；未激活的第二组不预占 64 个瓦片和 320 个辅助角色。
        Ok(Self {
            layer,
            slots: RefCell::new(Vec::new()),
            retired: RefCell::new(Vec::new()),
            palette: slot::Palette::new(current)?,
        })
    }
    fn clear(&self) -> bool {
        let mut intact = true;
        // 先移出有限槽，再发 Native 清理；关闭重入不会再次清同一槽或保留历史空层。
        let slots = std::mem::take(&mut *self.slots.borrow_mut());
        let mut retired = std::mem::take(&mut *self.retired.borrow_mut());
        // 历史身份只保 Weak；框架或外方仍持空角色时，关闭收据仍须观察它的真实生命周期。
        objc2::rc::autoreleasepool(|_| retired.retain(|weak| weak.load().is_some()));
        if retired.is_empty() {
            // 所有旧角色都已真实销毁，连 Weak 列表的历史容量也归还；活 Weak 一项不丢。
            retired = Vec::new();
        }
        for slot in &slots {
            slot.add_weaks(&mut retired);
        }
        self.retired.replace(retired); // 归还槽借用后才逐项发 Native 清理，允许同一关闭边界重入。
        for slot in &slots {
            intact &= slot.clear(&self.layer, &self.palette);
        }
        drop(slots); // 旧帧及缩窗淘汰的角色在本事务结束前归还，不成为闲置图层缓存。
        // 本组从不安装背景、mask、contents或delegate；外方后来设置的内容保持原样并拒绝复用。
        // SAFETY: 标准CALayer getter只核空值，不把外方opaque内容解引用为CGImage。
        let contents = unsafe { self.layer.contents() };
        // SAFETY: own slot已逐一移除；剩余children只能归属外方，不能清空整个数组夺走它们。
        let children = unsafe { self.layer.sublayers() };
        intact &= contents.is_none()
            && children.as_ref().is_none_or(|children| children.is_empty())
            && !self.layer.isHidden()
            && self.layer.opacity() == 1.0
            && self.layer.zPosition() == 0.0
            && self.layer.backgroundColor().is_none()
            && self.layer.mask().is_none()
            && self.layer.delegate().is_none();
        intact // 没有本项目设置的layer delegate；外方后设delegate不属于本owner，不能擅自清理。
    }
}

fn empty_container(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    // SAFETY: mount/group不安装业务contents；只核空值，不能把未知外方对象当作原图片。
    let contents = checked(current, || unsafe { layer.contents() })?;
    let background = checked(current, || layer.backgroundColor())?;
    let mask = checked(current, || layer.mask())?;
    let delegate = checked(current, || layer.delegate())?;
    // 原mount/group从不隐藏、变透明或改Z顺序；只验空载荷不足以证明实际输出仍相同。
    let unchanged = checked(current, || {
        !layer.isHidden() && layer.opacity() == 1.0 && layer.zPosition() == 0.0
    })?;
    if contents.is_none()
        && background.is_none()
        && mask.is_none()
        && delegate.is_none()
        && unchanged
    {
        Ok(())
    } else {
        Err(NativeMarkdownPagedTextViewError::Revoked)
    }
}

/// 原CALayer继承链直接借作AnyObject，弱登记不新增主线程强owner。
fn layer_weak(layer: &CALayer) -> Weak<AnyObject> {
    let object: &AnyObject = layer;
    Weak::from(object)
}

fn parent_is(layer: &CALayer, expected: &CALayer) -> bool {
    let parent = layer.superlayer();
    parent
        .as_deref()
        .is_some_and(|parent| std::ptr::eq(parent, expected))
}
