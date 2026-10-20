// Copyright 2015 Google Inc. All rights reserved.
// 原容器角色来自固定pulldown-cmark0.13.4；持久页只改变所有权与预算，MIT条款见LICENSE。

//! 全局活动容器spine的持久分页；不能为每个push/pop复制整段祖先前缀。

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::block_context::ContainerRole;
#[cfg(test)]
use crate::block_context::ContainerSpine;

/// 每次写时复制最多一页的32项；祖先页只共享Arc，不按depth复制。
const PAGE_ENTRIES: usize = 32;
/// page-pointer目录最多512槽，按两个实际2KiB片段延迟取得；遍历不递归。
pub(crate) const MAX_STRUCTURAL_DEPTH: usize = 16_384;
const MAX_SPINE_PAGES: usize = MAX_STRUCTURAL_DEPTH / PAGE_ENTRIES;
const CURSOR_DIRECTORY_SLOTS: usize = MAX_SPINE_PAGES / 2;
type CursorDirectory = [Option<Arc<SpinePage>>; CURSOR_DIRECTORY_SLOTS];
/// 所有checkpoint仍活的页payload合计最多4MiB，Arc/allocator开销不在此逻辑计量中。
pub(crate) const MAX_SPINE_PAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 只保存活动结构与源锚点；没有inline节点、label/title正文或已完成child链。
pub(crate) enum StructuralKind {
    Quote,
    List { marker: u8, tight: bool },
    Item { indent: usize },
    Definition { indent: usize },
    // 普通 ContainerRole 只证明续行的四列规则，不能推造首行 label 的结束位置。
    // 实际 producer 完成 label/colon/AllSpace 后才填写 body_start；NonZero 保持原页项宽度。
    Footnote { body_start: Option<NonZeroUsize> },
    Paragraph,
    Table,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StructuralEntry {
    pub(crate) kind: StructuralKind,
    pub(crate) source_start: usize,
}

impl StructuralEntry {
    pub(crate) fn for_role(role: ContainerRole, source_start: usize) -> Self {
        let kind = match role {
            ContainerRole::Quote => StructuralKind::Quote,
            ContainerRole::Item(indent) => StructuralKind::Item { indent },
            ContainerRole::Definition(indent) => StructuralKind::Definition { indent },
            ContainerRole::Footnote => StructuralKind::Footnote { body_start: None },
            ContainerRole::Other => StructuralKind::Other,
        };
        Self { kind, source_start }
    }
    /// 与AST view同一角色投影；list/leaf自身仍计作Other层，不能丢掉spine_len。
    pub(crate) fn role(self) -> ContainerRole {
        match self.kind {
            StructuralKind::Quote => ContainerRole::Quote,
            StructuralKind::Item { indent } => ContainerRole::Item(indent),
            StructuralKind::Definition { indent } => ContainerRole::Definition(indent),
            StructuralKind::Footnote { .. } => ContainerRole::Footnote,
            _ => ContainerRole::Other,
        }
    }
}

#[derive(Clone, Copy, Debug)]
/// 逻辑payload预算是同一producer全部存活页的总和，包含checkpoint继续持有的旧页。
pub struct StructuralSpineLimits {
    pub max_depth: usize,
    pub max_live_page_bytes: usize,
    pub max_page_allocations: usize,
    pub max_role_work: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 预算失败是显式未完成，不变成截断spine或“没有table”的成功结果。
pub enum StructuralSpineError {
    InvalidLimits,
    ConcurrentBudgetChange,
    Depth,
    LivePageBytes,
    AllocationFuel,
    RoleWorkFuel,
    InvalidIndex,
}

#[derive(Debug)]
struct SpineBudget {
    limits: StructuralSpineLimits,
    live_bytes: AtomicUsize,
    allocations: AtomicUsize,
    role_work: AtomicUsize,
}

/// 页的计量按实际项目payload类型大小；不宣称等于allocator/Arc内部头或整进程内存。
/// allocator overhead和调用者保存checkpoint handle的容器另归实际整产品内存验收。
const PAGE_PAYLOAD_BYTES: usize = std::mem::size_of::<SpinePage>();

impl SpineBudget {
    /// 原子增量在上限内才提交；Drop可在另一个Rust线程归还页，不重置单调fuel。
    fn reserve(
        counter: &AtomicUsize,
        amount: usize,
        maximum: usize,
        error: StructuralSpineError,
    ) -> Result<(), StructuralSpineError> {
        let mut old = counter.load(Ordering::Acquire);
        for _ in 0..32 {
            let next = old
                .checked_add(amount)
                .filter(|next| *next <= maximum)
                .ok_or(error)?;
            match counter.compare_exchange_weak(old, next, Ordering::AcqRel, Ordering::Acquire) {
                // 成功只提交本次增量；旧checkpoint并发归还不会被覆盖。
                Ok(_) => return Ok(()),
                // 最多32次CAS，不能在有界step中隐藏一个无期限自旋；失败没有本次reserve。
                Err(current) => old = current,
            }
        }
        Err(StructuralSpineError::ConcurrentBudgetChange)
    }

