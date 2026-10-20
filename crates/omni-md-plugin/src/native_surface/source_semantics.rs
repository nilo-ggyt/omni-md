//! 一个storage只有一个来源续体；有限step由真实可见准备接入，getter只消费同源已发布结果。
//!
//! 首次同步TextKit排版、逆向缺checkpoint及未覆盖语法仍保留旧路径，不能据此声称严格懒加载。

use super::*;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownOrdinarySemanticState, MarkdownTableRequestError, MarkdownTableViewportCoverage,
    MarkdownViewportSourceAuthority, MarkdownViewportSourceRequest, MarkdownViewportSourceStep,
    VerifiedMarkdownViewportSources,
};
use std::ops::Range;

const SOURCE_STEP: usize = 4096;
const MAX_SOURCE_VIEWPORT: usize = 16_384;

#[derive(Clone)]
/// 原输入上下文与完整方向固定后不可补签；字体scratch和表格几何变化不改变Core来源身份。
struct SourceKey {
    document: WeakMarkdownDocument,
    generation: u64,
    length: usize,
    selection: MarkdownTextSelection,
    semantic_epoch: u64,
    input_epoch: u64,
}

/// Core完成与Native提交是两个边界；候选不参与getter，旧结果保留到同批Native键一并切换。
#[derive(Default)]
struct SourcePublications {
    committed: Option<Rc<VerifiedMarkdownViewportSources>>,
    staged: Option<Rc<VerifiedMarkdownViewportSources>>,
}

impl SourcePublications {
    fn stage(&mut self, source: VerifiedMarkdownViewportSources) -> bool {
        if self.staged.is_some() {
            return false; // 一个原请求只容许一个待提交publication，不覆盖尚未完成Native提交的候选。
        }
        self.staged = Some(Rc::new(source));
        true
    }

    /// 调用者须先完成同源/settled核验，随后无Native调用地切换字体和预览键。
    fn commit_staged(&mut self) -> Option<Option<Rc<VerifiedMarkdownViewportSources>>> {
        let source = self.staged.take()?;
        Some(self.committed.replace(source))
    }
}

impl SourceKey {
    fn matches(&self, other: &Self) -> bool {
        std::rc::Weak::ptr_eq(&self.document, &other.document)
            && self.generation == other.generation
            && self.length == other.length
            && self.selection == other.selection
            && self.semantic_epoch == other.semantic_epoch
            && self.input_epoch == other.input_epoch
    }
    fn authority(&self) -> MarkdownViewportSourceAuthority {
        MarkdownViewportSourceAuthority {
            context_revision: self.semantic_epoch,
            selection: self.selection,
            cancelled: false,
        }
    }
}

/// 请求和publication都只由本storage槽保有；排队任务只有Weak，关闭不会被续体保活。
pub(in crate::native_surface) struct NativeMarkdownSourceOwner {
    key: SourceKey,
    view: Weak<PieceBackedMarkdownTextView>,
    request: RefCell<Option<MarkdownViewportSourceRequest>>,
    publications: RefCell<SourcePublications>,
    window: RefCell<Range<usize>>,
    desired: RefCell<Range<usize>>,
    pending: Cell<bool>,
    publication_pending: Cell<bool>,
    queued: Cell<bool>,
    failed: Cell<bool>,
}

impl NativeMarkdownSourceOwner {
    fn stop(&self) {
        self.pending.set(false);
        self.publication_pending.set(false);
        self.failed.set(true);
        let staged = self.publications.borrow_mut().staged.take();
        drop(staged); // 失败候选不得泄漏给getter；旧committed仍只能通过自身原来源核验读取。
        let mut request = self.request.borrow_mut().take();
        if let Some(request) = request.as_mut() {
            request.cancel(); // 原Core续体同步固定取消并释放scratch，不等待Weak任务再次运行。
        }
        drop(request);
    }
}

#[derive(Clone)]
/// 无表证明只弱持同一Sources publication，既不复制目录也不把普通gap升级为Plain。
pub(in crate::native_surface) struct NativeMarkdownNoTablesProof {
    source: std::rc::Weak<VerifiedMarkdownViewportSources>,
    window: Range<usize>,
}

impl NativeMarkdownNoTablesProof {
    pub(in crate::native_surface) fn is_current(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        document: &MarkdownDocumentBuffer,
    ) -> bool {
        let Some(owner) = storage.current_source_owner() else {
            return false; // 新输入或关闭不能借相同数值保活旧无表收据。
        };
        let Some(source) = self.source.upgrade() else {
            return false; // 原publication已离开唯一槽，不重建另一份同坐标来源。
        };
        let owns = owner
            .publications
            .borrow()
            .committed
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, &source));
        owns && no_tables_covered(&source, document, owner.key.authority(), &self.window)
    }
}

