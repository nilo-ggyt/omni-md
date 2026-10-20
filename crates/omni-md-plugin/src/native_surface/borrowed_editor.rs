//! 会话独占 field editor/撤销历史及 Find 借用边界；不靠窗口默认 manager 推断实际历史。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2::rc::{Retained, Weak};
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{NSTextField, NSTextView, NSWindow};
use objc2_foundation::NSUndoManager;

use super::*;

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod control_abort;

/// 提供或清理栈结束时仅归还标量权限；不在 Drop 清理用户历史。
struct UndoProvidingGuard<'a>(&'a Cell<bool>);

impl Drop for UndoProvidingGuard<'_> {
    fn drop(&mut self) {
        // 外层仍在栈内的嵌套拒绝不拥有此 guard，不能替外层复位。
        self.0.set(false);
    }
}

#[derive(Default)]
/// 只有真正请求 undo 时才创建 manager；关闭后永不重新建立会话历史。
pub(super) struct NativeMarkdownSessionUndo {
    manager: RefCell<Option<Retained<NSUndoManager>>>,
    providing: Cell<bool>,
    ending_client: Cell<bool>,
    closed: Cell<bool>,
    failure: RefCell<Option<String>>,
}

impl NativeMarkdownSessionUndo {
    /// 标准modal动作只借已经存在的历史；只读菜单验证不能创建manager或扩展资源图。
    pub(super) fn cached_manager(
        &self,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Option<Retained<NSUndoManager>> {
        // main-thread proof先于缓存门禁；门禁到既有provider入口之间没有native调用或可重入drop。
        MainThreadMarker::new()?;
        if self.manager.borrow().is_none() {
            // 尚未产生历史的活动字段没有可路由对象；这不是ownership错误，不记录永久失败。
            return None;
        }
        // 同主线程立即进入原take/retain/恢复流程，cached槽仍存在，不会进入新manager构造分支。
        self.manager(ledger)
    }

    /// 只读区分已记录的持久首错与临时 Busy；提示层不能解析错误文案或触发原生历史查询。
    pub(super) fn has_failure(&self) -> bool {
        // 布尔快照不借出错误文本或 manager，调用者进入 native UI 前此借用已结束。
        self.failure.borrow().is_some()
    }

    /// 窗口 delegate 与自有 editor override 共用独占历史，不依赖默认 editor 的隐式归属。
    pub(super) fn manager(
        &self,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Option<Retained<NSUndoManager>> {
        self.provide_owner(
            &self.manager,
            || {
                // 不假定调用线程；失败时尚未产生 native candidate，不能跨线程构造 owner。
                let mtm = MainThreadMarker::new().ok_or("会话撤销必须在 AppKit 主线程执行")?;
                Ok(NSUndoManager::new(mtm))
            },
            |manager| {
                // 新 manager 仅登记一次；登记期间的 runtime 调用也在 providing 权限范围内。
                ledger.include("session.undo-manager", manager)
            },
            |manager| {
                // objc retain 可能重入；唯一项目 owner 此时在局部，不持有状态借用。
                manager.clone()
            },
            |manager| {
                // 类型保持在唯一 owner 上；这里只借用已知对象，不沿 responder 获取其他历史。
                Self::clear_owned_manager(manager)
            },
        )
    }

    /// 正常辞任前后及 admission 前检查；提供栈尚未退出时保留活动会话，不接受关闭。
    pub(super) fn preflight(&self) -> Result<(), String> {
        if let Some(error) = self.failure.borrow().as_ref() {
            // 已有归属错误优先，较晚的 Busy 或封闭状态不能覆盖首因。
            return Err(error.clone());
        }
        if self.providing.get() || self.ending_client.get() {
            // 提供或真实 client 结束栈内的 Busy 不清空历史；原操作返回后才允许关闭。
            return Err("会话撤销管理器仍在准备中".into());
        }
        if self.closed.get() {
            // 已接受关闭的 owner 不能通过另一次预检恢复活动权限。
            return Err("会话撤销记录已关闭".into());
        }
        Ok(())
    }

    /// 同一控制流也接受纯 Rust owner，用可重入 retain/drop 反例验证实际 native 路径的顺序。
    fn provide_owner<T>(
        &self,
        slot: &RefCell<Option<T>>,
        create: impl FnOnce() -> Result<T, String>,
        register: impl FnOnce(&T) -> Result<(), String>,
        retain: impl FnOnce(&T) -> T,
        clear: impl Fn(&T) -> Result<(), String>,
    ) -> Option<T> {
        if self.closed.get() || self.failure.borrow().is_some() {
            // 迟到请求或已有失败不产生新 candidate，也不再借出旧 owner。
            return None;
        }
        if self.providing.replace(true) {
            // 嵌套 getter 只拒绝本次请求；不能毒化外层提供，更不能清除仍活动的历史。
            return None;
        }
        let _providing = UndoProvidingGuard(&self.providing);
        let cached = slot.take();
        let is_new = cached.is_none();
        let owner = match cached {
            Some(owner) => {
                // take 已结束 RefCell 借用；后续 retain、清理和 drop 均只接触局部 owner。
                owner
            }
            None => match create() {
                Ok(owner) => {
                    // native 初始化可能重入关闭，登记和发布之前仍须重新检查权限。
                    owner
                }
                Err(error) => {
                    // 尚无 candidate 的失败仍保留首因，不能被下一次 delegate 请求覆盖。
                    self.record_failure(error);
                    return None;
                }
            },
        };
        if !self.can_publish_owner() {
            // 构造重入后不再登记或 retain；原有活动 owner 与未发布 candidate 分开处理。
            self.reject_owner(slot, owner, None, is_new, &clear);
            return None;
        }
        if is_new && let Err(error) = register(&owner) {
            // 账本错误不能留下未登记的历史，也不能覆盖构造期间的较早失败。
            self.record_failure(error);
        }
        if !self.can_publish_owner() {
            // 登记含 runtime weak 操作；它返回后也必须拒绝封闭或失败的 candidate。
            self.reject_owner(slot, owner, None, is_new, &clear);
            return None;
        }
        let returned = retain(&owner);
        if !self.can_publish_owner() {
            // 不返回新的 claim；未接受关闭的 cached owner 仍须保留原历史并回到槽中。
            self.reject_owner(slot, owner, Some(returned), is_new, &clear);
            return None;
        }
        // providing 阻止其他写入者；finish 只能 take，因此私有槽仍空，返回值在借用外 drop。
        let previous = slot.replace(Some(owner));
        drop(previous);
        Some(returned)
    }

    /// 每个可能重入的 native 阶段后检查权限，首因一旦存在就不能发布 owner。
    fn can_publish_owner(&self) -> bool {
        if self.closed.get() {
            // 非正常直接封闭也要留出确定失败，不能因槽暂空误称清理完整。
            self.record_failure("准备撤销管理器期间会话已关闭".into());
        }
        !self.closed.get() && self.failure.borrow().is_none()
    }

    /// 未接受关闭时保留旧历史；只有新 candidate 或已封闭会话才能进入自有 actions 清理。
    fn reject_owner<T>(
        &self,
        slot: &RefCell<Option<T>>,
        owner: T,
        returned: Option<T>,
        is_new: bool,
        clear: impl FnOnce(&T) -> Result<(), String>,
    ) {
        if !is_new && !self.closed.get() {
            // 真实失败只撤销本次返回权限；恢复原唯一 owner，不执行 accepted 的历史清空。
            let previous = slot.replace(Some(owner));
            drop(previous);
            // 多出来的返回 claim 可结束，但 cached owner 已恢复；drop 重入仍由 providing 拒绝。
            drop(returned);
            return;
        }
        if let Err(error) = clear(&owner) {
            // 新 candidate 尚无活动历史，closed owner 已获清理权限；清理错误不能覆盖首因。
            self.record_failure(error);
        }
        drop(returned);
        drop(owner);
    }

    /// Objective-C delegate 无 Result 返回通道，后续正常关闭预检必须看到最早失败。
    fn record_failure(&self, error: String) {
        self.failure.borrow_mut().get_or_insert(error);
    }

    /// 清理参数来自唯一项目槽；公开 undo/redo 事实不能由窗口或未知 responder 推断。
    fn clear_owned_manager(manager: &NSUndoManager) -> Result<(), String> {
        manager.removeAllActions();
        if manager.canUndo() || manager.canRedo() {
            // 操作仍可持有旧文本或 target，不能签发资源收据。
            return Err("会话撤销记录未能清空".into());
        }
        Ok(())
    }

    /// 只比较已恢复槽中的身份；提供期的空槽不是另一个 manager 或完成释放证明。
    pub(super) fn owns_observed_manager(&self, manager: &NSUndoManager) -> bool {
        self.manager.borrow().as_ref().is_some_and(|owned| {
            // 地址比较不触发 native retain；实际输入路径另外验证该会话身份。
            std::ptr::eq(&**owned, manager)
        })
    }

    /// 正常成功 client 结束才撤销该字段的历史；不会清理未知 manager 或重建一个来观察。
    fn finish_client_history(
        &self,
        editor: &NSTextView,
        expected: &NSUndoManager,
    ) -> Result<(), String> {
        self.preflight()?;
        if !self.owns_observed_manager(expected) {
            // 弱观察或其他窗口的历史不能授予清理权限，原 actions 完整保留。
            return Err("已结束的输入框不再持有原会话撤销管理器".into());
        }
        if expected.isUndoing() || expected.isRedoing() {
            // Undo 本身不得重入销毁仍执行中的 group；保留历史并返回明确 Busy。
            return Err("输入框正在执行撤销或重做".into());
        }
        self.finish_client_history_owner(
            &self.manager,
            || editor.breakUndoCoalescing(),
            |manager| Self::clear_owned_manager(manager),
        )
    }

    /// 纯 owner 核心也用于反例测试：coalescing 可借现有 manager，真正 clear 再取得 providing 排他权。
    fn finish_client_history_owner<T>(
        &self,
        slot: &RefCell<Option<T>>,
        finish_coalescing: impl FnOnce(),
        clear: impl FnOnce(&T) -> Result<(), String>,
    ) -> Result<(), String> {
        self.preflight()?;
        self.ending_client.set(true);
        let _ending = UndoProvidingGuard(&self.ending_client);
        // 此处仍可只读提供原 owner，正常关闭预检则已是 Busy。
        finish_coalescing();
        if !self.can_publish_owner() || self.providing.get() {
            // 可重入 native 调用后重新确认权限，不把已发生的失败当作可清的新历史。
            return Err("输入结束期间撤销记录已变更".into());
        }
        self.clear_history_owner_preflighted(slot, clear)
    }

    /// 只用于已接受的旧笔记交接；窗口仍活动，结束旧 history 后保留同一 manager。
    pub(super) fn clear_history(&self) -> Result<(), String> {
        self.clear_history_owner(&self.manager, |manager| {
            // 参数只来自唯一自有槽，不沿 responder 查询或修改未知共享历史。
            Self::clear_owned_manager(manager)
        })
    }

    /// 已接受笔记交接与最终关闭共用短借用原则，但前者不能永久封闭窗口历史。
    fn clear_history_owner<T>(
        &self,
        slot: &RefCell<Option<T>>,
        clear: impl FnOnce(&T) -> Result<(), String>,
    ) -> Result<(), String> {
        self.preflight()?;
        self.clear_history_owner_preflighted(slot, clear)
    }

    /// 两种已验证边界共享局部 owner 清理；调用方已取得 close 或 client-end 的预检权限。
    fn clear_history_owner_preflighted<T>(
        &self,
        slot: &RefCell<Option<T>>,
        clear: impl FnOnce(&T) -> Result<(), String>,
    ) -> Result<(), String> {
        // 权限核对到标量写入没有 native 调用；清理期间 getter 不能生成第二个 owner。
        self.providing.set(true);
        let _providing = UndoProvidingGuard(&self.providing);
        let owner = slot.take();
        if let Some(owner) = owner {
            if let Err(error) = clear(&owner) {
                // 已接受的清历史错误保留首因；manager 本体仍由窗口持有，不能被未知历史替代。
                self.record_failure(error);
            }
            if self.closed.get() {
                // 非法重入最终关闭已封闭会话，局部 owner 在借用外结束，不能恢复活动槽。
                self.record_failure("清理撤销记录期间会话已关闭".into());
                drop(owner);
            } else {
                // 笔记交接允许清除旧操作，随后原 manager 继续为新客户端服务。
                let previous = slot.replace(Some(owner));
                drop(previous);
            }
        }
        self.failure
            .borrow()
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }

    /// 接受关闭之后清除 undo/redo，再移出唯一项目 owner；不会通过 Drop 做可失败的业务工作。
    fn finish(&self) -> Result<(), String> {
        self.closed.set(true);
        if self.providing.get() {
            // 正常 preflight 必须拒绝此情形；防御直接重入时封闭，但不能冒认栈外 claim 已释放。
            self.record_failure("准备撤销管理器期间会话已关闭".into());
        }
        let manager = self.manager.take();
        if let Some(manager) = manager {
            // 局部强引用只保护这次公开清理，不进入关闭回调或常驻缓存。
            if let Err(error) = Self::clear_owned_manager(&manager) {
                // 清理错误不能掩盖更早的归属失败，局部 owner 在状态借用外正常 drop。
                self.record_failure(error);
            }
        }
        if let Some(error) = self.failure.borrow().as_ref() {
            // 登记失败不能被后续清空动作隐藏。
            return Err(error.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod undo_tests {
    use super::*;
    use std::rc::Weak as RustWeak;

    /// 假 native claim 只验证提供路径；retain/drop 主动检查状态槽未被借用。
    struct FakeUndoOwner(Rc<FakeUndoState>);

    struct FakeUndoState {
        slot: RustWeak<RefCell<Option<FakeUndoOwner>>>,
        live_claims: Cell<usize>,
        cleared: Cell<usize>,
        text: RefCell<String>,
        undo: RefCell<Vec<String>>,
        redo: RefCell<Vec<String>>,
    }

    impl FakeUndoOwner {
        fn new(state: &Rc<FakeUndoState>) -> Self {
            // 模拟构造/retain 可进入外部代码；真实控制流不能在这里持有 manager 槽借用。
            assert!(state.slot.upgrade().unwrap().try_borrow_mut().is_ok());
            state.live_claims.set(state.live_claims.get() + 1);
            Self(state.clone())
        }

        fn retain(&self) -> Self {
            // 不用 Rust derive Clone 掩盖 native retain 的重入边界。
            Self::new(&self.0)
        }

        fn clear(&self) -> Result<(), String> {
            // 清理也可能重入，只允许在借用外操作本次明确自有的对象。
            assert!(self.0.slot.upgrade().unwrap().try_borrow_mut().is_ok());
            self.0.cleared.set(self.0.cleared.get() + 1);
            self.0.undo.borrow_mut().clear();
            self.0.redo.borrow_mut().clear();
            Ok(())
        }

        fn undo(&self) {
            // 合成历史保存真正的旧正文；测试需要恢复数据，不能只断言一个 has-actions 标量。
            let previous = self.0.undo.borrow_mut().pop().unwrap();
            let current = self.0.text.replace(previous);
            self.0.redo.borrow_mut().push(current);
        }

        fn redo(&self) {
            // 再次恢复新正文，证明 Busy 后不是仅剩一个空的原 manager 身份。
            let next = self.0.redo.borrow_mut().pop().unwrap();
            let current = self.0.text.replace(next);
            self.0.undo.borrow_mut().push(current);
        }
    }

    impl Drop for FakeUndoOwner {
        fn drop(&mut self) {
            // 若复发 borrow 内 clone/drop，反例在实际 helper 路径中直接失败。
            assert!(self.0.slot.upgrade().unwrap().try_borrow_mut().is_ok());
            self.0.live_claims.set(self.0.live_claims.get() - 1);
        }
    }

    fn fake_undo_slot() -> (Rc<RefCell<Option<FakeUndoOwner>>>, Rc<FakeUndoState>) {
        let slot = Rc::new(RefCell::new(None));
        let state = Rc::new(FakeUndoState {
            slot: Rc::downgrade(&slot),
            live_claims: Cell::new(0),
            cleared: Cell::new(0),
            text: RefCell::new("draft after input".into()),
            undo: RefCell::new(vec!["draft before input".into()]),
            redo: RefCell::new(Vec::new()),
        });
        (slot, state)
    }

    #[test]
    fn undo_provider_busy_preflight_preserves_active_owner_and_native_claims() {
        let undo = NativeMarkdownSessionUndo::default();
        let (slot, state) = fake_undo_slot();
        let created = Cell::new(0);
        let registered = Cell::new(0);
        let returned = undo
            .provide_owner(
                &slot,
                || {
                    created.set(created.get() + 1);
                    Ok(FakeUndoOwner::new(&state))
                },
                |_| {
                    // 账本登记也处于同一提供权限内，不能在 owner 暂空时接受关闭。
                    registered.set(registered.get() + 1);
                    assert!(undo.preflight().is_err());
                    Ok(())
                },
                |owner| {
                    // Busy 只拒绝本次 close；不冻结、不清空，也不留下持续归属错误。
                    assert!(undo.preflight().is_err());
                    assert!(!undo.closed.get());
                    assert!(undo.failure.borrow().is_none());
                    assert!(slot.borrow().is_none());
                    owner.retain()
                },
                FakeUndoOwner::clear,
            )
            .unwrap();
        assert!(undo.preflight().is_ok());
        assert_eq!(created.get(), 1);
        assert_eq!(registered.get(), 1);
        assert_eq!(state.live_claims.get(), 2);
        assert_eq!(*state.undo.borrow(), ["draft before input"]);
        let second = undo
            .provide_owner(
                &slot,
                || {
                    // 已有 owner 必须复用，不能在第二次查询时重建活动历史。
                    panic!("cached manager must not be recreated")
                },
                |_| {
                    // 相同活 owner 已经登记，不得制造第二个身份或重复构造路径。
                    panic!("cached manager must not be registered again")
                },
                FakeUndoOwner::retain,
                FakeUndoOwner::clear,
            )
            .unwrap();
        assert!(Rc::ptr_eq(&returned.0, &second.0));
        assert_eq!(state.live_claims.get(), 3);
        drop(returned);
        drop(second);
        let owner = slot.take();
        drop(owner);
        assert_eq!(state.live_claims.get(), 0);
        assert_eq!(state.cleared.get(), 0);
    }

    #[test]
    fn undo_nested_native_getter_keeps_same_cached_owner_and_working_history() {
        let undo = NativeMarkdownSessionUndo::default();
        let (slot, state) = fake_undo_slot();
        slot.replace(Some(FakeUndoOwner::new(&state)));
        let nested_creations = Cell::new(0);
        let returned = undo.provide_owner(
            &slot,
            || {
                // 此反例从已有 manager 的 retain 进入，首次构造不应发生。
                panic!("cached manager must not be recreated")
            },
            |_| {
                // 旧 manager 的身份已登记，反例只观察 native retain 重入。
                panic!("cached manager must not be registered again")
            },
            |owner| {
                assert!(slot.borrow().is_none());
                let nested = undo.provide_owner(
                    &slot,
                    || {
                        // 若 guard 缺失，空槽会错误创建第二个 owner，本计数会揭示回归。
                        nested_creations.set(nested_creations.get() + 1);
                        Ok(FakeUndoOwner::new(&state))
                    },
                    |_| Ok(()),
                    FakeUndoOwner::retain,
                    FakeUndoOwner::clear,
                );
                assert!(nested.is_none());
                owner.retain()
            },
            FakeUndoOwner::clear,
        );
        let returned = returned.unwrap();
        let owner = slot.take().unwrap();
        assert!(Rc::ptr_eq(&returned.0, &owner.0));
        assert!(!undo.providing.get());
        assert_eq!(nested_creations.get(), 0);
        assert_eq!(state.live_claims.get(), 2);
        assert_eq!(state.cleared.get(), 0);
        assert!(undo.failure.borrow().is_none());
        assert!(undo.preflight().is_ok());
        assert_eq!(*state.text.borrow(), "draft after input");
        owner.undo();
        assert_eq!(*state.text.borrow(), "draft before input");
        owner.redo();
        assert_eq!(*state.text.borrow(), "draft after input");
        drop(returned);
        drop(owner);
        assert_eq!(state.live_claims.get(), 0);
    }

    #[test]
    fn undo_close_during_each_native_stage_discards_without_publication() {
        for (cached, close_at) in [
            (false, "create"),
            (false, "register"),
            (false, "retain"),
            (true, "retain"),
        ] {
            // 新 candidate 的三个阶段和活动 cached retain 均须后检；只有 closed 可清旧历史。
            let undo = NativeMarkdownSessionUndo::default();
            let (slot, state) = fake_undo_slot();
            if cached {
                // cached 模式从已有活动历史进入，不能将关闭清理误当成首次分配失败。
                slot.replace(Some(FakeUndoOwner::new(&state)));
            }
            let returned = undo.provide_owner(
                &slot,
                || {
                    let owner = FakeUndoOwner::new(&state);
                    if close_at == "create" {
                        // 绕过正常 preflight 的直接关闭必须失败并封闭，不能冒认栈外 owner 已释放。
                        assert!(undo.finish().is_err());
                    }
                    Ok(owner)
                },
                |_| {
                    if close_at == "register" {
                        // 登记返回前关闭也不得把 candidate 放回槽或返回给 AppKit。
                        assert!(undo.finish().is_err());
                    }
                    Ok(())
                },
                |owner| {
                    let returned = owner.retain();
                    if close_at == "retain" {
                        // 两份 claim 已存在时仍必须全部回收，不只丢弃槽的唯一 owner。
                        assert!(undo.finish().is_err());
                    }
                    returned
                },
                FakeUndoOwner::clear,
            );
            assert!(returned.is_none(), "stage={close_at}");
            assert!(slot.borrow().is_none());
            assert!(!undo.providing.get());
            assert!(undo.closed.get());
            assert_eq!(state.live_claims.get(), 0);
            assert_eq!(state.cleared.get(), 1);
            assert!(state.undo.borrow().is_empty());
            assert!(state.redo.borrow().is_empty());
            assert_eq!(
                undo.preflight().unwrap_err(),
                "准备撤销管理器期间会话已关闭"
            );
        }
    }

    #[test]
    fn undo_registration_failure_cleans_candidate_and_keeps_first_reason() {
        let undo = NativeMarkdownSessionUndo::default();
        let (slot, state) = fake_undo_slot();
        let returned = undo.provide_owner(
            &slot,
            || Ok(FakeUndoOwner::new(&state)),
            |_| Err("ledger failed first".into()),
            |_| {
                // 登记失败后不应再创建返回 claim。
                panic!("failed manager must not be retained for publication")
            },
            |owner| {
                owner.clear()?;
                Err("clear failed later".into())
            },
        );
        assert!(returned.is_none());
        assert!(slot.borrow().is_none());
        assert_eq!(state.live_claims.get(), 0);
        assert_eq!(state.cleared.get(), 1);
        assert_eq!(undo.preflight().unwrap_err(), "ledger failed first");
    }

    #[test]
    fn undo_real_failure_before_admission_restores_cached_owner_without_clearing_history() {
        let undo = NativeMarkdownSessionUndo::default();
        let (slot, state) = fake_undo_slot();
        slot.replace(Some(FakeUndoOwner::new(&state)));
        let returned = undo.provide_owner(
            &slot,
            || {
                // 已有 owner 的真实归属错误不能触发重新分配。
                panic!("cached manager must not be recreated")
            },
            |_| {
                // 已登记的身份不能在拒绝返回 claim 时被重新注册。
                panic!("cached manager must not be registered again")
            },
            |owner| {
                let returned = owner.retain();
                // 不把真实 ownership 错误冒充 Busy；后续 close 保留首因，但仍未接受清理。
                undo.record_failure("real ownership failure first".into());
                undo.record_failure("later observation failure".into());
                returned
            },
            |_| {
                // 未 closed 的 cached owner 不获 accepted 清理权限，执行到这里就是数据丢失。
                panic!("active cached history must not be cleared")
            },
        );
        assert!(returned.is_none());
        assert!(!undo.closed.get());
        assert!(!undo.providing.get());
        assert_eq!(state.live_claims.get(), 1);
        assert_eq!(state.cleared.get(), 0);
        assert_eq!(
            undo.preflight().unwrap_err(),
            "real ownership failure first"
        );
        let owner = slot.take().unwrap();
        assert!(Rc::ptr_eq(&owner.0, &state));
        owner.undo();
        assert_eq!(*state.text.borrow(), "draft before input");
        owner.redo();
        assert_eq!(*state.text.borrow(), "draft after input");
        drop(owner);
        assert_eq!(state.live_claims.get(), 0);
    }

    #[test]
    fn accepted_note_history_clear_keeps_window_owner_and_blocks_reentrant_creation() {
        let undo = NativeMarkdownSessionUndo::default();
        let (slot, state) = fake_undo_slot();
        slot.replace(Some(FakeUndoOwner::new(&state)));
        let created = Cell::new(0);
        undo.clear_history_owner(&slot, |owner| {
            // 旧笔记交接已接受，但窗口尚未关闭；原生清理重入不得取得临时空槽。
            assert!(undo.preflight().is_err());
            let nested = undo.provide_owner(
                &slot,
                || {
                    created.set(created.get() + 1);
                    Ok(FakeUndoOwner::new(&state))
                },
                |_| Ok(()),
                FakeUndoOwner::retain,
                FakeUndoOwner::clear,
            );
            assert!(nested.is_none());
            owner.clear()
        })
        .unwrap();
        assert_eq!(created.get(), 0);
        assert_eq!(state.live_claims.get(), 1);
        assert_eq!(state.cleared.get(), 1);
        assert_eq!(*state.text.borrow(), "draft after input");
        assert!(state.undo.borrow().is_empty());
        assert!(state.redo.borrow().is_empty());
        assert!(undo.preflight().is_ok());
        assert!(!undo.closed.get());
        let owner = slot.take().unwrap();
        assert!(Rc::ptr_eq(&owner.0, &state));
        drop(owner);
        assert_eq!(state.live_claims.get(), 0);
    }

    #[test]
    fn note_history_clear_during_provision_is_busy_and_preserves_active_payload() {
        let undo = NativeMarkdownSessionUndo::default();
        let (slot, state) = fake_undo_slot();
        slot.replace(Some(FakeUndoOwner::new(&state)));
        let returned = undo
            .provide_owner(
                &slot,
                || panic!("cached manager must not be recreated"),
                |_| panic!("cached manager must not be registered again"),
                |owner| {
                    let result = undo.clear_history_owner(&slot, |_| {
                        // 预检尚未接受交接，绝不能因 owner 恰好借出而清理或伪称无历史。
                        panic!("busy preflight must not clear active history")
                    });
                    assert_eq!(result.unwrap_err(), "会话撤销管理器仍在准备中");
                    owner.retain()
                },
                FakeUndoOwner::clear,
            )
            .unwrap();
        assert!(undo.preflight().is_ok());
        assert_eq!(state.cleared.get(), 0);
        let owner = slot.take().unwrap();
        owner.undo();
        assert_eq!(*state.text.borrow(), "draft before input");
        owner.redo();
        assert_eq!(*state.text.borrow(), "draft after input");
        drop(returned);
        drop(owner);
        assert_eq!(state.live_claims.get(), 0);
    }

    #[test]
    fn final_close_during_accepted_history_clear_cannot_restore_window_owner() {
        let undo = NativeMarkdownSessionUndo::default();
        let (slot, state) = fake_undo_slot();
        slot.replace(Some(FakeUndoOwner::new(&state)));
        let result = undo.clear_history_owner(&slot, |owner| {
            owner.clear()?;
            // 故意绕过正常 Busy 预检模拟非法最终关闭，后检必须阻止恢复 manager。
            assert!(undo.finish().is_err());
            Ok(())
        });
        assert_eq!(result.unwrap_err(), "准备撤销管理器期间会话已关闭");
        assert!(slot.borrow().is_none());
        assert!(undo.closed.get());
        assert!(!undo.providing.get());
        assert_eq!(state.cleared.get(), 1);
        assert_eq!(state.live_claims.get(), 0);
    }

    #[test]
    fn successful_client_end_breaks_coalescing_before_clearing_only_owned_history() {
        let undo = NativeMarkdownSessionUndo::default();
        let (slot, state) = fake_undo_slot();
        slot.replace(Some(FakeUndoOwner::new(&state)));
        state.redo.borrow_mut().push("redo before end".into());
        let coalesced = Cell::new(false);
        undo.finish_client_history_owner(
            &slot,
            || {
                // 原 owner 在 coalescing 时仍可被 getter 借到，正常 close 则由 ending 标量拒绝。
                assert!(undo.preflight().is_err());
                let claim = undo
                    .provide_owner(
                        &slot,
                        || panic!("must reuse owner"),
                        |_| panic!("must reuse registration"),
                        FakeUndoOwner::retain,
                        FakeUndoOwner::clear,
                    )
                    .unwrap();
                assert_eq!(state.cleared.get(), 0);
                assert_eq!(*state.undo.borrow(), ["draft before input"]);
                drop(claim);
                coalesced.set(true);
            },
            |owner| {
                // clear 取得局部唯一 owner；不持槽 RefMut，避免 native 析构/回调借用冲突。
                assert!(coalesced.get());
                owner.clear()
            },
        )
        .unwrap();
        assert_eq!(state.cleared.get(), 1);
        assert!(state.undo.borrow().is_empty() && state.redo.borrow().is_empty());
        assert!(slot.borrow().is_some());
        assert!(undo.preflight().is_ok());
        // 假 owner 的析构会检查槽借用；先明确释放 owner，再允许槽本身结束生命周期。
        drop(slot.take());
        assert_eq!(state.live_claims.get(), 0);
    }

    #[test]
    fn client_end_busy_and_coalescing_unwind_preserve_the_original_history() {
        let undo = NativeMarkdownSessionUndo::default();
        let (slot, state) = fake_undo_slot();
        slot.replace(Some(FakeUndoOwner::new(&state)));
        undo.providing.set(true);
        assert!(
            undo.finish_client_history_owner(
                &slot,
                || panic!("busy must not touch coalescing"),
                FakeUndoOwner::clear
            )
            .is_err()
        );
        undo.providing.set(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            undo.finish_client_history_owner(
                &slot,
                || panic!("synthetic coalescing unwind"),
                FakeUndoOwner::clear,
            )
        }));
        assert!(result.is_err());
        assert_eq!(state.cleared.get(), 0);
        assert_eq!(*state.undo.borrow(), ["draft before input"]);
        assert!(slot.borrow().is_some());
        assert!(!undo.ending_client.get());
        assert!(undo.failure.borrow().is_none());
        // 保留历史的反例也必须在槽仍存活时释放 owner，避免夹具析构制造无关失败。
        drop(slot.take());
        assert_eq!(state.live_claims.get(), 0);
    }

    #[test]
    fn client_boundary_same_change_failed_end_and_late_end_preserve_current_data() {
        let boundary = ClientHistoryBoundary::<usize>::default();
        let history = RefCell::new(vec!["old undo", "old redo"]);
        boundary.observe(|client| *client == 1, || Ok(1)).unwrap();
        boundary
            .observe(
                |client| *client == 1,
                || panic!("same client must not replace owner"),
            )
            .unwrap();
        assert_eq!(history.borrow().len(), 2);
        assert!(
            boundary
                .finish(
                    |client| *client == 1,
                    |_| Err("owned manager validation rejected".into())
                )
                .is_err()
        );
        assert_eq!(history.borrow().len(), 2);
        assert_eq!(**boundary.active.borrow().as_ref().unwrap(), 1);
        boundary
            .finish(
                |client| *client == 1,
                |_| {
                    history.borrow_mut().clear();
                    Ok(())
                },
            )
            .unwrap();
        boundary.observe(|client| *client == 2, || Ok(2)).unwrap();
        history.borrow_mut().push("new active undo");
        // 旧成功 end 的重复/晚到副本不能清新 client，即使实际 native editor 会被复用。
        boundary
            .finish(
                |client| *client == 1,
                |_| panic!("late old end must not execute clear"),
            )
            .unwrap();
        assert_eq!(*history.borrow(), ["new active undo"]);
        assert_eq!(**boundary.active.borrow().as_ref().unwrap(), 2);
    }

    #[test]
    fn client_boundary_reentry_cannot_publish_or_clear_a_second_client() {
        let boundary = ClientHistoryBoundary::<usize>::default();
        boundary.observe(|client| *client == 1, || Ok(1)).unwrap();
        let cleared = Cell::new(0);
        boundary
            .finish(
                |client| *client == 1,
                |_| {
                    assert!(
                        boundary
                            .observe(
                                |client| *client == 2,
                                || panic!("busy must not create client")
                            )
                            .is_err()
                    );
                    assert!(
                        boundary
                            .finish(
                                |client| *client == 1,
                                |_| panic!("nested end must not clear twice")
                            )
                            .is_err()
                    );
                    cleared.set(cleared.get() + 1);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(cleared.get(), 1);
        assert!(boundary.active.borrow().is_none());
        assert!(!boundary.ending.get());
    }
}

/// 一次 provider 调用的栈权限；嵌套构造拒绝，原生调用返回后重新确认可发布。
struct FieldEditorCreationLease<'a> {
    creating: &'a Cell<bool>,
    closed: &'a Cell<bool>,
}

impl<'a> FieldEditorCreationLease<'a> {
    fn enter(creating: &'a Cell<bool>, closed: &'a Cell<bool>) -> Result<Self, &'static str> {
        if creating.replace(true) {
            // 嵌套调用不拥有外层标量，不能在错误返回时替它复位。
            return Err("笔记输入控件的创建发生重入");
        }
        Ok(Self { creating, closed })
    }

    /// 检查的是原生初始化/retain 后的当前权限；不是进入函数时的旧 closed 快照。
    fn complete<T>(&self, candidate: T) -> Result<T, (T, &'static str)> {
        if self.closed.get() {
            // 把尚未发布的 owner 还给调用方清空/drop，不能写回已 freeze 的 provider。
            Err((candidate, "笔记输入控件在创建期间已关闭"))
        } else {
            // complete 与纯所有权 move 发布之间不得再调用原生代码。
            Ok(candidate)
        }
    }
}

impl Drop for FieldEditorCreationLease<'_> {
    fn drop(&mut self) {
        // 正常返回、构造错误和 Rust unwind 都只归还本栈的构造权限。
        self.creating.set(false);
    }
}

#[derive(Clone, Copy)]
/// 只保留公开可检查的标量事实，不把不可见或经过时间当成缓存已安全。
struct BorrowedEditorFacts {
    text_empty: bool,
    unmarked: bool,
    delegate_detached: bool,
    hierarchy_detached: bool,
    selection_empty: bool,
}

impl BorrowedEditorFacts {
    /// 每项都与数据或回调权限有关，不能按框架类名放宽其中任何一项。
    fn verify(self) -> bool {
        self.text_empty
            && self.unmarked
            && self.delegate_detached
            && self.hierarchy_detached
            && self.selection_empty
    }
}

/// 客户身份与结束事务分开；泛型主体只操作 Rc/标量，native 观察由借用外的闭包完成。
struct ClientHistoryBoundary<T> {
    active: RefCell<Option<Rc<T>>>,
    ending: Cell<bool>,
}

impl<T> Default for ClientHistoryBoundary<T> {
    fn default() -> Self {
        Self {
            active: RefCell::new(None),
            ending: Cell::new(false),
        }
    }
}

impl<T> ClientHistoryBoundary<T> {
    fn observe(
        &self,
        same: impl Fn(&T) -> bool,
        create: impl FnOnce() -> Result<T, String>,
    ) -> Result<(), String> {
        if self.ending.get() {
            // 结束栈中的重入没有创建/替换当前客户的权限，原历史不被清空。
            return Err("输入框撤销记录仍在处理中".into());
        }
        let current = self.active.borrow().clone();
        if let Some(current) = current {
            // 相同客户 change 不改 Undo/Redo；没有成功 end 的跨客户 begin 明确拒绝。
            return if same(&current) {
                Ok(())
            } else {
                Err("上一输入框的撤销记录尚未结束".into())
            };
        }
        let candidate = Rc::new(create()?);
        let current = self.active.borrow().clone();
        if let Some(current) = current {
            // 原生 getter/weak 初始化若重入完成观察，不覆盖它刚发布的身份。
            return if same(&current) {
                Ok(())
            } else {
                Err("观察输入期间撤销记录的归属已变更".into())
            };
        }
        *self.active.borrow_mut() = Some(candidate);
        Ok(())
    }

    fn finish(
        &self,
        same: impl FnOnce(&T) -> bool,
        operation: impl FnOnce(&T) -> Result<(), String>,
    ) -> Result<(), String> {
        let current = self.active.borrow().clone();
        let Some(current) = current else {
            return Ok(()); // 没有开始的/重复 end 不清任何 manager。
        };
        if self.ending.replace(true) {
            return Err("输入框撤销记录仍在处理中".into()); // 嵌套拒绝不拥有下面的复位 guard。
        }
        let _ending = UndoProvidingGuard(&self.ending);
        if !same(&current) {
            return Ok(()); // 迟到旧 end 不得清当前客户，即使两客户复用同一个 editor。
        }
        operation(&current)?;
        // operation 成功前不移走 owner；失败验证/Busy/未知 manager 保留原身份和历史。
        let previous = self.active.borrow_mut().take();
        drop(previous);
        Ok(())
    }
}

/// 活动编辑身份只持 weak；Rc 允许在状态借用外查询 native 身份，避免 Weak 操作跨 RefCell 借用。
struct FieldHistoryClient {
    field: Weak<NSTextField>,
    editor: Weak<NSTextView>,
    manager: Weak<NSUndoManager>,
}

#[derive(Default)]
/// 同一会话的预检与具名 probe 共用完成事实，字段不拥有任何原生对象。
struct BorrowedEditorState {
    editors: Vec<Weak<NSTextView>>,
    observed_undo: Vec<Weak<NSUndoManager>>,
    finished: bool,
    controls_detached: bool,
    failure: Option<String>,
}

/// accepted 清理只执行一次；先释放明确自有 editor，再判定最终公开事实，首错始终保留。
fn complete_accepted_field_cleanup<R>(
    freeze: impl FnOnce(),
    capture: impl FnOnce() -> Result<(), String>,
    end_editing: impl FnOnce(),
    clear_history: impl FnOnce() -> Result<(), String>,
    release_owned: impl FnOnce() -> R,
    verify: impl FnOnce() -> Result<(), String>,
) -> (R, Result<(), String>) {
    freeze();
    let mut failure = capture().err();
    end_editing();
    if let Err(error) = clear_history() {
        // 历史错误不跳过已知自有载荷清理，也不覆盖较早 capture 的原因。
        failure.get_or_insert(error);
    }
    let receipt = release_owned();
    if let Err(error) = verify() {
        // 所有清理完成后仍须严格验证；资源 drop 不能把失败边界重标成功。
        failure.get_or_insert(error);
    }
    (receipt, failure.map_or(Ok(()), Err))
}

#[derive(Clone, Default)]
/// 最多记录前后四个 editor weak；共享元数据不安装标记或拥有框架缓存。
pub(super) struct NativeMarkdownBorrowedEditorSnapshot(
    Rc<RefCell<BorrowedEditorState>>,
    Rc<ClientHistoryBoundary<FieldHistoryClient>>,
);

impl NativeMarkdownBorrowedEditorSnapshot {
    /// 只在真实编辑 begin/change 回调查询实际 manager，不在关闭时为观察而创建历史。
    pub(super) fn observe_editing(
        &self,
        control: &NSTextField,
        undo: &NativeMarkdownSessionUndo,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Result<(), String> {
        if self.1.ending.get() {
            // 结束操作的同步重入不发布新客户，也不将暂时 Busy 记录为持久失败。
            return Err("输入框撤销记录仍在处理中".into());
        }
        let result = self.observe_active_editor(control, undo, ledger);
        if let Err(error) = &result {
            // 身份不匹配保存在共享状态，后续预检失败时保留原始可编辑输入。
            self.0.borrow_mut().failure = Some(error.clone());
        }
        result
    }

    fn observe_active_editor(
        &self,
        control: &NSTextField,
        undo: &NativeMarkdownSessionUndo,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Result<(), String> {
        let Some(editor) = control.currentEditor() else {
            // 非编辑中的原生值更新没有可记录的实际 editor，不创建一个用于测试。
            return Ok(());
        };
        let editor = editor
            .downcast::<NSTextView>()
            .map_err(|_| "当前笔记输入控件不支持原生文本编辑".to_owned())?;
        self.observe_editor(&editor, undo, ledger)?;
        let result = self.1.observe(
            |client| {
                client
                    .field
                    .load()
                    .is_some_and(|field| std::ptr::eq(&*field, control))
            },
            || {
                let actual = editor.undoManager().ok_or("当前输入框没有会话撤销记录")?;
                if !undo.owns_observed_manager(&actual) {
                    // 观察不授予未知 manager 清理权；仍保留输入并让后续预检拒绝。
                    return Err("当前输入框的会话撤销记录已变更".into());
                }
                Ok(FieldHistoryClient {
                    field: Weak::from(control),
                    editor: Weak::from_retained(&editor),
                    manager: Weak::from_retained(&actual),
                })
            },
        );
        if let Err(error) = &result
            && !self.1.ending.get()
        {
            // 非 Busy 的跨客户混合历史不再授权 Undo getter，避免继续执行错误的旧 target 范围。
            undo.record_failure(error.clone());
        }
        result
    }

    /// 仅由公开 controlTextDidEndEditing 成功通知调用；迟到旧字段不能清当前字段的历史。
    pub(super) fn finished_control_editing(
        &self,
        control: &NSTextField,
        undo: &NativeMarkdownSessionUndo,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Result<(), String> {
        self.finish_control_history(
            |client| {
                client
                    .field
                    .load()
                    .is_some_and(|field| std::ptr::eq(&*field, control))
            },
            undo,
            ledger,
        )
    }

    /// 正常didEnd与已证明的原abort票共用同一归属检查和唯一历史结束事务。
    fn finish_control_history(
        &self,
        same: impl FnOnce(&FieldHistoryClient) -> bool,
        undo: &NativeMarkdownSessionUndo,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Result<(), String> {
        if undo.closed.get() {
            // accepted close 的统一 finish 已取得唯一清理权，不重复清理或恢复旧状态。
            return Ok(());
        }
        if undo.providing.get() || undo.ending_client.get() {
            // 暂时 Busy 不毒化快照，也不拿走原字段及其完整历史。
            return Err("输入框撤销记录仍在处理中".into());
        }
        let result = self.1.finish(same, |client| {
            undo.preflight()?;
            let editor = client
                .editor
                .load()
                .ok_or("输入控件在撤销记录清理前已消失")?;
            if !editor
                .downcast_ref::<NativeMarkdownSessionFieldEditor>()
                .is_some_and(|editor| editor.belongs_to(undo, ledger))
            {
                // 原 editor 的 weak 身份和会话来源都须仍可证明，不能沿窗口猜一个替代对象。
                return Err("已结束的输入控件不属于原会话".into());
            }
            let expected = client
                .manager
                .load()
                .ok_or("已结束输入框的撤销管理器已消失")?;
            if !undo.owns_observed_manager(&expected) {
                // 非创建式比较后才允许实际 getter；当前 cached manager 必须仍是原输入所用的对象。
                return Err("已结束输入框的撤销管理器已变更".into());
            }
            let actual = editor
                .undoManager()
                .ok_or("已结束输入框没有实际撤销管理器")?;
            if !std::ptr::eq(&*actual, &*expected) {
                // 未知 actual history 完整保留，不执行 removeAllActions 或降级成无 Undo 的正常成功。
                return Err("已结束输入框的撤销管理器归属异常".into());
            }
            undo.finish_client_history(&editor, &expected)
        });
        if let Err(error) = &result
            && !self.1.ending.get()
        {
            // 失败保留身份/历史并进入资源预检；晚到旧字段被正常忽略，不会污染新历史。
            self.0.borrow_mut().failure.get_or_insert(error.clone());
        }
        result
    }

    /// 真实窗口事件结束后的 Find editor 也须记录失败，不能冒充为已知共享历史。
    pub(super) fn observe_native_editor(
        &self,
        editor: &NSTextView,
        undo: &NativeMarkdownSessionUndo,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Result<(), String> {
        let result = self.observe_editor(editor, undo, ledger);
        if let Err(error) = &result {
            // 实际编辑归属错误使后续可失败预检拒绝，不在此清理未知数据。
            self.0.borrow_mut().failure = Some(error.clone());
        }
        result
    }

    /// 真实编辑事件/回调中查询该实际 editor 的 manager，不在关闭观测时创建历史。
    pub(super) fn observe_editor(
        &self,
        editor: &NSTextView,
        undo: &NativeMarkdownSessionUndo,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Result<(), String> {
        if !editor
            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
            .is_some_and(|editor| editor.belongs_to(undo, ledger))
        {
            // 窗口 undoManager 不能代表实际 field editor 的历史；未知 provider 一律拒绝，不清其历史。
            return Err("当前笔记输入控件不属于原会话".into());
        }
        self.capture_editor(editor)?;
        let expected = undo
            .manager(ledger)
            .ok_or("编辑期间笔记会话撤销管理器不可用")?;
        // 这是实际用户编辑的 undo 初始化路径，可能按公开 responder 链请求窗口 delegate。
        let actual = editor.undoManager();
        let Some(actual) = actual else {
            // 本会话 provider 承诺正常 Undo；nil 不是可接受的静默功能降级。
            return Err("当前笔记输入框没有撤销管理器".into());
        };
        if Retained::as_ptr(&expected) != Retained::as_ptr(&actual) {
            // 未知或共享历史不能被整个清空，也不能被冒充为已拥有的会话历史。
            return Err("当前笔记输入框的撤销管理器归属异常".into());
        }
        let known = self.0.borrow().observed_undo.clone();
        if !known.iter().any(|manager| {
            // 去重只比较实际仍存活的 manager 身份，不使用过期地址作为权威。
            manager
                .load()
                .is_some_and(|manager| std::ptr::eq(&*manager, &*actual))
        }) {
            if known.len() == 4 {
                // 真实编辑期间不允许无限切换历史所有者；保留失败而不丢弃旧证据。
                return Err("笔记撤销管理器超过会话数量上限".into());
            }
            self.0
                .borrow_mut()
                .observed_undo
                .push(Weak::from_retained(&actual));
        }
        Ok(())
    }

    /// 记录一个已实际编辑的共享对象；身份去重以活 weak 为准。
    fn capture_editor(&self, editor: &NSTextView) -> Result<(), String> {
        let known = self.0.borrow().editors.clone();
        if known.iter().any(|known| {
            known
                .load()
                .is_some_and(|value| std::ptr::eq(&*value, editor))
        }) {
            // 相同活身份只有一个观察角色，已 nil 的旧对象不能遮蔽地址复用。
            return Ok(());
        }
        if known.len() == 4 {
            // 明确拒绝超出有限预算的轮换，不丢失旧业务编辑器的证明。
            return Err("笔记共享输入控件超过会话数量上限".into());
        }
        self.0.borrow_mut().editors.push(Weak::from(editor));
        Ok(())
    }

    /// 非创建式记录窗口已存在的默认 editor，兼顾辞任期间替换的实例。
    pub(super) fn capture(&self, panel: &NSWindow) -> Result<(), String> {
        if let Some(error) = self.failure() {
            // 实际编辑期间已发现的归属错误必须在清空输入前拒绝关闭。
            return Err(error);
        }
        // SAFETY: false 明确禁止创建，nil client 只查询主线程活窗口现有的默认 editor。
        let editor = unsafe { panel.fieldEditor_forObject(false, None) };
        if let Some(editor) = editor {
            let editor = editor
                .downcast::<NSTextView>()
                .map_err(|_| "默认笔记输入控件不支持原生文本编辑".to_owned())?;
            let known_editors = self.0.borrow().editors.clone();
            let already_known = known_editors.iter().any(|known| {
                // 只把仍存活且身份完全相同的 weak 当作重复；旧 nil 不掩盖地址复用。
                known
                    .load()
                    .is_some_and(|known| std::ptr::eq(&*known, &*editor))
            });
            if !already_known {
                if self.0.borrow().editors.len() == 4 {
                    // 单次关闭不允许无限替换 editor；超限保留失败而不丢弃已有证据。
                    return Err("笔记共享输入控件超过会话数量上限".into());
                }
                self.0
                    .borrow_mut()
                    .editors
                    .push(Weak::from_retained(&editor));
            }
        }
        Ok(())
    }

    /// 正常辞任必须已结束真实 composition；失败发生在 accepted release 之前。
    pub(super) fn preflight_unmarked(&self) -> Result<(), String> {
        if self.1.ending.get() {
            // 当前成功辞任正在核对/清理自有历史；不能重入接受释放同一 editor。
            return Err("输入框撤销记录仍在处理中".into());
        }
        let editors = self.0.borrow().editors.clone();
        for editor in editors {
            if let Some(editor) = editor.load() {
                // SAFETY: 已验证的普通共享 NSTextView 在主线程实现公开输入状态 getter。
                let marked: bool = unsafe { msg_send![&*editor, hasMarkedText] };
                if marked {
                    // 不清空原始输入状态，调用方恢复可编辑会话并反馈失败。
                    return Err("笔记共享输入框的输入尚未完成".into());
                }
            }
        }
        Ok(())
    }

    /// 仅在验证与 close admission 成功后调用；失败作为不可重试的资源证明保留。
    pub(super) fn finish_accepted(
        &self,
        panel: &NSWindow,
        controls: &[&NSTextField],
        undo: &NativeMarkdownSessionUndo,
        provider: &NativeMarkdownFieldEditorProvider,
        close_window: bool,
    ) -> Option<Weak<NativeMarkdownSessionFieldEditor>> {
        let (editor, result) = self.finish_in_place(panel, controls, undo, provider, close_window);
        self.0.borrow_mut().finished = true;
        if let Err(error) = result {
            // 此时不能回滚成“未接受关闭”或重新尝试；后续 barrier 必须维持 Closing。
            self.0.borrow_mut().failure.get_or_insert(error);
        }
        editor
    }

    /// 公开 endEditing 清除共享编辑器关系；调用方已正常辞任并接受了唯一关闭请求。
    fn finish_in_place(
        &self,
        panel: &NSWindow,
        controls: &[&NSTextField],
        undo: &NativeMarkdownSessionUndo,
        provider: &NativeMarkdownFieldEditorProvider,
        close_window: bool,
    ) -> (
        Option<Weak<NativeMarkdownSessionFieldEditor>>,
        Result<(), String>,
    ) {
        complete_accepted_field_cleanup(
            || {
                // 所有可失败 capture 或 Native 清理之前先封闭新建权限。
                provider.freeze();
                if close_window {
                    // 完整窗口不再为任何客户端提供新的撤销历史。
                    undo.closed.set(true);
                }
            },
            || self.capture(panel),
            || {
                // SAFETY: nil 是公开允许参数，输入验证和正常辞任都已在 accepted 前成功。
                unsafe { panel.endEditingFor(None) };
            },
            || {
                if close_window {
                    // 最终关闭移出 manager 的项目强 owner。
                    undo.finish()
                } else {
                    // 笔记交接只清旧历史，仍活动的窗口可为新客户端按需使用 manager。
                    undo.clear_history()
                }
            },
            || {
                for control in controls {
                    // 标准 endEditing 不承诺清空自有 editor 的 backing string；先清客户端数据。
                    control.setStringValue(ns_string!(""));
                }
                // 再 seal/清空自有 editor 并移出 owner，之后才允许作最终 detached 判定。
                provider.finish_accepted()
            },
            || {
                let captured = self.capture(panel);
                let controls_detached = controls.iter().all(|control| {
                    // currentEditor 非创建式；任一旧编辑关系仍存在就使最终事实失败。
                    control.currentEditor().is_none()
                });
                self.0.borrow_mut().controls_detached = controls_detached;
                if !controls_detached
                    || !self.editors_are_detached()
                    || !self.observed_undo_is_empty()
                {
                    // 仍检查同一严格事实；较早的 capture 错误优先返回，不省略任何成功条件。
                    return captured.and(Err("笔记共享输入控件尚未清空或解除关联".into()));
                }
                captured
            },
        )
    }

    /// 真实项目 cohort 完成后仅检查一次缓存边界，不把框架对象存活误报为项目泄漏。
    pub(super) fn is_detached(&self) -> bool {
        let admitted = {
            let state = self.0.borrow();
            // 先复制标量并结束借用，再访问可能重入的原生 getter。
            state.finished && state.failure.is_none() && state.controls_detached
        };
        admitted && self.editors_are_detached() && self.observed_undo_is_empty()
    }

    fn observed_undo_is_empty(&self) -> bool {
        let managers = self.0.borrow().observed_undo.clone();
        managers.iter().all(|manager| {
            // 只检查实际编辑时登记的对象；这里绝不调用可能创建 manager 的 responder getter。
            manager
                .load()
                .is_none_or(|manager| !manager.canUndo() && !manager.canRedo())
        })
    }

    fn editors_are_detached(&self) -> bool {
        let editors = self.0.borrow().editors.clone();
        editors.iter().all(|editor| {
            let Some(editor) = editor.load() else {
                // 框架已经自行释放的缓存自然不再持有旧会话数据。
                return true;
            };
            // SAFETY: 经过动态类型验证的主线程 NSTextView 实现公开 NSTextInputClient getter。
            let marked: bool = unsafe { msg_send![&*editor, hasMarkedText] };
            // SAFETY: weak load 临时持有活的主线程 view，父视图查询仅用于本次同步布尔事实。
            let no_superview = unsafe { editor.superview().is_none() };
            BorrowedEditorFacts {
                text_empty: editor.string().length() == 0,
                unmarked: !marked,
                delegate_detached: editor.delegate().is_none(),
                hierarchy_detached: no_superview && editor.window().is_none(),
                selection_empty: editor.selectedRange().length == 0,
            }
            .verify()
        })
    }

    /// 只报告实际失败原因及缓存实例数，不读取或输出用户文本。
    pub(super) fn failure(&self) -> Option<String> {
        self.0.borrow().failure.clone()
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// 失败时只检查一次有限 weak 角色/公开标量，不输出文本、路径、类名或任意错误内容。
    pub(super) fn failure_diagnostic(&self) -> String {
        let (finished, detached, failed, editors, managers) = {
            let state = self.0.borrow();
            (
                state.finished,
                state.controls_detached,
                state.failure.is_some(),
                state.editors.clone(),
                state.observed_undo.clone(),
            )
        };
        // 全部状态借用已经结束；临时 native 升级不进入收据或后续回调。
        let editor_facts: Vec<_> = editors
            .iter()
            .take(4)
            .enumerate()
            .map(|(index, editor)| {
                if let Some(editor) = editor.load() {
                    // SAFETY: 编辑期间已验证的主线程 NSTextView 实现公开 composition/superview getter。
                    let marked: bool = unsafe { msg_send![&*editor, hasMarkedText] };
                    // SAFETY: weak 升级在主线程临时保活该 view，只读 superclass 声明的父关系。
                    let no_superview = unsafe { editor.superview().is_none() };
                    format!(
                        "editor{index}:live=true,text_empty={},unmarked={},delegate_nil={},super_nil={},window_nil={},selection_empty={}",
                        editor.string().length() == 0,
                        !marked,
                        editor.delegate().is_none(),
                        no_superview,
                        editor.window().is_none(),
                        editor.selectedRange().length == 0,
                    )
                } else {
                    // 已实际 nil 的角色不读取任何其他状态。
                    format!("editor{index}:live=false")
                }
            })
            .collect();
        let undo_facts: Vec<_> = managers
            .iter()
            .take(4)
            .enumerate()
            .map(|(index, manager)| {
                if let Some(manager) = manager.load() {
                    // 只读取实际输入已记录的 manager，不查询可能构造历史的 responder getter。
                    format!(
                        "undo{index}:live=true,canUndo={},canRedo={}",
                        manager.canUndo(),
                        manager.canRedo()
                    )
                } else {
                    format!("undo{index}:live=false")
                }
            })
            .collect();
        format!(
            "finished={finished},controls_detached={detached},failed={failed},editors={editor_facts:?},undo={undo_facts:?}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{BorrowedEditorFacts, complete_accepted_field_cleanup};
    use std::cell::{Cell, RefCell};

    #[test]
    fn final_facts_follow_owned_editor_payload_release() {
        let owned_editor_text = RefCell::new(Some(vec![3_u8; 128]));
        let controls_empty = Cell::new(false);
        let frozen = Cell::new(false);
        let (receipt, result) = complete_accepted_field_cleanup(
            || frozen.set(true),
            || Ok(()),
            || {
                // 反例对应公开 endEditing 的有限保证：客户端结束不代表自有 editor backing string 已空。
                assert!(frozen.get());
                controls_empty.set(true);
                assert!(owned_editor_text.borrow().is_some());
            },
            || Ok(()),
            || {
                // 与生产 provider 相同，明确取走自有数据/owner；不能把这一步放在最终判定之后。
                owned_editor_text.take();
                7_usize
            },
            || {
                assert!(controls_empty.get());
                assert!(owned_editor_text.borrow().is_none());
                Ok(())
            },
        );
        assert_eq!(receipt, 7);
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn capture_and_history_errors_do_not_skip_owner_release_or_replace_first_failure() {
        for fail_capture in [true, false] {
            let owned_payload = RefCell::new(Some(vec![9_u8; 256]));
            let frozen = Cell::new(false);
            let (_, result) = complete_accepted_field_cleanup(
                || frozen.set(true),
                || {
                    assert!(frozen.get());
                    if fail_capture {
                        // 先前 capture 失败也必须进入一次完整的自有 cleanup。
                        Err("capture first".into())
                    } else {
                        Ok(())
                    }
                },
                || assert!(frozen.get()),
                || Err("history failed".into()),
                || {
                    owned_payload.take();
                },
                || {
                    assert!(owned_payload.borrow().is_none());
                    Err("boundary still live".into())
                },
            );
            assert_eq!(
                result,
                Err(if fail_capture {
                    "capture first".into()
                } else {
                    "history failed".into()
                })
            );
        }
    }

    #[test]
    fn released_owned_payload_does_not_authorize_a_failed_borrowed_boundary() {
        let released = Cell::new(false);
        let (_, result) = complete_accepted_field_cleanup(
            || {},
            || Ok(()),
            || {},
            || Ok(()),
            || released.set(true),
            || {
                // 调整时序没有删掉严格成功条件；已释放自有资源也必须拒绝残留共享关系。
                assert!(released.get());
                Err("public facts failed".into())
            },
        );
        assert_eq!(result, Err("public facts failed".into()));
    }

    #[test]
    /// 任一个正文、composition、delegate、层级或选择残留都使公开缓存证明失败。
    fn every_borrowed_editor_data_or_authority_edge_must_be_empty() {
        let empty = BorrowedEditorFacts {
            text_empty: true,
            unmarked: true,
            delegate_detached: true,
            hierarchy_detached: true,
            selection_empty: true,
        };
        assert!(empty.verify());
        for facts in [
            BorrowedEditorFacts {
                text_empty: false,
                ..empty
            },
            BorrowedEditorFacts {
                unmarked: false,
                ..empty
            },
            BorrowedEditorFacts {
                delegate_detached: false,
                ..empty
            },
            BorrowedEditorFacts {
                hierarchy_detached: false,
                ..empty
            },
            BorrowedEditorFacts {
                selection_empty: false,
                ..empty
            },
        ] {
            // 相同的空壳状态不能掩盖另一项仍活跃的业务或回调能力。
            assert!(!facts.verify());
        }
    }
}

/// 只观察本 editor 的公开 Find bar；不把框架子树登记为项目所有者。
#[derive(Clone, Default)]
pub(super) struct NativeMarkdownFindResources {
    fields: Vec<Weak<NSTextField>>,
    finished: bool,
    failed: bool,
}

impl NativeMarkdownFindResources {
    /// 只登记项目栏的两个已知输入，不遍历或改写标准 Find 的私有组合视图。
    pub(super) fn from_fields(fields: impl IntoIterator<Item = Retained<NSTextField>>) -> Self {
        Self {
            fields: fields
                .into_iter()
                .map(|field| Weak::from_retained(&field))
                .collect(),
            finished: false,
            failed: false,
        }
    }

    /// 多次公开 UI 变化仍保留旧活字段，地址复用不掩盖另一个生命周期。
    pub(super) fn merge(&mut self, other: Self) -> Result<(), String> {
        for field in other.fields {
            let Some(current) = field.load() else {
                // 已真实释放的字段没有旧查询数据或回调权限。
                continue;
            };
            let known = self.fields.iter().any(|known| {
                known
                    .load()
                    .is_some_and(|known| std::ptr::eq(&*known, &*current))
            });
            if !known {
                if self.fields.len() == 128 {
                    // 只回收真实 nil 的旧槽，不能丢弃仍存活的旧查询字段。
                    self.fields.retain(|field| field.load().is_some());
                }
                if self.fields.len() == 128 {
                    return Err("笔记查找输入框超过会话数量上限".into());
                }
                self.fields.push(field);
            }
        }
        Ok(())
    }

    /// 返回短暂强引用供已接受的清空过程使用；调用前已结束 editor 状态借用。
    pub(super) fn fields(&self) -> Vec<Retained<NSTextField>> {
        self.fields.iter().filter_map(Weak::load).collect()
    }

    /// 共享 field editor 正常结束后清理本地 Find 字段，不修改全局 find pasteboard。
    pub(super) fn finish_accepted(&mut self) {
        let fields = self.fields();
        for field in fields {
            // 不改框架 delegate/target；清空其业务值并保留已结束 currentEditor 的事实检查。
            field.setStringValue(ns_string!(""));
            field.setEditable(false);
            field.setSelectable(false);
        }
        self.finished = true;
    }

    /// 不完整捕获是永久失败，不能靠后续空查询覆盖。
    pub(super) fn mark_failed(&mut self) {
        self.failed = true;
    }

    /// live Find 组合对象可由框架缓存，但不能继续拥有查询、替换值或活动客户端。
    pub(super) fn is_detached(&self) -> bool {
        self.finished
            && !self.failed
            && self.fields.iter().all(|field| {
                field.load().is_none_or(|field| {
                    field.stringValue().length() == 0
                        && field.currentEditor().is_none()
                        && !field.isEditable()
                        && !field.isSelectable()
                })
            })
    }
}

/// 只弱借用所属会话；原生 editor 自身不能延长历史、ledger 或 controller 生命周期。
pub(super) struct NativeMarkdownSessionFieldEditorIvars {
    undo: RefCell<std::rc::Weak<NativeMarkdownSessionUndo>>,
    ledger: RefCell<std::rc::Weak<NativeMarkdownOwnedLedger>>,
    /// 只归并一次真实协议栈的 Find 通知；不持有字段、栏或文稿强边。
    find_input: NativeMarkdownFindProtocolState,
    /// 只记录实际 setDelegate: 的编辑路由变化；provider 的只读请求不构成换代。
    input_client_revision: Cell<u64>,
    /// 只比较相邻真实 delegate 路由，不解释其类型，也不据此授予字段或数据权限。
    last_input_delegate: Cell<Option<usize>>,
}

define_class!(
    // SAFETY: 保留NSTextView输入协议；公开undoManager及undo:/redo:仅消费同一会话字段历史，ivar先初始化且限主线程。
    #[unsafe(super = NSTextView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeMarkdownSessionFieldEditorIvars]
    pub(super) struct NativeMarkdownSessionFieldEditor;

    // SAFETY: 初始化的主线程 NSTextView 子类保留标准 NSObject 身份。
    unsafe impl NSObjectProtocol for NativeMarkdownSessionFieldEditor {}

    impl NativeMarkdownSessionFieldEditor {
        #[unsafe(method(setDelegate:))]
        /// 跟随 AppKit 的实际编辑路由交接；只读 provider 查询不能撤销正在执行的输入票。
        fn set_delegate(&self, delegate: Option<&ProtocolObject<dyn NSTextViewDelegate>>) {
            let address = delegate.map(|delegate| std::ptr::from_ref(delegate).addr());
            if self.ivars().last_input_delegate.get() != address {
                // 先撤销旧路由再进入可能重入的系统 setter；A→B→A 不能让 A 的旧输入票复活。
                let revision = self
                    .input_client_revision()
                    .checked_add(1)
                    .expect("字段编辑路由代次溢出");
                self.ivars().last_input_delegate.set(address);
                self.ivars().input_client_revision.set(revision);
            }
            // SAFETY: 主线程 receiver 及原 delegate 参数仍在回调栈；super 恰执行一次，保留系统弱关系与输入行为。
            unsafe { let _: () = msg_send![super(self), setDelegate: delegate]; }
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        /// 候选仍完整交 AppKit；最外层返回后才观察原字段最终组合状态。
        ///
        /// # Safety
        /// string 须为本次主线程回调内存活的 NSString 或 NSAttributedString，范围遵守 NSTextInputClient 合同。
        unsafe fn set_marked_text_selected_range_replacement_range(
            &self,
            string: &AnyObject,
            selected_range: NSRange,
            replacement_range: NSRange,
        ) {
            let scope = self.ivars().find_input.enter(self.as_super(), "marked");
            // SAFETY: 原输入对象和范围仍在实际回调栈；仅向原 AppKit 实现转发一次，不改候选或历史。
            unsafe { let _: () = msg_send![super(self), setMarkedText: string, selectedRange: selected_range, replacementRange: replacement_range]; }
            scope.finish();
        }

        #[unsafe(method(unmarkText))]
        /// 解除标记由 AppKit 完成；嵌套调用归入原输入栈，不提前刷新查询。
        fn unmark_text(&self) {
            let scope = self.ivars().find_input.enter(self.as_super(), "unmark");
            // SAFETY: 原生主线程 receiver 存活；公开 selector 无额外参数，super 只执行一次。
            unsafe { let _: () = msg_send![super(self), unmarkText]; }
            scope.finish();
        }

        #[unsafe(method(insertText:replacementRange:))]
        /// 提交与验证仍由原 AppKit 实现执行；通知只在真实最外层完成后刷新一次 Find。
        ///
        /// # Safety
        /// string 须为当前主线程输入栈内存活的受支持文本对象，replacement_range 保留原公开协议语义。
        unsafe fn insert_text_replacement_range(&self, string: &AnyObject, replacement_range: NSRange) {
            let scope = self.ivars().find_input.enter(self.as_super(), "insert");
            // SAFETY: 原文本及范围保持不变，AppKit 独占标准提交、Undo 分组与字段校验。
            unsafe { let _: () = msg_send![super(self), insertText: string, replacementRange: replacement_range]; }
            scope.finish();
        }

        #[unsafe(method_id(undoManager))]
        /// 标准输入仍由 AppKit 实现；实际使用的历史显式归本会话，不推断窗口默认值。
        fn undo_manager(&self) -> Option<Retained<NSUndoManager>> {
            let undo = self.ivars().undo.borrow().upgrade();
            let ledger = self.ivars().ledger.borrow().upgrade();
            match (undo, ledger) {
                // 先结束两个 RefCell 借用，再运行按需 manager 构造及账本登记。
                (Some(undo), Some(ledger)) => undo.manager(&ledger),
                // accepted release 已移走 weak 能力，旧原生 editor 不能重建历史。
                _ => None,
            }
        }

        #[unsafe(method(undo:))]
        /// 字段先处理标准撤销，避免响应链落到正文后先提交草稿再撤销canonical。
        fn undo(&self, _sender: Option<&AnyObject>) {
            if let Err(error) = self.restore_field_history(MarkdownHistoryDirection::Undo) {
                present_native_markdown_error(&error);
            }
        }

        #[unsafe(method(redo:))]
        /// 本地重做不辞任字段，也不因正文恰有历史而转移目标。
        fn redo(&self, _sender: Option<&AnyObject>) {
            if let Err(error) = self.restore_field_history(MarkdownHistoryDirection::Redo) {
                present_native_markdown_error(&error);
            }
        }
    }
);

impl NativeMarkdownSessionFieldEditor {
    /// 首个 Native getter 前取得原编辑路由代次；getter 重入真实 delegate 交接会撤销原输入观察。
    pub(super) fn input_client_revision(&self) -> u64 {
        self.ivars().input_client_revision.get()
    }

    /// 只操作当前字段已记录的manager；标准selector在本receiver终止，空栈亦不转发到正文。
    fn restore_field_history(&self, direction: MarkdownHistoryDirection) -> Result<(), String> {
        let undo = self.ivars().undo.borrow().upgrade();
        let ledger = self.ivars().ledger.borrow().upgrade();
        let (Some(undo), Some(ledger)) = (undo, ledger) else {
            return Ok(()); // accepted已撤销两个Weak能力，迟到的字段命令不重建历史。
        }; // Rust Weak升级结束全部RefCell借用，随后才允许Native getter或manager动作重入。
        undo.preflight()?;
        let Some(manager) = undo.cached_manager(&ledger) else {
            return Ok(()); // 没有既存字段历史就消费为空操作，不能创建manager或借用正文历史。
        };
        let Some(window) = self.window() else {
            return Ok(()); // 已离开窗口的框架缓存不是可编辑客户端。
        };
        let current = || {
            let focused = window.firstResponder().is_some_and(|responder| {
                std::ptr::from_ref(&*responder).addr() == std::ptr::from_ref(self).addr()
            });
            // firstResponder的retain/release也可能重入；最后只核原会话与原manager，绝不重捕后继字段。
            focused
                && self.belongs_to(&undo, &ledger)
                && undo.owns_observed_manager(&manager)
                && undo.preflight().is_ok()
        };
        if !current() {
            return Ok(()); // 标准字段动作只授予原真实焦点，不发送给窗口中另一个客户端。
        }
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super());
        if input.hasMarkedText() {
            return Err("请先完成当前输入，再撤销或重做".into()); // 保留IME组合，不强制确认或取消它。
        }
        if manager.isUndoing() || manager.isRedoing() || manager.groupingLevel() > 1 {
            return Err("当前输入的撤销或重做仍在完成中".into()); // 不嵌套执行，也不手工结束原生输入组。
        }
        let available = match direction {
            MarkdownHistoryDirection::Undo => manager.canUndo(),
            MarkdownHistoryDirection::Redo => manager.canRedo(),
        };
        if !available || !current() {
            return Ok(()); // checkpoint可以改变客户；空栈或已撤权都保留字段，不降为正文命令。
        }
        match direction {
            MarkdownHistoryDirection::Undo => manager.undo(),
            MarkdownHistoryDirection::Redo => manager.redo(),
        } // AppKit发布本地文本/选择及反向历史；它合法推进输入epoch，之后不再消费旧before。
        Ok(())
    }

    /// 标准公开 initializer 在 set_ivars 后运行；不改写系统输入、IME、验证和文本命令。
    fn new(
        undo: &Rc<NativeMarkdownSessionUndo>,
        ledger: &Rc<NativeMarkdownOwnedLedger>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(NativeMarkdownSessionFieldEditorIvars {
            undo: RefCell::new(Rc::downgrade(undo)),
            ledger: RefCell::new(Rc::downgrade(ledger)),
            find_input: NativeMarkdownFindProtocolState::default(),
            input_client_revision: Cell::new(0),
            last_input_delegate: Cell::new(None),
        });
        // SAFETY: 主线程 NSLayout/TextView 的公开初始化签名与 NSRect ABI 一致，Rust ivar 已完整安装。
        let editor: Retained<Self> =
            unsafe { msg_send![super(allocated), initWithFrame: NSRect::ZERO] };
        editor.setFieldEditor(true);
        editor.setAllowsUndo(true);
        editor.setRichText(false);
        editor.setImportsGraphics(false);
        #[cfg(feature = "native-markdown-runtime-probe")]
        println!(
            "native_field_checking_policy phase=before enabled_types={}",
            editor.enabledTextCheckingTypes()
        ); // 只观察公开标量，不取得检查controller或正文对象。
        super::text_input_policy::configure_literal_text_input(editor.as_super());
        #[cfg(feature = "native-markdown-runtime-probe")]
        println!(
            "native_field_checking_policy phase=after enabled_types={}",
            editor.enabledTextCheckingTypes()
        ); // 配置后只输出同一标量，不增加输入动作、历史或延迟任务。
        editor
    }

    /// 只在 accepted 清理中撤销历史能力并清空本 editor 的公开数据，不调用私有缓存 API。
    fn seal_and_empty(&self) {
        self.ivars().find_input.seal(); // accepted 先撤协议弱票，后续清值不能刷新已结束笔记。
        let undo = self.ivars().undo.replace(std::rc::Weak::new());
        let ledger = self.ivars().ledger.replace(std::rc::Weak::new());
        drop((undo, ledger));
        self.setAllowsUndo(false);
        self.setString(ns_string!(""));
        self.setSelectedRange(NSRange::new(0, 0));
        self.setDelegate(None);
        // SAFETY: accepted 后 editor 已离开真实客户端；清除 nullable responder 边不转移任何 owner。
        unsafe { self.setNextResponder(None) };
    }

    pub(super) fn belongs_to(
        &self,
        undo: &NativeMarkdownSessionUndo,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> bool {
        let current_undo = self.ivars().undo.borrow().upgrade();
        let current_ledger = self.ivars().ledger.borrow().upgrade();
        current_undo.is_some_and(|value| std::ptr::eq(&*value, undo))
            && current_ledger.is_some_and(|value| std::ptr::eq(&*value, ledger))
    }
}

/// 窗口公开 provider 最多拥有一个标准行为的具体 editor；nil-client 查询不会创建对象。
pub(super) struct NativeMarkdownFieldEditorProvider {
    editor: RefCell<Option<Retained<NativeMarkdownSessionFieldEditor>>>,
    undo: std::rc::Weak<NativeMarkdownSessionUndo>,
    ledger: std::rc::Weak<NativeMarkdownOwnedLedger>,
    borrowed: RefCell<NativeMarkdownBorrowedEditorSnapshot>,
    creating: Cell<bool>,
    closed: Cell<bool>,
}

impl NativeMarkdownFieldEditorProvider {
    /// 首个 Native getter 前固定既存 owner 的实际地址和编辑路由；不创建、retain 或读取输入值。
    pub(super) fn current_input_client_identity(&self) -> Option<(usize, u64)> {
        self.editor.borrow().as_ref().map(|editor| {
            (
                std::ptr::from_ref(&**editor).addr(),
                editor.input_client_revision(),
            )
        })
    }

    /// 同一 owner 的路由观察复用原标量快照，不建立另一套编辑客户记录。
    pub(super) fn current_input_client_revision(&self, editor_address: usize) -> Option<u64> {
        self.current_input_client_identity()
            .and_then(|(address, revision)| (address == editor_address).then_some(revision))
    }

    /// 通知借原 provider 的既存 editor 记录同 field 协议事实；不创建 editor 或调用 Native getter。
    pub(super) fn defer_find_input(
        &self,
        controller: &NativeMarkdownController,
        field: &NSTextField,
        changed: bool,
        ended: bool,
    ) -> bool {
        self.editor.borrow().as_ref().is_some_and(|editor| {
            editor.ivars().find_input.defer_notification(
                controller,
                field,
                editor.input_client_revision(),
                changed,
                ended,
            )
        })
    }

    /// modal路由只读provider当前权限；构造或封闭期间不能执行活动字段历史。
    pub(super) fn permits_modal_command(&self) -> bool {
        !self.closed.get() && !self.creating.get()
    }

    pub(super) fn new(
        undo: &Rc<NativeMarkdownSessionUndo>,
        ledger: &Rc<NativeMarkdownOwnedLedger>,
        borrowed: &NativeMarkdownBorrowedEditorSnapshot,
    ) -> Self {
        Self {
            editor: RefCell::new(None),
            undo: Rc::downgrade(undo),
            ledger: Rc::downgrade(ledger),
            borrowed: RefCell::new(borrowed.clone()),
            creating: Cell::new(false),
            closed: Cell::new(false),
        }
    }

    /// 公开 windowWillReturnFieldEditor:toObject: 入口；只在实际非 nil 客户请求时分配。
    pub(super) fn provide(
        &self,
        client: Option<&AnyObject>,
        mtm: MainThreadMarker,
    ) -> Option<Retained<AnyObject>> {
        if self.closed.get() {
            // 关闭、handoff 或迟到查询都不能创建后继编辑能力。
            return None;
        }
        if client.is_none() && self.editor.borrow().is_none() {
            // 未创建对象的只读查询不是嵌套构造，不把正常 initializer 观察误记为失败。
            return None;
        }
        let lease = match FieldEditorCreationLease::enter(&self.creating, &self.closed) {
            Ok(lease) => lease,
            Err(error) => {
                // 只保留首因；嵌套请求不构造或发布第二个 editor。
                self.record_failure(error.into());
                return None;
            }
        };
        let cached = self.editor.take();
        let editor = if let Some(editor) = cached {
            // nil-client 可观察已存在 owner，但不构造；retain 发生在 RefCell 借用结束后。
            editor
        } else {
            // 无 cached editor 的 nil-client 观察始终无分配，也不调用原生 initializer。
            client?;
            self.create(mtm)?
        };
        let returned = editor.clone();
        let (editor, returned) = match lease.complete((editor, returned)) {
            Ok(candidate) => candidate,
            Err(((editor, returned), error)) => {
                // freeze 可发生在 initializer、登记或 retain 重入中；清空未发布对象的能力再 drop。
                self.record_failure(error.into());
                editor.seal_and_empty();
                drop((returned, editor));
                return None;
            }
        };
        // 最终权限检查后只移动 owner，不再 retain/call/drop 可能重入的旧对象。
        let previous = self.editor.replace(Some(editor));
        assert!(
            previous.is_none(),
            "a nested field editor bypassed its creation lease"
        );
        Some(returned.into())
    }

    /// 原生构造与有限观察都在 owner 发布前完成，失败仍留下可拒绝预检的首因。
    fn create(&self, mtm: MainThreadMarker) -> Option<Retained<NativeMarkdownSessionFieldEditor>> {
        let undo = self.undo.upgrade()?;
        let ledger = self.ledger.upgrade()?;
        let editor = NativeMarkdownSessionFieldEditor::new(&undo, &ledger, mtm);
        if let Err(error) = ledger.include("session.field-editor", &editor) {
            // delegate 无 Result 通道；记录失败使预检拒绝，不盲清随后可能出现的系统 manager。
            undo.record_failure(error);
            return None;
        }
        let borrowed = self.borrowed.borrow().clone();
        if let Err(error) = borrowed.capture_editor(&editor) {
            undo.record_failure(error);
            return None;
        }
        Some(editor)
    }

    /// 复制纯 Rust owner 后结束全部借用；错误记录不持 native 对象或修改未知 undo。
    fn record_failure(&self, error: String) {
        if let Some(undo) = self.undo.upgrade() {
            undo.record_failure(error);
        }
    }

    /// 正常结束共享编辑关系之后才取走强 owner；返回 weak 供旧交接要求实际 nil。
    pub(super) fn finish_accepted(&self) -> Option<Weak<NativeMarkdownSessionFieldEditor>> {
        self.freeze();
        let editor = self.editor.take();
        let weak = editor.as_ref().map(Weak::from_retained);
        if let Some(editor) = editor {
            // 公共数据与 weak 业务能力都先清空，再释放项目拥有的 native 引用。
            editor.seal_and_empty();
        }
        weak
    }

    /// accepted 边界先撤销创建权限，仍保留现有 owner 供一次正常 endEditing 清理。
    pub(super) fn freeze(&self) {
        self.closed.set(true);
    }

    /// 旧图已通过收据后，允许新客户端按需得到全新 editor；从不恢复旧 editor 或旧历史。
    pub(super) fn resume(&self, borrowed: &NativeMarkdownBorrowedEditorSnapshot) {
        if !self.closed.get() {
            // 初次挂载或同一空工作区刷新没有 accepted 交接，保留当前正常编辑客户端。
            return;
        }
        if self.creating.get() {
            // 未返回的旧构造栈不能被一次 resume 复活成新代次，防止 closed 布尔 ABA。
            self.record_failure("笔记输入控件在创建期间重新激活".into());
            return;
        }
        assert!(
            self.editor.borrow().is_none(),
            "old session field editor owner remains"
        );
        *self.borrowed.borrow_mut() = borrowed.clone();
        self.closed.set(false);
    }
}

#[cfg(test)]
mod provider_tests {
    use super::*;

    #[test]
    fn cold_provider_holds_no_native_or_strong_session_owner() {
        // 不构造 AppKit；只验证按需 provider 的 Rust 强弱所有权和空初始状态。
        let undo = Rc::new(NativeMarkdownSessionUndo::default());
        let ledger = Rc::new(NativeMarkdownOwnedLedger::default());
        let borrowed = NativeMarkdownBorrowedEditorSnapshot::default();
        let provider = NativeMarkdownFieldEditorProvider::new(&undo, &ledger, &borrowed);
        assert_eq!(Rc::strong_count(&undo), 1);
        assert_eq!(Rc::strong_count(&ledger), 1);
        assert!(provider.editor.borrow().is_none());
        drop((undo, ledger));
        assert!(provider.undo.upgrade().is_none());
        assert!(provider.ledger.upgrade().is_none());
    }

    #[test]
    fn resume_preserves_active_tracker_and_only_reopens_an_accepted_empty_owner() {
        let undo = Rc::new(NativeMarkdownSessionUndo::default());
        let ledger = Rc::new(NativeMarkdownOwnedLedger::default());
        let initial = NativeMarkdownBorrowedEditorSnapshot::default();
        let next = NativeMarkdownBorrowedEditorSnapshot::default();
        let provider = NativeMarkdownFieldEditorProvider::new(&undo, &ledger, &initial);
        provider.resume(&next);
        // 同一活动空工作区刷新不丢掉已观察的旧身份，且不重建 editor。
        assert!(Rc::ptr_eq(&provider.borrowed.borrow().0, &initial.0));
        assert!(provider.finish_accepted().is_none());
        assert!(provider.closed.get());
        provider.resume(&next);
        // 调用方已验证旧收据后才切换到新 tracker，下一次实际客户端请求方可按需构造。
        assert!(!provider.closed.get());
        assert!(provider.editor.borrow().is_none());
        assert!(Rc::ptr_eq(&provider.borrowed.borrow().0, &next.0));
    }

    #[test]
    fn close_during_initializer_drops_candidate_without_publishing_or_reopening() {
        struct Candidate(Rc<Cell<usize>>);
        impl Drop for Candidate {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let undo = Rc::new(NativeMarkdownSessionUndo::default());
        let ledger = Rc::new(NativeMarkdownOwnedLedger::default());
        let borrowed = NativeMarkdownBorrowedEditorSnapshot::default();
        let provider = NativeMarkdownFieldEditorProvider::new(&undo, &ledger, &borrowed);
        let released = Rc::new(Cell::new(0));
        let published = Cell::new(0);
        {
            let lease = FieldEditorCreationLease::enter(&provider.creating, &provider.closed)
                .expect("outer construction accepted");
            // 模拟 initializer 完成前同步回调接受关闭；工厂已经取得尚未发布的真实所有权。
            let candidate = Candidate(Rc::clone(&released));
            provider.freeze();
            provider.resume(&NativeMarkdownBorrowedEditorSnapshot::default());
            match lease.complete(candidate) {
                Ok(_candidate) => published.set(published.get() + 1),
                Err((candidate, _)) => {
                    // 与生产相同的最终权限检查把 candidate 交还，关闭后的 owner 槽不会再获得它。
                    drop(candidate);
                }
            }
        }
        assert_eq!(released.get(), 1);
        assert_eq!(published.get(), 0);
        assert!(provider.editor.borrow().is_none());
        assert!(provider.closed.get());
        assert!(!provider.creating.get());
    }

    #[test]
    fn nested_initializer_is_rejected_and_unwind_restores_outer_admission() {
        let creating = Cell::new(false);
        let closed = Cell::new(false);
        let factory_calls = Cell::new(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _outer = FieldEditorCreationLease::enter(&creating, &closed)
                .expect("one outer construction");
            factory_calls.set(factory_calls.get() + 1);
            if let Ok(_nested) = FieldEditorCreationLease::enter(&creating, &closed) {
                // 只有取得同一生产门禁才能进入工厂；回归会使计数错误增加。
                factory_calls.set(factory_calls.get() + 1);
            }
            assert!(
                creating.get(),
                "nested rejection cannot reset the outer lease"
            );
            panic!("synthetic initializer unwind");
        }));
        assert!(result.is_err());
        assert_eq!(factory_calls.get(), 1);
        assert!(!creating.get());
        // 异常只复位栈权限，不制造永久 Busy；下一次独立构造仍能取得门禁。
        assert!(FieldEditorCreationLease::enter(&creating, &closed).is_ok());
        assert!(!creating.get());
    }

    #[test]
    fn provider_and_late_observation_failures_preserve_the_first_cause() {
        let undo = Rc::new(NativeMarkdownSessionUndo::default());
        let ledger = Rc::new(NativeMarkdownOwnedLedger::default());
        let provider = NativeMarkdownFieldEditorProvider::new(
            &undo,
            &ledger,
            &NativeMarkdownBorrowedEditorSnapshot::default(),
        );
        undo.record_failure("first registration failure".into());
        provider.record_failure("later observation failure".into());
        provider.freeze();
        provider.record_failure("late closed initializer".into());
        assert_eq!(undo.preflight(), Err("first registration failure".into()));
    }
}