    /// 先reserve活容量，再consume不可退回的分配fuel；失败前绝不Arc::new。
    fn reserve_page(&self) -> Result<(), StructuralSpineError> {
        Self::reserve(
            &self.live_bytes,
            PAGE_PAYLOAD_BYTES,
            self.limits.max_live_page_bytes,
            StructuralSpineError::LivePageBytes,
        )?;
        if let Err(error) = Self::reserve(
            &self.allocations,
            1,
            self.limits.max_page_allocations,
            StructuralSpineError::AllocationFuel,
        ) {
            // 尚未分配页，归还仅本次reserve；历史allocation/workfuel绝不回退。
            self.live_bytes
                .fetch_sub(PAGE_PAYLOAD_BYTES, Ordering::AcqRel);
            return Err(error);
        }
        Ok(())
    }

    /// 角色复制和访问都收费；恢复旧checkpoint不能领取一份新的fuel跳过总体工作上限。
    fn work(&self, count: usize) -> Result<(), StructuralSpineError> {
        Self::reserve(
            &self.role_work,
            count,
            self.limits.max_role_work,
            StructuralSpineError::RoleWorkFuel,
        )
    }
}

#[derive(Debug)]
struct SpinePage {
    parent: Option<Arc<SpinePage>>,
    base: usize,
    entries: [Option<StructuralEntry>; PAGE_ENTRIES],
    budget: Arc<SpineBudget>,
}

impl Drop for SpinePage {
    fn drop(&mut self) {
        // 唯一页payload已经结束；只归还活容量，不做IO、等待或fuel重置。
        self.budget
            .live_bytes
            .fetch_sub(PAGE_PAYLOAD_BYTES, Ordering::AcqRel);
        let mut parent = self.parent.take();
        while let Some(shared) = parent {
            // into_inner以一次原子释放决定本线程是否取得末个owner；None不递归销毁T。
            // 唯一父页先take其parent，再drop空父页，使长链释放也保持常数Rust调用栈。
            match Arc::into_inner(shared) {
                Some(mut owned) => {
                    parent = owned.parent.take();
                    drop(owned);
                }
                None => break, // 其他checkpoint仍拥有祖先；本次只结束自己的边，绝不强制其释放。
            }
        }
    }
}

#[derive(Clone)]
/// checkpoint只共享末页+有效长度。被pop的后缀是纯标量，不影响旧checkpoint的前缀。
pub struct StructuralSpineSnapshot {
    tail: Option<Arc<SpinePage>>,
    len: usize,
    budget: Arc<SpineBudget>,
}
impl std::fmt::Debug for StructuralSpineSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不能沿Arc父链递归打印所有历史页；诊断只有有限depth/活payload/fuel标量。
        f.debug_struct("StructuralSpineSnapshot")
            .field("depth", &self.len)
            .field(
                "live_page_bytes",
                &self.budget.live_bytes.load(Ordering::Acquire),
            )
            .field(
                "page_allocations",
                &self.budget.allocations.load(Ordering::Acquire),
            )
            .finish()
    }
}