/// 只接受同一Sources中真实Gap的连续覆盖；相交Table、空洞和覆盖外一律没有无表权限。
fn no_tables_covered(
    sources: &VerifiedMarkdownViewportSources,
    document: &MarkdownDocumentBuffer,
    authority: MarkdownViewportSourceAuthority,
    requested: &Range<usize>,
) -> bool {
    if requested.is_empty() {
        return false; // 空快照不能被当作一份已经完成的可见语义。
    }
    let Ok(table) = sources.table_sources_for(document, authority) else {
        return false;
    };
    let Ok(coverage) = table.coverage_for(document) else {
        return false;
    };
    let mut cursor = requested.start;
    for part in coverage {
        let range = part.utf16_range();
        if range.end <= cursor {
            continue; // 真实目录按源序排列，只跳过当前请求之前的已证段。
        }
        if range.start > cursor {
            return false; // 不能用首尾gap的包络填补未知中段。
        }
        let MarkdownTableViewportCoverage::Gap { gap_index, .. } = part else {
            return false; // 当前位置属于真实Table，必须继续原表格功能路径。
        };
        let Ok(Some(gap)) = table.gap_at_for(document, *gap_index) else {
            return false;
        };
        let Ok(actual) = gap.utf16_range_for(document) else {
            return false;
        };
        if actual.start > range.start || actual.end < range.end {
            return false; // coverage必须仍由这一原gap记录持有，不能只比较几个整数。
        }
        cursor = range.end.min(requested.end);
        if cursor == requested.end {
            return true; // 每个字符都有真实无表证明；仍未声明普通inline样式已完成。
        }
    }
    false
}

/// 来源优化不能撤销正在等待原几何/paint的前台动作；只在真实settled事件后消费一次发布。
fn publication_is_settled(view: &PieceBackedMarkdownTextView) -> bool {
    let settled = !view.ivars().textkit_graph_teardown.get()
        && view.ivars().textkit_input_depth.get() == 0
        && !view.ivars().table_layout_rebuilding.get()
        && view
            .ivars()
            .marked_text_range
            .get()
            .is_none_or(|range| range.length == 0)
        && view.ivars().find_reveal.borrow().is_none()
        && view.ivars().mode_viewport.borrow().is_none()
        && view.ivars().table_mutation_anchor.borrow().is_none()
        && view.ivars().active_table_cell.borrow().is_none()
        && view.ivars().pending_table_cell_activation.is_empty();
    #[cfg(feature = "native-table-row-append")]
    {
        settled && !view.table_append_pending() // 原末格续体尚持提交权时，也不能让新派生键撤旧几何。
    }
    #[cfg(not(feature = "native-table-row-append"))]
    {
        settled
    }
}

impl PieceBackedMarkdownTextStorage {
    fn source_key(&self) -> Option<SourceKey> {
        if !self.ivars().accepts_edits.get()
            || self.ivars().composition_active.get()
            || self.ivars().native_input_depth.get() != 0
            || self.ivars().input_transaction.borrow().is_some()
        {
            return None; // 临时输入阶段不推进或发布派生来源，后继原准备事件可重新申请。
        }
        let semantic_epoch = self.ivars().presentation_semantic_epoch.get()?;
        let document = self.document()?;
        let current = document.borrow();
        Some(SourceKey {
            document: Rc::downgrade(&document),
            generation: current.generation(),
            length: current.utf16_len(),
            selection: current.selection(),
            semantic_epoch,
            input_epoch: self.ivars().input_transaction_epoch.get(),
        })
    }

    fn current_source_owner(&self) -> Option<Rc<NativeMarkdownSourceOwner>> {
        let key = self.source_key()?;
        let owner = self
            .ivars()
            .source_semantics
            .borrow()
            .as_ref()
            .map(Rc::clone)?;
        owner.key.matches(&key).then_some(owner)
    }

    fn owns_source(&self, original: &Rc<NativeMarkdownSourceOwner>) -> bool {
        self.current_source_owner()
            .is_some_and(|current| Rc::ptr_eq(&current, original))
    }

    /// 实际编辑/选择/模式撤权和accepted release共用取消边界，不在getter重建Global。
    pub(super) fn cancel_source_semantics(&self) {
        let owner = self.ivars().source_semantics.borrow_mut().take();
        if let Some(owner) = owner {
            owner.stop();
        } // 槽借用先结束，析构有限Core资源也不持正文或Native强引用。
    }