impl StructuralSpineSnapshot {
    /// 同一前沿回放源前缀时从空根开始；旧恢复点与新页继续共享原库存及不可退还的工作量。
    pub(crate) fn empty_shared(&self) -> Self {
        Self {
            tail: None,
            len: 0,
            budget: Arc::clone(&self.budget),
        }
    }

    /// 空spine本身不分配页；所有页的reserve发生在真正push之前。
    pub(crate) fn new(limits: StructuralSpineLimits) -> Result<Self, StructuralSpineError> {
        if limits.max_depth > MAX_STRUCTURAL_DEPTH
            || limits.max_live_page_bytes > MAX_SPINE_PAGE_BYTES
        {
            // 硬深度界限同时保证固定pointer数组可容纳真实全部页；不接受使遍历越界的配置。
            return Err(StructuralSpineError::InvalidLimits);
        }
        Ok(Self {
            tail: None,
            len: 0,
            budget: Arc::new(SpineBudget {
                limits,
                live_bytes: AtomicUsize::new(0),
                allocations: AtomicUsize::new(0),
                role_work: AtomicUsize::new(0),
            }),
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 克隆/恢复继续共享同一预算与页；不把常数checkpoint大小冒充producer总容量。
    #[cfg(test)]
    pub(crate) fn shares_tail(&self, other: &Self) -> bool {
        self.len == other.len
            && match (&self.tail, &other.tail) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => Arc::ptr_eq(&self.budget, &other.budget),
                _ => false,
            }
    }

    /// 只在有限上下文查询中使用；从tail查一项最多经过ceil(depth/32)页。
    /// root-first全遍历须用visit_roles，不可在循环中反复调用本方法形成depth²。
    pub(crate) fn entry(&self, index: usize) -> Option<StructuralEntry> {
        if index >= self.len {
            return None;
        }
        let mut page = self.tail.as_deref();
        while let Some(current) = page {
            if index >= current.base {
                return current.entries[index - current.base];
            }
            page = current.parent.as_deref();
        }
        None
    }

    /// 真实单层 Root 只需一页一项；沿同一角色账本收费，不为此初始化通用深树游标的4KiB槽。
    pub(crate) fn sole_entry(&self) -> Result<Option<StructuralEntry>, StructuralSpineError> {
        if self.len != 1 {
            return Ok(None);
        }
        self.budget.work(2)?;
        Ok(self.tail.as_ref().and_then(|page| page.entries[0]))
    }

    /// push至多复制32项；唯一页可原位写，不为没有checkpoint的普通解析创建无用版本。
    pub(crate) fn push(&mut self, entry: StructuralEntry) -> Result<(), StructuralSpineError> {
        if self.len == self.budget.limits.max_depth {
            return Err(StructuralSpineError::Depth);
        }
        let used = self.tail.as_ref().map_or(0, |page| self.len - page.base);
        if used < PAGE_ENTRIES {
            if let Some(page) = self.tail.as_mut().and_then(Arc::get_mut) {
                // 没有其他snapshot拥有该页，写入不会改变任一已发布checkpoint。
                self.budget.work(1)?;
                page.entries[used] = Some(entry);
                self.len += 1;
                return Ok(());
            }
        }
        let copy_count = if used < PAGE_ENTRIES { used } else { 0 };
        self.budget.work(copy_count + 1)?;
        self.budget.reserve_page()?;
        let (parent, base) = if copy_count != 0 {
            let old = self.tail.as_ref().expect("非空部分页有tail");
            (old.parent.clone(), old.base)
        } else {
            (self.tail.clone(), self.len)
        };
        let mut entries = [None; PAGE_ENTRIES];
        if copy_count != 0 {
            // 只复制当前有效的部分页，历史已pop后缀不被重新发布。
            entries[..copy_count]
                .copy_from_slice(&self.tail.as_ref().unwrap().entries[..copy_count]);
        }
        entries[copy_count] = Some(entry);
        self.tail = Some(Arc::new(SpinePage {
            parent,
            base,
            entries,
            budget: Arc::clone(&self.budget),
        }));
        self.len += 1;
        Ok(())
    }

    /// pop仅移动有效长度/末页，不分配。共享旧checkpoint继续看原长度。
    pub(crate) fn pop(&mut self) -> Result<Option<StructuralEntry>, StructuralSpineError> {
        let Some(index) = self.len.checked_sub(1) else {
            // 空spine没有资源转换，也不伪造容器关闭事实。
            return Ok(None);
        };
        let result = self
            .entry(index)
            .ok_or(StructuralSpineError::InvalidIndex)?;
        self.budget.work(1)?;
        self.len -= 1;
        if self.tail.as_ref().is_some_and(|page| self.len == page.base) {
            // 进入上一整页时只clone一个Arc；不复制全部祖先或角色。
            self.tail = self.tail.as_ref().and_then(|page| page.parent.clone());
        }
        Ok(Some(result))
    }

    /// 只更新末项或祖父项；最多两页copy，不能提供任意深层set导致整前缀复制。
    pub(crate) fn update_near_top(
        &mut self,
        distance: usize,
        entry: StructuralEntry,
    ) -> Result<(), StructuralSpineError> {
        if distance > 1 || distance >= self.len {
            return Err(StructuralSpineError::InvalidIndex);
        }
        let index = self.len - 1 - distance;
        self.budget.work(PAGE_ENTRIES * 2)?;
        Self::replace_near(&mut self.tail, index, &self.budget).map(|slot| *slot = Some(entry))
    }

    /// 递归只可能跨tail边界一次；原spine深度不影响写时复制路径长度。
    fn replace_near<'a>(
        tail: &'a mut Option<Arc<SpinePage>>,
        index: usize,
        budget: &Arc<SpineBudget>,
    ) -> Result<&'a mut Option<StructuralEntry>, StructuralSpineError> {
        if tail.is_none() {
            return Err(StructuralSpineError::InvalidIndex);
        }
        if Arc::get_mut(tail.as_mut().unwrap()).is_none() {
            // checkpoint共享时先准入这一页，再复制固定32项；父页仍只借一个Arc。
            budget.reserve_page()?;
            let old = tail.as_ref().unwrap();
            let copy = SpinePage {
                parent: old.parent.clone(),
                base: old.base,
                entries: old.entries,
                budget: Arc::clone(budget),
            };
            *tail = Some(Arc::new(copy));
        }
        let current = Arc::get_mut(tail.as_mut().unwrap()).expect("刚创建或唯一的页");
        if index >= current.base {
            Ok(&mut current.entries[index - current.base])
        } else {
            Self::replace_near(&mut current.parent, index, budget)
        }
    }

    /// root-first遍历每页/角色一次，无递归、Vec或nth重复爬链。
    /// 调用者在step中须先预留role_visit_work再charge，不能把同步visitor视为零工作。
    #[cfg(test)]
    pub(crate) fn role_visit_work(&self) -> usize {
        self.len + (self.len + PAGE_ENTRIES - 1) / PAGE_ENTRIES
    }
    #[cfg(test)]
    pub(crate) fn charge_role_visit(&self) -> Result<(), StructuralSpineError> {
        self.budget.work(self.role_visit_work())
    }
    pub(crate) fn work_used(&self) -> usize {
        self.budget.role_work.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn payload_counts(&self) -> (usize, usize, usize) {
        (
            self.budget.live_bytes.load(Ordering::Acquire),
            self.budget.allocations.load(Ordering::Acquire),
            self.budget.role_work.load(Ordering::Acquire),
        )
    }

    #[cfg(test)]
    pub(crate) fn from_roles(roles: &[ContainerRole]) -> Self {
        let mut snapshot = Self::new(StructuralSpineLimits {
            max_depth: roles.len(),
            max_live_page_bytes: MAX_SPINE_PAGE_BYTES,
            max_page_allocations: usize::MAX,
            max_role_work: usize::MAX,
        })
        .expect("真实测试capture在硬深度上限内");
        for role in roles {
            snapshot
                .push(StructuralEntry::for_role(*role, 0))
                .expect("测试真实capture已给足有限预算");
        }
        snapshot
    }
}

/// 单个活动prefix续体最多持有两片page-pointer目录；空scope不分配目录，也不保存源正文。
pub(crate) struct StructuralSpineCursor {
    pages: [Option<Box<CursorDirectory>>; 2],
    building: Option<Arc<SpinePage>>,
    count: usize,
    emitted: usize,
    len: usize,
    budget: Arc<SpineBudget>,
}

pub(crate) enum StructuralCursorStep {
    Building,
    Entry {
        index: usize,
        entry: StructuralEntry,
    },
    End,
}

impl StructuralSpineSnapshot {
    /// 构造只持原tail和两个空槽；目录随真实page重建分片取得，祖先Arc仍逐步收费。
    pub(crate) fn cursor(&self) -> StructuralSpineCursor {
        StructuralSpineCursor {
            pages: [None, None],
            building: self.tail.clone(),
            count: 0,
            emitted: 0,
            len: self.len,
            budget: Arc::clone(&self.budget),
        }
    }
}

impl StructuralSpineCursor {
    /// 仅报告本cursor实际取得的目录payload；外层引用事务据此计入同一批次库存。
    pub(crate) fn allocated_workspace_bytes(&self) -> usize {
        self.pages.iter().filter(|page| page.is_some()).count()
            * std::mem::size_of::<CursorDirectory>()
    }