    /// 仅由settled可见准备调用；一个活上下文只建一份原SourceRequest，后续复用真实forward。
    pub(in crate::native_surface) fn prepare_source_semantics(
        &self,
        view: &PieceBackedMarkdownTextView,
        viewport: Range<usize>,
    ) {
        let Some(key) = self.source_key() else {
            return;
        };
        if viewport.is_empty() || viewport.len() > MAX_SOURCE_VIEWPORT || viewport.end > key.length
        {
            return; // 只接受原生可见行得出的有限范围，不扩大到paragraph或文档边界。
        }
        let owner = if let Some(owner) = self.current_source_owner() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            if *owner.desired.borrow() != viewport {
                native_markdown_trace! {
                    // 只记录真实请求窗口变化，不在每个 Pending 量子或属性 getter 中输出。
                    eprintln!("native_source_semantics event=prepare_existing generation={} selection={:?} semantic_epoch={} input_epoch={} window={:?} desired={:?} requested={viewport:?} pending={} publication_pending={} failed={} dirty={} formal=false",
                        owner.key.generation, owner.key.selection, owner.key.semantic_epoch, owner.key.input_epoch,
                        *owner.window.borrow(), *owner.desired.borrow(), owner.pending.get(), owner.publication_pending.get(),
                        owner.failed.get(), view.ivars().source_decorations_dirty.get());
                }
            }
            *owner.desired.borrow_mut() = viewport;
            owner // 同一上下文的失败/逆向也不会通过重新构造工厂获得新Global和额度。
        } else {
            self.cancel_source_semantics();
            let request = key.document.upgrade().and_then(|document| {
                let request = document
                    .borrow()
                    .viewport_leaf_source_request(
                        viewport.clone(),
                        key.semantic_epoch,
                        key.selection,
                    );
                #[cfg(feature = "native-markdown-runtime-probe")]
                if let Err(error) = &request {
                    native_markdown_trace! {
                        eprintln!("native_source_semantics event=create_failed generation={} requested={viewport:?} error={error:?} formal=false", key.generation);
                    }
                }
                request.ok()
            });
            let failed = request.is_none();
            let owner = Rc::new(NativeMarkdownSourceOwner {
                key,
                view: Weak::from(view),
                request: RefCell::new(request),
                publications: RefCell::new(SourcePublications::default()),
                window: RefCell::new(viewport.clone()),
                desired: RefCell::new(viewport),
                pending: Cell::new(!failed),
                publication_pending: Cell::new(false),
                queued: Cell::new(false),
                failed: Cell::new(failed),
            });
            let previous = self
                .ivars()
                .source_semantics
                .replace(Some(Rc::clone(&owner)));
            drop(previous); // 新槽已发布且RefCell借用结束，再释放先前纯Core owner。
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_trace! {
                eprintln!("native_source_semantics event=prepare_new generation={} length={} selection={:?} semantic_epoch={} input_epoch={} window={:?} desired={:?} pending={} failed={} dirty={} formal=false",
                    owner.key.generation, owner.key.length, owner.key.selection, owner.key.semantic_epoch, owner.key.input_epoch,
                    *owner.window.borrow(), *owner.desired.borrow(), owner.pending.get(), owner.failed.get(),
                    view.ivars().source_decorations_dirty.get());
            }
            owner // 失败也保留无payload tombstone，不能每次draw重新发放同一上下文预算。
        };
        if !owner.pending.get() && !owner.failed.get() {
            self.forward_source_if_needed(&owner);
        }
        self.enqueue_source_step(&owner);
    }

    fn forward_source_if_needed(&self, owner: &Rc<NativeMarkdownSourceOwner>) {
        if !self.owns_source(owner)
            || owner.pending.get()
            || owner.failed.get()
            || owner.publication_pending.get()
        {
            return;
        }
        let desired = owner.desired.borrow().clone();
        let window = owner.window.borrow().clone();
        if window.start <= desired.start && desired.end <= window.end {
            return; // 当前publication已覆盖请求窗口，重复准备没有新工作。
        }
        let Some(previous) = owner
            .publications
            .borrow()
            .committed
            .as_ref()
            .map(Rc::clone)
        else {
            return;
        };
        let Some(document) = owner.key.document.upgrade() else {
            owner.stop();
            return;
        };
        let result = owner
            .request
            .borrow_mut()
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)
            .and_then(|request| {
                request.continue_forward(
                    &document.borrow(),
                    &previous,
                    desired.clone(),
                    owner.key.authority(),
                )
            });
        match result {
            Ok(()) => {
                *owner.window.borrow_mut() = desired;
                owner.pending.set(true); // 原Global和联合quota已接受唯一后继，不能提前发布Ready。
            }
            Err(error @ MarkdownTableRequestError::CoverageRequiresCheckpoint) => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_trace! {
                    eprintln!("native_source_semantics event=forward_failed generation={} window={window:?} desired={desired:?} error={error:?} formal=false", owner.key.generation);
                }
                let _ = error; // 非诊断构建仍保原具体分支，不改变继续旧功能路径的语义。
                // 逆向缺checkpoint仍走原功能路径；保留原请求，禁止从0偷偷重建Global。
            }
            Err(error) => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_trace! {
                    eprintln!("native_source_semantics event=forward_failed generation={} window={window:?} desired={desired:?} error={error:?} formal=false", owner.key.generation);
                }
                let _ = error;
                owner.stop(); // sticky失败不重发预算；旧只读publication仍由原核验决定权限。
            }
        }
    }

    fn enqueue_source_step(&self, owner: &Rc<NativeMarkdownSourceOwner>) {
        if !self.owns_source(owner)
            || !owner.pending.get()
            || owner.failed.get()
            || owner.queued.replace(true)
        {
            return; // 一个原owner最多排一个续体，不按属性请求或重复绘制增加任务。
        }
        let Some(mtm) = MainThreadMarker::new() else {
            owner.queued.set(false);
            owner.stop();
            return;
        };
        let weak = MainThreadBound::new((Weak::from(self), Rc::downgrade(owner)), mtm);
        enqueue_native_markdown_main_task(move || {
            // SAFETY: 唯一executor在main queue运行；MainThreadBound只携Weak，不保活旧图或Core资源。
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let (storage, owner) = weak.get(mtm);
            let (Some(storage), Some(owner)) = (storage.load(), owner.upgrade()) else {
                return;
            };
            owner.queued.set(false);
            storage.advance_source_step(&owner);
        });
    }

    fn advance_source_step(&self, owner: &Rc<NativeMarkdownSourceOwner>) {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if !self.owns_source(owner) || !owner.pending.get() || owner.failed.get() {
            return; // 旧队列不能重新领取新owner或恢复取消的来源。
        }
        let Some(view) = owner.view.load() else {
            owner.stop();
            return;
        };
        let original_storage = view.ivars().text_storage.load();
        let same_storage = original_storage.as_ref().is_some_and(|original| {
            std::ptr::eq::<PieceBackedMarkdownTextStorage>(&**original, self)
        });
        drop(original_storage); // weak临时+1先结束，再核原owner；不把另一套view图当作本来源的发布端。
        if !same_storage || !self.owns_source(owner) || view.ivars().textkit_graph_teardown.get() {
            owner.stop();
            return; // weak加载也可能释放Native对象；回到纯Core前重新守原票。
        }
        let Some(document) = owner.key.document.upgrade() else {
            owner.stop();
            return;
        };
        let step = {
            let current = document.borrow();
            let mut request = owner.request.borrow_mut();
            request
                .as_mut()
                .ok_or(MarkdownTableRequestError::InvalidRequest)
                .and_then(|request| request.advance(&current, owner.key.authority(), SOURCE_STEP))
        }; // 所有Core/RefCell借用在Native发布或下一个queue之前结束。
        match step {
            Ok(MarkdownViewportSourceStep::Progress { work }) => {
                if work.source_bytes == 0
                    && work.structural_transitions == 0
                    && work.metadata_bytes == 0
                {
                    owner.stop();
                    return; // 没有真实工作不能排轮询；缺进展明确终止而非等待到Ready。
                }
                self.enqueue_source_step(owner); // 每次只一个有限量子，累计fuel仍属于原Core请求。
            }
            Ok(MarkdownViewportSourceStep::SourcesAvailable { .. }) => {
                let source = {
                    let current = document.borrow();
                    owner.request.borrow_mut().as_mut().and_then(|request| {
                        request
                            .take_sources_for(&current, owner.key.authority())
                            .ok()
                            .flatten()
                    })
                };
                let Some(source) = source else {
                    owner.stop();
                    return;
                };
                if !self.owns_source(owner) {
                    return;
                }
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_trace! {
                    // 仅读本次真实 SourcesAvailable 的既有语义标签，不解析、物化正文或推进来源。
                    let state = document.borrow();
                    let semantic = match source.ordinary_semantics_for(&state, owner.key.authority()) {
                        Ok(MarkdownOrdinarySemanticState::SourcesOnly) => "sources_only",
                        Ok(MarkdownOrdinarySemanticState::Pending(_)) => "pending",
                        Ok(MarkdownOrdinarySemanticState::PlainReady(_)) => "plain_ready",
                        Ok(MarkdownOrdinarySemanticState::LeafPending(reason)) => {
                            eprintln!("native_source_semantics event=leaf_pending reason={reason:?} generation={} window={:?} formal=false", owner.key.generation, *owner.window.borrow());
                            "leaf_pending"
                        }
                        Ok(MarkdownOrdinarySemanticState::RootLeafReady(_)) => "root_leaf_ready",
                        Err(_) => "revoked",
                    };
                    eprintln!("native_source_semantics event=sources_available generation={} selection={:?} semantic_epoch={} input_epoch={} window={:?} desired={:?} semantic={semantic} dirty={} formal=false",
                        owner.key.generation, owner.key.selection, owner.key.semantic_epoch, owner.key.input_epoch,
                        *owner.window.borrow(), *owner.desired.borrow(), view.ivars().source_decorations_dirty.get());
                }
                let staged = { owner.publications.borrow_mut().stage(source) };
                if !staged {
                    owner.stop();
                    return; // 未消费的候选不能被另一轮完成覆盖；不临时制造第三份同活publication。
                }
                owner.pending.set(false);
                owner.publication_pending.set(true);
                // 只让原settled executor处理Native副作用；这里不能绕开TextKit外层编辑事务检查。
                if self.owns_source(owner) {
                    view.schedule_visible_table_layout_update();
                } // 候选尚未提交时不forward；Core最后publication与Native当前publication保持可追溯。
            }
            Err(error) => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_trace! {
                    eprintln!("native_source_semantics event=advance_failed generation={} window={:?} desired={:?} error={error:?} formal=false", owner.key.generation, *owner.window.borrow(), *owner.desired.borrow());
                }
                let _ = error;
                owner.stop(); // 取消、源变化或预算不足都释放续体，不循环重试相同事实。
            }
        }
    }

    fn publish_source_ready(
        &self,
        owner: &Rc<NativeMarkdownSourceOwner>,
        view: &PieceBackedMarkdownTextView,
    ) {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if !self.owns_source(owner)
            || !publication_is_settled(view)
            || !owner.publication_pending.get()
        {
            return; // 仍在Find/Mode/table输入时只保候选；getter继续读旧committed，不能提前切换语义。
        }
        let ranges = owner.key.document.upgrade().and_then(|document| {
            let current = document.borrow();
            let publications = owner.publications.borrow();
            let source = publications.staged.as_ref()?;
            match source
                .ordinary_semantics_for(&current, owner.key.authority())
                .ok()?
            {
                MarkdownOrdinarySemanticState::RootLeafReady(ready) => Some(
                    ready
                        .covered_ranges_for(&current, owner.key.authority())
                        .ok()?
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                _ => Some(Vec::new()), // Sources/LeafPending只可提供真实表格Gap，不能通知伪普通样式。
            }
        });
        let Some(ranges) = ranges else {
            owner.stop();
            return; // 原publication核验失败不能按空成功发出Native通知。
        }; // 至多256个有限交集；不复制正文、runs或源目录。
        if !self.owns_source(owner) || !publication_is_settled(view) {
            return;
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_trace! {
            eprintln!("native_source_semantics event=publish_begin generation={} selection={:?} semantic_epoch={} input_epoch={} window={:?} desired={:?} ranges={ranges:?} dirty={} formal=false",
                owner.key.generation, owner.key.selection, owner.key.semantic_epoch, owner.key.input_epoch,
                *owner.window.borrow(), *owner.desired.borrow(), view.ivars().source_decorations_dirty.get());
        }
        let previous = owner.publications.borrow_mut().commit_staged();
        let Some(previous) = previous else {
            owner.stop();
            return; // 标志不能代替真实候选，绝不重新读取当前getter拼一份成功publication。
        };
        owner.publication_pending.set(false);
        if !ranges.is_empty() {
            // 只有真实RootLeaf样式交集才改变Native属性。表格窗口的Sources/LeafPending
            // 只新增无表/目录证明，没有任何普通样式发布；不能因空通知撤销已验证的表格高度，
            // 否则getter立即恢复literal，再在下一轮重建相同高度，破坏刚完成的视口几何。
            self.ivars().font_run_scratch.invalidate();
            self.advance_table_geometry_epoch(); // 实际代码/分隔线样式可改变前置行高，仍必须撤旧几何。
            // 字典按style/font/variant区分；只撤在途loan，不能在提交中段触发额外Native析构。
            self.ivars().attribute_cache.invalidate_loans();
        } // 零样式交集仍正常提交原publication/目录与续体预算，不伪造普通内容Ready或取消来源任务。
        drop(previous); // 原候选和字体键已同批提交；旧同活库存此时释放，不留下publication历史链。
        // Preview键本就包含font epoch；旧geometry/attempt同步失权，原scheduler会按新键准备。
        // 不在此调用release_preview_blocks，以免它的取消回调在首Native通知前清掉重入产生的新准备。
        // 只发送一次有限窗口metadata通知，避免多个Native通知之间又开始前台动作后补发旧批次。
        // 中间未知区仍由原功能查询；通知包络不授予任何未覆盖语义，也不会读取其正文。
        let invalidated = ranges
            .first()
            .zip(ranges.last())
            .map(|(first, last)| first.start..last.end);
        self.notify_or_defer_attribute_range(invalidated);
        if !self.owns_source(owner) {
            return; // Native通知重入后只停本批，不给后继上下文重签或回滚已经提交的纯metadata。
        }
        // 真实新覆盖可能刚好补齐旧 Find 查询的未知段；即使查询/视口数值未变，原空矩形批次也已过时。
        // 只让原 settled executor 重建有限装饰，不在发布栈内排版、执行 Find 或推进别的来源。
        view.ivars().source_decorations_dirty.set(true);
        view.schedule_visible_table_layout_update(); // 继续既有settled executor与Find/Mode流程。
        if self.owns_source(owner) {
            // publication只准备了原有限视口，不能用整篇逻辑bounds向AppKit申请新的正文backing。
            // 原helper取真实visibleRect并拒绝已关闭/异常几何；滚动与后继覆盖仍由同一executor重建。
            view.invalidate_visible_body(); // 有真实新publication才排绘制，不以draw推进Core或改变布局范围。
        }
        if self.owns_source(owner) {
            self.forward_source_if_needed(owner);
            self.enqueue_source_step(owner);
        } // 原Native提交完成后才用这一唯一新publication续原Global，并联合计费旧/新库存。
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_trace! {
            eprintln!("native_source_semantics event=publish_end generation={} current={} window={:?} desired={:?} pending={} publication_pending={} failed={} dirty={} formal=false",
                owner.key.generation, self.owns_source(owner), *owner.window.borrow(), *owner.desired.borrow(),
                owner.pending.get(), owner.publication_pending.get(), owner.failed.get(), view.ivars().source_decorations_dirty.get());
        }
    }

    /// 原scheduler只查询一个发布标志；没有真实新publication就不增加背景准备机会。
    pub(in crate::native_surface) fn source_publication_needs_preparation(
        &self,
        view: &PieceBackedMarkdownTextView,
    ) -> bool {
        publication_is_settled(view)
            && self
                .current_source_owner()
                .is_some_and(|owner| owner.publication_pending.get())
    }

    /// 仅从既有settled executor调用；迟到Core结果不会在绘制getter内通知或推进来源。
    pub(in crate::native_surface) fn finish_source_publication(
        &self,
        view: &PieceBackedMarkdownTextView,
    ) {
        let Some(owner) = self.current_source_owner() else {
            return;
        };
        if !owner.publication_pending.get() || !publication_is_settled(view) {
            return;
        }
        let Some(original) = owner.view.load() else {
            owner.stop();
            return;
        };
        if std::ptr::eq::<PieceBackedMarkdownTextView>(&*original, view) && self.owns_source(&owner)
        {
            self.publish_source_ready(&owner, view);
        } // 原Weak/view不匹配只放弃本次，不能替换owner的原发布端。
    }

    /// 属性和背景getter只做纯查询；None仍交原功能路径，不新建或推进任何来源请求。
    pub(in crate::native_surface) fn source_leaf_query(
        &self,
        index: usize,
    ) -> Option<omni_markdown_core::MarkdownPresentationQuery> {
        let owner = self.current_source_owner()?;
        let document = owner.key.document.upgrade()?;
        let current = document.borrow();
        let publications = owner.publications.borrow();
        let source = publications.committed.as_ref()?;
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = source
            .ordinary_semantics_for(&current, owner.key.authority())
            .ok()?
        else {
            return None;
        };
        leaf.query_for(&current, owner.key.authority(), index)
            .ok()
            .flatten()
    }

    /// 单独证明当前窗口无表格；普通gap的inline样式仍未知，不能据此隐藏或改写正文。
    pub(super) fn source_no_tables_proof(
        &self,
        window: &Range<usize>,
    ) -> Option<NativeMarkdownNoTablesProof> {
        let owner = self.current_source_owner()?;
        let document = owner.key.document.upgrade()?;
        let publications = owner.publications.borrow();
        let source = publications.committed.as_ref()?;
        let current = document.borrow();
        no_tables_covered(source, &current, owner.key.authority(), window).then(|| {
            NativeMarkdownNoTablesProof {
                source: Rc::downgrade(source),
                window: window.clone(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分别记录真实载荷复制与 Parser 输入；RootLeaf 可拥有已规范化 URI，正文仍不复制。
    struct SourceWork {
        copied_source_bytes: usize,
        parser_input_bytes: usize,
    }

    fn sources(
        source: &str,
    ) -> (
        tempfile::TempDir,
        MarkdownDocumentBuffer,
        VerifiedMarkdownViewportSources,
        SourceWork,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("原来源无表证明.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        let authority = authority(&document);
        let mut request = document
            .viewport_leaf_source_request(
                0..document.utf16_len(),
                authority.context_revision,
                authority.selection,
            )
            .unwrap();
        let (sources, work) = complete_request(&document, authority, &mut request);
        (directory, document, sources, work)
    }

    /// 一个有限测试驱动共用真实advance/take；同一请求forward后也不能另建Global绕过库存收费。
    fn complete_request(
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        request: &mut MarkdownViewportSourceRequest,
    ) -> (VerifiedMarkdownViewportSources, SourceWork) {
        for _ in 0..100_000 {
            let (MarkdownViewportSourceStep::Progress { work }
            | MarkdownViewportSourceStep::SourcesAvailable { work }) =
                request.advance(document, authority, SOURCE_STEP).unwrap();
            assert!(
                work.source_bytes <= SOURCE_STEP
                    && work.structural_transitions <= SOURCE_STEP
                    && work.metadata_bytes <= SOURCE_STEP
                    && work.copied_source_bytes <= SOURCE_STEP
                    && work.parser_input_bytes <= SOURCE_STEP
            );
            if let Some(sources) = request.take_sources_for(document, authority).unwrap() {
                // 表格按原预算复制有限行；RootLeaf 只准许真实 URI 输出片，具体用例核精确来源。
                let total = request.total_work();
                return (
                    sources,
                    SourceWork {
                        copied_source_bytes: total.copied_source_bytes,
                        parser_input_bytes: total.parser_input_bytes,
                    },
                );
            }
        }
        panic!("有限真实来源请求没有完成");
    }

    fn authority(document: &MarkdownDocumentBuffer) -> MarkdownViewportSourceAuthority {
        MarkdownViewportSourceAuthority {
            context_revision: 71,
            selection: document.selection(),
            cancelled: false,
        }
    }

    #[test]
    fn staged_forward_result_keeps_old_committed_publication_until_native_boundary() {
        let source = "```text\n第一段😀\n```\n\n```text\n第二段中文\n```\n";
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("同请求候选提交.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        let authority = authority(&document);
        let first_end = source[..source.find("\n\n").unwrap()]
            .encode_utf16()
            .count();
        let mut request = document
            .viewport_leaf_source_request(
                0..first_end,
                authority.context_revision,
                authority.selection,
            )
            .unwrap();
        let (first, first_work) = complete_request(&document, authority, &mut request);
        assert_eq!(first_work.copied_source_bytes, 0);
        assert_eq!(first_work.parser_input_bytes, 0);
        let mut publications = SourcePublications::default();
        assert!(publications.stage(first));
        assert!(
            publications.committed.is_none(),
            "首候选不能提前对getter可见"
        );
        assert!(publications.commit_staged().unwrap().is_none());
        let original = publications.committed.as_ref().unwrap();
        let original_weak = Rc::downgrade(original);
        let original_publication = Rc::as_ptr(original);

        // 续批始终消费同一真实last_publication；Core在此联合预扣旧结果和候选的metadata/runs。
        request
            .continue_forward(&document, original, 0..document.utf16_len(), authority)
            .unwrap();
        let (next, next_work) = complete_request(&document, authority, &mut request);
        assert_eq!(next_work.copied_source_bytes, 0);
        assert_eq!(next_work.parser_input_bytes, 0);
        assert!(publications.stage(next));
        assert_eq!(
            Rc::as_ptr(publications.committed.as_ref().unwrap()),
            original_publication,
            "未到Native提交边界，三个消费者仍只能取得旧committed"
        );
        assert!(no_tables_covered(
            publications.committed.as_ref().unwrap(),
            &document,
            authority,
            &(0..first_end)
        ));
        assert!(no_tables_covered(
            publications.staged.as_ref().unwrap(),
            &document,
            authority,
            &(0..document.utf16_len())
        ));

        let previous = publications.commit_staged().unwrap().unwrap();
        assert_eq!(Rc::as_ptr(&previous), original_publication);
        assert_ne!(
            Rc::as_ptr(publications.committed.as_ref().unwrap()),
            original_publication
        );
        drop(previous);
        assert!(
            original_weak.upgrade().is_none(),
            "提交后不保旧publication链"
        );
        assert!(publications.staged.is_none());
        assert!(
            publications.commit_staged().is_none(),
            "无候选不能补签一次提交"
        );
    }

    #[test]
    /// 同一无表窗口分别核来源票与真实语义票；已支持的直接链接必须取得自身完整语义，
    /// 来源专用工厂即使持相同 Gap，也不能借这个已完成的邻请求替自己补签语义。
    fn gap_proof_and_plain_semantics_keep_independent_authority() {
        // 保留原长 label、目标和代码夹具。直接链接现已真实支持，不能继续把旧 Unsupported
        // 当作权限负例；唯一复制是规范化目标，绝不能把 label 或整个 Root 文本混入该账。
        const DESTINATION: &str = "https://example.invalid";
        let source = format!(
            "普通正文\n\n[复杂{}仍未知](https://example.invalid)\n\n```text\n中文😀\n```\n\n尾部\n",
            "正文".repeat(400)
        );
        let (directory, document, sources, work) = sources(&source);
        let backing = document.original_backing_observer().unwrap();
        let authority = authority(&document);
        let window = 0..document.utf16_len();
        assert_eq!(work.copied_source_bytes, DESTINATION.len());
        assert_eq!(work.parser_input_bytes, 0);
        assert!(no_tables_covered(&sources, &document, authority, &window));
        assert!(!no_tables_covered(&sources, &document, authority, &(0..0)));
        assert!(!no_tables_covered(
            &sources,
            &document,
            authority,
            &(0..document.utf16_len() + 1)
        ));
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&document, authority)
            .unwrap()
        else {
            panic!("普通闭合块和代码交集必须来自真实语义证明");
        };
        assert_eq!(
            ready
                .query_for(&document, authority, 0)
                .unwrap()
                .unwrap()
                .styles,
            omni_markdown_core::MarkdownPresentationStyles::default(),
            "普通块由原 Plain proof 授权，不能再把真实证明误判成仅有gap"
        );
        let link_open = source[..source.find("[复杂").unwrap()]
            .encode_utf16()
            .count();
        let label_start = link_open + 1;
        let label_end = source[..source.find("](https://example.invalid)").unwrap()]
            .encode_utf16()
            .count();
        let link = ready
            .link_at_for(&document, authority, label_start)
            .unwrap()
            .expect("原完整长标签必须取得自己的真实直接链接");
        assert_eq!(link.label_utf16_range, label_start..label_end);
        assert_eq!(link.destination.clone().into_string(), DESTINATION);
        for index in [link_open, label_end, label_end + 2] {
            assert!(
                ready
                    .link_at_for(&document, authority, index)
                    .unwrap()
                    .is_none()
            );
        } // bracket 和 URI 本身只有原 marker 样式，不属于可点击 label；不能借 owner 包络扩权。

        // 独立完整 Parser 仅消费这份既有有限夹具，核长 label 与 marker 的实际样式；
        // 它不参与分页生产或复制计量，也不从待测 RootLeaf 的默认样式反造期望。
        let mut original = MarkdownViewportPresentation::default();
        original.rebuild(0, window.clone(), &source).unwrap();
        for index in link_open..label_end + 2 + DESTINATION.len() + 1 {
            assert_eq!(
                ready
                    .query_for(&document, authority, index)
                    .unwrap()
                    .unwrap()
                    .styles,
                original.query(index).unwrap().styles,
                "原直接链接的 label/marker 样式必须符合完整 Parser：source={index}"
            );
        }
        let mut stale = authority;
        stale.context_revision += 1;
        assert!(ready.link_at_for(&document, stale, label_start).is_err());
        let cancelled = MarkdownViewportSourceAuthority {
            cancelled: true,
            ..authority
        };
        assert!(
            ready
                .link_at_for(&document, cancelled, label_start)
                .is_err()
        );
        let changed_selection = MarkdownViewportSourceAuthority {
            selection: MarkdownTextSelection::caret(1),
            ..authority
        };
        assert!(
            ready
                .link_at_for(&document, changed_selection, label_start)
                .is_err()
        );
        assert!(
            ready
                .link_at_for(&document, authority, label_start)
                .unwrap()
                .is_some()
        );

        // 来源专用工厂确有相同无表事实，却没有任何语义Ready；两种权限不能相互转换。
        let mut request = document
            .viewport_source_request(
                window.clone(),
                authority.context_revision,
                authority.selection,
            )
            .unwrap();
        let (gap_only, work) = complete_request(&document, authority, &mut request);
        assert!(no_tables_covered(&gap_only, &document, authority, &window));
        assert!(matches!(
            gap_only
                .ordinary_semantics_for(&document, authority)
                .unwrap(),
            MarkdownOrdinarySemanticState::SourcesOnly
        ));
        assert_eq!(work.copied_source_bytes, 0);
        assert_eq!(work.parser_input_bytes, 0);
        assert_eq!(document.generation(), 0);
        assert_eq!(document.selection(), authority.selection);
        assert!(!document.can_undo() && !document.can_redo() && !document.is_dirty());
        assert!(document.original_file_identity_is_unchanged());
        assert_eq!(
            std::fs::read(directory.path().join("原来源无表证明.md")).unwrap(),
            source.as_bytes()
        );
        drop((gap_only, request, sources, document));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }

    #[test]
    fn actual_table_cannot_be_hidden_by_gap_envelope_or_empty_snapshot() {
        let source = "前言\n\n| 左 | 右 |\n| --- | --- |\n| 中文 | 😀 |\n\n尾部\n";
        let (_directory, document, sources, _) = sources(source);
        // 无表权限只能来自真实结构覆盖；结构行可在不复制正文、不运行整窗解析时发布。
        // 此例继续验证表格不能被隐藏，不把某一种复制或解析实现当作表格存在的证据。
        assert!(!no_tables_covered(
            &sources,
            &document,
            authority(&document),
            &(0..document.utf16_len())
        ));
        assert!(no_tables_covered(
            &sources,
            &document,
            authority(&document),
            &(0..1)
        ));
        let table = source[..source.find('|').unwrap()].encode_utf16().count();
        assert!(!no_tables_covered(
            &sources,
            &document,
            authority(&document),
            &(table..table + 1)
        ));
        // 来源票要实际拥有这一行；不要求先为旧整窗后端物化全部单元格语义。
        assert!(
            sources
                .table_row_at_for(&document, authority(&document), table)
                .unwrap()
                .is_some(),
            "必须取得真实结构表行，不能把未知来源当作表格覆盖"
        );
    }

    #[test]
    fn no_table_fact_rejects_cancelled_context_foreign_root_and_post_edit_undo() {
        let source = "普通中文\n\n***\n";
        let (_directory, mut document, sources, work) = sources(source);
        assert_eq!(work.copied_source_bytes, 0);
        assert_eq!(work.parser_input_bytes, 0);
        let window = 0..document.utf16_len();
        let mut cancelled = authority(&document);
        cancelled.cancelled = true;
        assert!(!no_tables_covered(&sources, &document, cancelled, &window));
        let mut stale = authority(&document);
        stale.context_revision += 1;
        assert!(!no_tables_covered(&sources, &document, stale, &window));
        let (_other_directory, other, _, _) = self::sources(source);
        assert!(!no_tables_covered(
            &sources,
            &other,
            authority(&other),
            &window
        ));
        document
            .insert_utf16(0, "改", MarkdownTextSelection::caret(1))
            .unwrap();
        document.undo().unwrap();
        assert!(!no_tables_covered(
            &sources,
            &document,
            authority(&document),
            &window
        ));
    }
}