    /// 一次只移交一页或一个role；外层决定本step剩余transition是否足够，不藏整depth循环。
    pub(crate) fn next(&mut self) -> Result<StructuralCursorStep, StructuralSpineError> {
        if let Some(page) = self.building.as_ref() {
            self.budget.work(1)?;
            let next = page.parent.clone();
            let directory = self.count / CURSOR_DIRECTORY_SLOTS;
            // 一次真实page交接至多新建一片2KiB目录，不在小请求owner里预埋整个4KiB数组。
            let slots = self.pages[directory]
                .get_or_insert_with(|| Box::new(std::array::from_fn(|_| None)));
            slots[self.count % CURSOR_DIRECTORY_SLOTS] = self.building.take();
            self.count += 1;
            self.building = next;
            return Ok(StructuralCursorStep::Building);
        }
        if self.emitted == self.len {
            return Ok(StructuralCursorStep::End);
        }
        self.budget.work(1)?;
        let index = self.emitted;
        // 页base固定为32的倍数；只对已经按root-first建立的page槽取值，不重复爬parent。
        let page_index = self.count - 1 - index / PAGE_ENTRIES;
        let page = self.pages[page_index / CURSOR_DIRECTORY_SLOTS]
            .as_ref()
            .expect("完整cursor已经取得真实目录片")[page_index % CURSOR_DIRECTORY_SLOTS]
            .as_ref()
            .expect("完整cursor已经建立全部page槽");
        let entry = page.entries[index - page.base].expect("有效长度内有真实entry");
        self.emitted += 1;
        Ok(StructuralCursorStep::Entry { index, entry })
    }
}

#[cfg(test)]
impl ContainerSpine for StructuralSpineSnapshot {
    fn visit_roles(&self, mut visit: impl FnMut(ContainerRole) -> bool) {
        // 固定数组只借page，不cloneArc，不引入heap或递归栈；硬max_depth先保证槽位足够。
        let mut pages = [None; MAX_SPINE_PAGES];
        let mut count = 0;
        let mut next = self.tail.as_deref();
        while let Some(page) = next {
            pages[count] = Some(page);
            count += 1;
            next = page.parent.as_deref();
        }
        for page in pages[..count].iter().rev().flatten() {
            // 有效len裁掉已pop后缀；按真实root→leaf顺序，首false立即停止。
            for entry in page
                .entries
                .iter()
                .take((self.len - page.base).min(PAGE_ENTRIES))
                .flatten()
            {
                if !visit(entry.role()) {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
