//! 显式构造角色的有限弱账本；共享编辑器与框架子树不自动归为项目所有。

use super::*;
use omni_markdown_core::MarkdownOriginalBackingObserver;
use std::collections::HashMap;

/// 总预算仅覆盖进入OwnedLedger的显式身份，包含所有scope及仍活旧对象；不是Foundation对象总数。
const MAX_OWNED_NATIVE_IDENTITIES: usize = 1_024;

#[cfg(feature = "native-close-stage-diagnostic")]
/// 只保存一次关闭观察的纯事实；地址仅供离线追踪，不再解引用或作为释放权限。
pub(super) struct NativeMarkdownCloseRasterIdentity {
    role: &'static str,
    address: usize,
    class: String,
    length: Option<usize>,
}

#[cfg(feature = "native-close-stage-diagnostic")]
impl std::fmt::Debug for NativeMarkdownCloseRasterIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{{role:{:?},ptr:{:#x},class:{:?},length:{:?}}}",
            self.role, self.address, self.class, self.length,
        )
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
/// scope 由构造者明确指定，只决定既有收据的子集，不是原生类型豁免。
enum NativeMarkdownOwnedScope {
    Surface,
    Editor,
    Modal,
}

#[derive(Clone)]
struct OwnedNativeIdentity {
    role: &'static str,
    object: Weak<AnyObject>,
    scope: NativeMarkdownOwnedScope,
}

#[derive(Clone)]
/// 仅保留原映射owner的真实VM事实；Doc或Snapshot的其他alias不能被Weak-nil掩盖。
struct OwnedOriginalBacking {
    role: &'static str,
    observer: MarkdownOriginalBackingObserver,
    scope: NativeMarkdownOwnedScope,
}

impl OwnedOriginalBacking {
    /// 单独的零长度或Drop进度均不足够；先前读取/回收失败同样永久阻止成功收据。
    fn released(&self) -> bool {
        self.observer.unmap_succeeded()
            && self.observer.mapped_bytes() == 0
            && !self.observer.failed()
    }
}

#[derive(Clone, Default)]
/// 收据只拥有weak与纯原子元数据；标准控件要求实际nil，源映射要求真实munmap成功。
pub(super) struct NativeMarkdownOwnedSnapshot {
    identities: Vec<OwnedNativeIdentity>,
    original_backings: Vec<OwnedOriginalBacking>,
    addresses: HashMap<usize, usize>,
    incomplete: bool,
}

impl NativeMarkdownOwnedSnapshot {
    /// 仅私有退出诊断使用一次既有Weak清单；统计不是资源收据，不添加或漏掉任何身份。
    #[cfg(feature = "native-close-stage-diagnostic")]
    pub(super) fn diagnostic_alive_roles(&self) -> (bool, Vec<(&'static str, usize, usize)>) {
        let mut roles = std::collections::BTreeMap::<&'static str, (usize, usize)>::new();
        for entry in &self.identities {
            // 原清单最多1024身份；临时weak升级在本次局部pool中结束，没有账本借用跨native。
            let alive = entry.object.load().is_some();
            let counts = roles.entry(entry.role).or_default();
            counts.0 += 1;
            counts.1 += usize::from(alive);
        }
        (
            !self.incomplete,
            roles
                .into_iter()
                .map(|(role, (total, alive))| (role, total, alive))
                .collect(),
        )
    }

    /// 只观察原账本中侧栏的两个已登记Weak，不遍历框架子树或建立额外身份清单。
    /// 调用方在同一局部pool内消费；所有Native临时claim结束后才输出返回的纯标量文本。
    #[cfg(feature = "native-close-stage-diagnostic")]
    pub(super) fn diagnostic_sidebar_relationships(&self) -> String {
        let table = self
            .identities
            .iter()
            .find(|entry| entry.role == "sidebar.table")
            .and_then(|entry| entry.object.load());
        let table_facts = match table.as_deref() {
            None => "live=false".to_owned(),
            Some(object) => match object.downcast_ref::<objc2_app_kit::NSTableView>() {
                Some(table) => {
                    // 类型由原构造角色及动态类型共同确认；公开getter只读取关系/数量，不读行内容。
                    // SAFETY: 原主线程NSTableView仍由本次Weak升级保活，未持有TLS或账本可变借用。
                    let (super_attached, data_source_attached, delegate_attached) = unsafe {
                        (
                            table.superview().is_some(),
                            table.dataSource().is_some(),
                            table.delegate().is_some(),
                        )
                    };
                    format!(
                        "live=true type_match=true window_attached={} super_attached={super_attached} data_source_attached={data_source_attached} delegate_attached={delegate_attached} target_attached={} header_attached={} corner_attached={} columns={} rows={}",
                        table.window().is_some(),
                        table.target().is_some(),
                        table.headerView().is_some(),
                        table.cornerView().is_some(),
                        table.numberOfColumns(),
                        table.numberOfRows(),
                    )
                }
                None => "live=true type_match=false".to_owned(),
            },
        };
        drop(table); // 不把table的临时Native强边带入下一个角色或stage。
        let column = self
            .identities
            .iter()
            .find(|entry| entry.role == "sidebar.column")
            .and_then(|entry| entry.object.load());
        let column_facts = match column.as_deref() {
            None => "live=false".to_owned(),
            Some(object) => match object.downcast_ref::<objc2_app_kit::NSTableColumn>() {
                Some(column) => format!(
                    "live=true type_match=true table_attached={}",
                    column.tableView().is_some(),
                ),
                None => "live=true type_match=false".to_owned(),
            },
        };
        drop(column); // getter返回值与原Weak升级都只归本次观察，不延长关闭资源的真实寿命。
        format!("table=[{table_facts}] column=[{column_facts}]")
    }

    /// 与首次关闭角色统计同池执行；只记录指定既有Weak，不安装额外观察或改变nil门禁。
    #[cfg(feature = "native-close-stage-diagnostic")]
    pub(super) fn diagnostic_raster_close_identities(
        &self,
    ) -> (Vec<NativeMarkdownCloseRasterIdentity>, bool) {
        let mut identities = Vec::with_capacity(8);
        let mut truncated = false;
        for entry in &self.identities {
            if !matches!(
                entry.role,
                "editor.raster.body-layer"
                    | "editor.raster.gutter-layer"
                    | "editor.raster.layer"
                    | "editor.raster.line-string"
            ) {
                continue; // 角色由构造边显式登记；不按类名扩张目标或推断所有权。
            }
            let Some(object) = entry.object.load() else {
                continue; // 实际nil不造身份；本函数的存活或缺失都不替代原释放检查。
            };
            if identities.len() == 8 {
                truncated = true;
                continue; // 只确认其仍活，不再读取超额对象的类名或长度。
            }
            let name = object.class().name().to_string_lossy();
            let mut end = name.len().min(128);
            while !name.is_char_boundary(end) {
                end -= 1; // 按UTF8边界截短到最多128字节，不读取对象正文或描述。
            }
            let class = name[..end].to_owned();
            let length = if entry.role == "editor.raster.line-string" {
                object.downcast_ref::<NSString>().map(NSString::length)
            } else {
                None // 图层绝不发送字符串getter；已知字符串角色也须真实类型认证。
            };
            identities.push(NativeMarkdownCloseRasterIdentity {
                role: entry.role,
                address: std::ptr::from_ref(&*object).addr(),
                class,
                length,
            }); // 仅移动标量和短Rust字符串；本轮临时Native claim随迭代归还。
        }
        (identities, truncated)
    }

    /// 仅登记调用方已经确认创建或配置的对象；不通过可达性或类名推断所有权。
    fn include(
        &mut self,
        role: &'static str,
        object: &AnyObject,
        scope: NativeMarkdownOwnedScope,
    ) -> Result<(), String> {
        let address = std::ptr::from_ref(object).addr();
        if self.addresses.get(&address).is_some_and(|index| {
            // 旧 weak 仍存活才算同一生命周期，地址复用必须创建新记录。
            self.identities[*index].object.load().is_some()
        }) {
            return Ok(());
        }
        self.make_room()?;
        self.addresses.insert(address, self.identities.len());
        self.identities.push(OwnedNativeIdentity {
            role,
            object: Weak::new(object),
            scope,
        });
        Ok(())
    }

    /// 映射和Native身份共享一个session总预算；只回收已有真实释放证据的历史槽。
    fn make_room(&mut self) -> Result<(), String> {
        if self.identities.len() + self.original_backings.len() == MAX_OWNED_NATIVE_IDENTITIES {
            // Native仍只按实际nil淘汰；映射还必须证明无失败且最终munmap成功。
            self.identities
                .retain(|entry| entry.object.load().is_some());
            self.original_backings.retain(|entry| !entry.released());
            self.addresses.clear();
            for (index, entry) in self.identities.iter().enumerate() {
                if let Some(object) = entry.object.load() {
                    // 重建地址索引不推断映射身份；观察者不含可复用为权限的正文地址。
                    self.addresses
                        .insert(std::ptr::from_ref(&*object).addr(), index);
                }
            }
        }
        if self.identities.len() + self.original_backings.len() == MAX_OWNED_NATIVE_IDENTITIES {
            // 拒绝新资源后留下永久不完整事实，不能在调用方清理后冒签缺项的成功快照。
            self.incomplete = true;
            return Err("笔记自有资源登记超过容量上限".into());
        }
        Ok(())
    }

    /// 构造边显式登记一次原映射；克隆observer只共享固定原子，不复制或保活映射。
    fn include_original_backing(
        &mut self,
        role: &'static str,
        observer: MarkdownOriginalBackingObserver,
        scope: NativeMarkdownOwnedScope,
    ) -> Result<(), String> {
        self.make_room()?;
        self.original_backings.push(OwnedOriginalBacking {
            role,
            observer,
            scope,
        });
        Ok(())
    }

    /// 关闭停止构造后释放地址索引，仅保留有限角色和零化 weak。
    pub(super) fn seal(&mut self) {
        self.addresses = HashMap::new();
    }

    /// 最终文稿仍活时一次取得其实际映射票；编辑历史的文件段同原文件一样必须真实归还。
    pub(super) fn capture_document_backings(
        &mut self,
        observers: impl Iterator<Item = MarkdownOriginalBackingObserver>,
    ) {
        // 这里不发布业务、不调用Native，也不新增构造预算；只有Doc实际仍拥有的映射进入收据。
        // 用完整当前集合替换构造期原文件票，避免同一映射重复登记或漏掉已发布的编辑文件。
        self.original_backings = observers
            .map(|observer| OwnedOriginalBacking {
                role: "editor.document-backing",
                observer,
                scope: NativeMarkdownOwnedScope::Editor,
            })
            .collect();
    }

    /// 只给显式标准对象安装真实销毁事件；具体 TextKit 空壳由独立类型证明。
    pub(super) fn track_remaining(
        &self,
        cohort: &mut NativeMarkdownDeallocationCohort,
        mtm: MainThreadMarker,
    ) {
        for entry in &self.identities {
            if let Some(object) = entry.object.load() {
                // marker 不反向持有对象，临时强引用在本轮迭代结束。
                cohort.track(&object, mtm);
            }
        }
    }

    /// 真实检查每个角色，不用父根 nil 代替独立字段/cell/menu 的释放。
    pub(super) fn verify_released(&self) -> Result<(), String> {
        if self.incomplete {
            // 登记中的重入快照没有完整权限，不能把暂借期间的空槽当作已释放。
            return Err("笔记自有资源快照不完整".into());
        }
        let roles: Vec<_> = self
            .identities
            .iter()
            .filter_map(|entry| {
                // 失败只报告静态角色，不读取正文、路径或私有类名。
                entry.object.load().map(|_| entry.role)
            })
            .collect();
        if !roles.is_empty() {
            return Err(format!("笔记原生资源未完成释放：{roles:?}"));
        }
        let mappings: Vec<_> = self
            .original_backings
            .iter()
            .filter(|entry| !entry.released())
            .map(|entry| entry.role)
            .collect();
        if !mappings.is_empty() {
            // Doc的Weak或Native图均已nil也不能代替共享Snapshot最终归还原映射的事实。
            return Err(format!("笔记源映射未完成真实释放：{mappings:?}"));
        }
        Ok(())
    }
}

#[cfg(feature = "native-raster-surface-probe")]
/// 只记源码固定LF在读文档前的实际原Weak；无强字符串、正文、Native图或进程级缓存。
struct NativeMarkdownPreexistingLiteral {
    original: Weak<NSString>,
    eligible_after_pool: bool,
    observations: Cell<usize>,
}

/// 一个 backing 持有实际 weak 清单；Modal handle 共享它，不创建第三份身份预算。
struct NativeMarkdownOwnedLedgerState {
    snapshot: RefCell<Option<NativeMarkdownOwnedSnapshot>>,
    registering: Cell<Option<usize>>,
    // 若root/local登记中途被拒绝，保留负面事实，不允许缺失映射的后续快照成功。
    mapping_registration_failed: Cell<bool>,
    #[cfg(feature = "native-raster-surface-probe")]
    preexisting_literal: Option<NativeMarkdownPreexistingLiteral>,
}

/// Session root统一计数，具体后端保存其Native局部收据；原映射另归外层文稿收据。
/// 父链接只有Rust Weak，不保活前台图或canonical模型。
pub(super) struct NativeMarkdownOwnedLedger {
    state: Rc<NativeMarkdownOwnedLedgerState>,
    root: Option<std::rc::Weak<NativeMarkdownOwnedLedgerState>>,
    scope: NativeMarkdownOwnedScope,
}

impl Default for NativeMarkdownOwnedLedger {
    fn default() -> Self {
        Self {
            state: Rc::new(NativeMarkdownOwnedLedgerState {
                snapshot: RefCell::new(Some(NativeMarkdownOwnedSnapshot::default())),
                registering: Cell::new(None),
                mapping_registration_failed: Cell::new(false),
                #[cfg(feature = "native-raster-surface-probe")]
                preexisting_literal: None,
            }),
            root: None,
            scope: NativeMarkdownOwnedScope::Surface,
        }
    }
}

impl NativeMarkdownOwnedLedger {
    /// 仅真实WindowSurface构造最前调用；时序早于pager、open_document_buffer及任何文档getter。
    /// Default故意不调用本入口，测试或离线probe若已经有文档，就不能事后获得预存身份。
    pub(super) fn for_native_session_before_document_open() -> Self {
        let mut ledger = Self::default();
        #[cfg(feature = "native-raster-surface-probe")]
        if super::raster_surface::NativeMarkdownRasterSurface::requested() {
            // 固定工程字面量独立进入Foundation；不是从用户正文、幸存行或现有Native值复制。
            let original = autoreleasepool(|_| {
                let literal = NSString::from_str("\n");
                Weak::from_retained(&literal)
            });
            // 原构造强claim与本地pool已真正结束。若Weak已nil，本session永久无借用资格。
            let eligible_after_pool = autoreleasepool(|_| original.load().is_some());
            let state = Rc::get_mut(&mut ledger.state).expect("新session账本尚未共享");
            state.preexisting_literal = Some(NativeMarkdownPreexistingLiteral {
                original,
                eligible_after_pool,
                observations: Cell::new(0),
            });
            eprintln!(
                "native_raster_surface event=literal_baseline before_document_open=true source_fixed_lf=true weak_alive_after_pool={eligible_after_pool} strong_owner=false formal=false"
            );
        }
        #[cfg(not(feature = "native-raster-surface-probe"))]
        let _ = &mut ledger; // 普通构建没有任何Foundation基线构造或额外Native读。
        ledger
    }

    #[cfg(feature = "native-raster-surface-probe")]
    /// 只把标准getter返回的同一预存身份记为借用；不改变include或任何自有对象的strictWeak合同。
    pub(super) fn observe_preexisting_literal_string(
        &self,
        role: &'static str,
        string: &NSString,
        units: usize,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<bool, String> {
        if !current() {
            return Err("原光栅票在借用字面量观察前已撤销".into());
        }
        if units != 1 {
            return Ok(false); // 只允许与预先固定LF同长度的实际getter值进入身份比较，不读取字符。
        }
        let root = match self.root.as_ref() {
            Some(root) => root.upgrade().ok_or("原session基线所属账本已结束")?,
            None => Rc::clone(&self.state), // 根本身就是这一session最初的账本。
        };
        let Some(baseline) = root.preexisting_literal.as_ref() else {
            return Ok(false); // Default/editor本地账本无基线，不补建或回采其他已出现的业务值。
        };
        if !baseline.eligible_after_pool {
            return Ok(false); // 构造归还后已nil的原Weak不能因后继同值或地址复用恢复资格。
        }
        let actual = baseline.original.load();
        if !current() {
            drop(actual);
            return Err("加载预存原Weak时光栅票已撤销".into());
        }
        let same = actual
            .as_deref()
            .is_some_and(|original| std::ptr::eq::<NSString>(original, string));
        drop(actual); // Weak临时claim的Native析构在无账本借用的局部栈内结束。
        if !current() {
            return Err("归还预存原Weak临时claim后光栅票已撤销".into());
        }
        if !same {
            return Ok(false); // 同文本但另一实际身份仍完整进入原owned/actual-dealloc门禁。
        }
        let count = baseline
            .observations
            .get()
            .checked_add(1)
            .ok_or("借用字面量观察计数已耗尽")?;
        baseline.observations.set(count); // 诊断是固定标量，不保留每次line对象或另建身份集合。
        if count <= 8 {
            eprintln!(
                "native_raster_surface event=borrowed_literal role={role} before_document_open=true original_weak_identity_match=true utf16_units=1 observations={count} owned=false formal=false"
            );
        }
        Ok(true)
    }

    /// 为具体后端分出Native局部交接清单；原映射不归此清单，所有新身份仍先经过同一Session预算。
    pub(super) fn for_editor(parent: &Self) -> Self {
        let root = parent.root.clone().unwrap_or_else(|| {
            // surface 本身就是 root；Weak 只共享身份登记，不延长 native/session 生命周期。
            Rc::downgrade(&parent.state)
        });
        Self {
            state: Rc::new(NativeMarkdownOwnedLedgerState {
                snapshot: RefCell::new(Some(NativeMarkdownOwnedSnapshot::default())),
                registering: Cell::new(None),
                mapping_registration_failed: Cell::new(false),
                #[cfg(feature = "native-raster-surface-probe")]
                preexisting_literal: None,
            }),
            root: Some(root),
            scope: NativeMarkdownOwnedScope::Editor,
        }
    }

    /// modal 给现有 editor/root backing 换一个明确构造 scope，不创建新 snapshot 或 quota。
    pub(super) fn modal_scope(&self) -> Self {
        Self {
            state: Rc::clone(&self.state),
            root: self.root.clone(),
            scope: NativeMarkdownOwnedScope::Modal,
        }
    }

    /// 弱升级/临时引用析构也可能交还原生控制权；借出账本后再登记，回调不跨 RefMut。
    pub(super) fn include(&self, role: &'static str, object: &AnyObject) -> Result<(), String> {
        if let Some(root) = &self.root {
            // 父 root 先接受身份；子登记失败时仍保留父 weak 证据，禁止预算回滚伪释放。
            let root = root.upgrade().ok_or("笔记会话资源已不可用")?;
            Self::include_in(&root, role, object, self.scope)?;
        }
        Self::include_in(&self.state, role, object, self.scope)
    }

    /// 文稿原映射进入Session总预算，但最终释放收据不混入具体后端的Native清单。
    pub(super) fn include_document_backing(
        &self,
        observer: Option<MarkdownOriginalBackingObserver>,
    ) -> Result<NativeMarkdownOwnedSnapshot, String> {
        let mut document = NativeMarkdownOwnedSnapshot::default();
        if let Some(observer) = observer {
            // 先登记真实Session归属；根拒绝时不发布缺失原映射事实的文稿根。
            self.include_original_backing("editor.original-backing", observer.clone())?;
            document.include_original_backing("editor.original-backing", observer, self.scope)?;
        } // 没有原映射时不伪造源文件；空收据只表示没有该类待归还资源，不推断正文为空。
        document.seal();
        Ok(document)
    }

    /// 原Doc构造后立即登记；与Native登记相同地先经过root，再写入明确所有者的局部收据。
    pub(super) fn include_original_backing(
        &self,
        role: &'static str,
        observer: MarkdownOriginalBackingObserver,
    ) -> Result<(), String> {
        let result = (|| {
            if let Some(root) = &self.root {
                let root = root.upgrade().ok_or("笔记会话资源已不可用")?;
                if let Err(error) =
                    Self::include_original_backing_in(&root, role, observer.clone(), self.scope)
                {
                    // root已拒绝或正在借出时，父子都不能把未登记的源映射视为不存在。
                    root.mapping_registration_failed.set(true);
                    return Err(error);
                }
            }
            Self::include_original_backing_in(&self.state, role, observer, self.scope)
        })();
        if result.is_err() {
            self.state.mapping_registration_failed.set(true);
        }
        result
    }

    /// 借出唯一snapshot后再做容量回收；其中Native弱升级绝不跨越RefCell借用。
    fn include_original_backing_in(
        state: &NativeMarkdownOwnedLedgerState,
        role: &'static str,
        observer: MarkdownOriginalBackingObserver,
        scope: NativeMarkdownOwnedScope,
    ) -> Result<(), String> {
        crate::foreground_owner::with_foreground_owner(
            &state.snapshot,
            &state.registering,
            |_| 1,
            |snapshot| snapshot.include_original_backing(role, observer, scope),
            || {},
        )
        .map_err(|_| "笔记源映射登记仍在处理中".to_owned())?
    }

    /// 每个 backing 的本地 owner 借出后才做 weak/native 操作，重入只能看到 Busy。
    fn include_in(
        state: &NativeMarkdownOwnedLedgerState,
        role: &'static str,
        object: &AnyObject,
        scope: NativeMarkdownOwnedScope,
    ) -> Result<(), String> {
        crate::foreground_owner::with_foreground_owner(
            &state.snapshot,
            &state.registering,
            |_| 1,
            |snapshot| {
                // snapshot 位于独占局部 owner，原生 weak 初始化/析构不跨越 RefCell 借用。
                snapshot.include(role, object, scope)
            },
            || {},
        )
        .map_err(|_| "笔记资源登记仍在处理中".to_owned())?
    }

    /// 配置字段/按钮时登记其实际 cell，不递归接管框架内部视图。
    /// 组合登记不是回滚事务；中途失败保留已登记的 weak，调用方必须中止未完整对象的发布并负责清理。
    pub(super) fn include_control(
        &self,
        role: &'static str,
        control: &NSControl,
    ) -> Result<(), String> {
        let cell = control.cell();
        // control 先取得独立角色；后续 cell 登记失败也不能抹去这项既有观察证据。
        self.include(role, control)?;
        if let Some(cell) = cell {
            // 项目配置的文本、动作可能由 cell 独立持有，必须独立进入账本。
            self.include(role, &cell)?;
        }
        Ok(())
    }

    /// 显式配置 popup/context 菜单后登记 menu 和所有实际 item。
    /// 任一 item 登记失败都保留之前的 weak 证据；调用方须清理未完整发布的 owner，不能使用部分菜单。
    pub(super) fn include_menu(&self, role: &'static str, menu: &NSMenu) -> Result<(), String> {
        let items = menu.itemArray();
        if items.count() > MAX_OWNED_NATIVE_IDENTITIES {
            // 输入数组超限不截断，调用者不得发布这个未完整登记的菜单。
            return Err("笔记菜单超过资源数量上限".into());
        }
        // 预算或重入错误可能发生在任意后续 item；逐项传播失败，不撤销已登记角色或截断后继续发布。
        self.include(role, menu)?;
        for index in 0..items.count() {
            // 先取得当前 item，再短暂借用账本；getter 不与 RefMut 重叠。
            let item = items.objectAtIndex(index);
            self.include(role, &item)?;
        }
        Ok(())
    }

    /// 快照只复制 weak，不能延长任何项目原生对象的生命周期。
    pub(super) fn snapshot(&self) -> NativeMarkdownOwnedSnapshot {
        if let Some(root) = &self.root {
            let Some(root) = root.upgrade() else {
                // 没有共同预算owner时不能再签一个看似完整的新局部收据。
                return Self::incomplete_snapshot();
            };
            if root.registering.get().is_some() {
                // 父登记已开始而子记录尚未提交，局部空snapshot也不具备完整清单权限。
                return Self::incomplete_snapshot();
            }
            if root.mapping_registration_failed.get() {
                return Self::incomplete_snapshot(); // 共同预算中曾缺失源映射，子清单不能另签完整。
            }
        }
        Self::snapshot_in(&self.state)
    }

    /// 不完整状态是显式拒绝，绝不以零角色伪装资源已经释放。
    fn incomplete_snapshot() -> NativeMarkdownOwnedSnapshot {
        NativeMarkdownOwnedSnapshot {
            incomplete: true,
            ..NativeMarkdownOwnedSnapshot::default()
        }
    }

    /// 空工作区或保留父 editor 的操作只取得 modal 角色，不把活父控件纳入释放等待。
    pub(super) fn snapshot_modal(&self) -> NativeMarkdownOwnedSnapshot {
        let mut snapshot = self.snapshot();
        snapshot.identities.retain(|entry| {
            // scope 来自显式构造，不根据类名、可见性或时间推断可释放对象。
            entry.scope == NativeMarkdownOwnedScope::Modal
        });
        snapshot.original_backings.retain(|entry| {
            // modal完成时父文档合法存活；仅筛选显式构造scope，不豁免任何Editor关闭收据。
            entry.scope == NativeMarkdownOwnedScope::Modal
        });
        snapshot.seal();
        snapshot
    }

    /// clone weak 也可能进入 runtime；整个复制发生在局部 owner 而非 RefCell borrow 中。
    fn snapshot_in(state: &NativeMarkdownOwnedLedgerState) -> NativeMarkdownOwnedSnapshot {
        if state.mapping_registration_failed.get() {
            return Self::incomplete_snapshot(); // 失败路径保留永久负面证据，不能回退为空账本。
        }
        crate::foreground_owner::with_foreground_owner(
            &state.snapshot,
            &state.registering,
            |_| 1,
            |snapshot| {
                // 这里只复制有限 weak/scalar；调用方拿不到任何强 native owner。
                Ok::<_, ()>(snapshot.clone())
            },
            || {},
        )
        .ok()
        .and_then(Result::ok)
        .unwrap_or_else(|| {
            // 登记/复制期间的重入必须得到不完整收据，不能冒签完整空账本。
            Self::incomplete_snapshot()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实私有源映射供资源测试使用；不创建NSApplication、原生控件或用户目录数据。
    fn mapped_document() -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
        let mut file = tempfile::NamedTempFile::new().expect("synthetic original source");
        file.write_all("源映射🙂\n".as_bytes())
            .expect("write source");
        let document = MarkdownDocumentBuffer::open(file.path(), 4096).expect("open source");
        (file, document)
    }

    #[test]
    fn source_alias_keeps_actual_mapping_unreleased_after_document_drop() {
        // 文档析构不是源映射最终所有者：Snapshot仍合法读取同一不可变inode。
        let (_file, document) = mapped_document();
        let alias = document.text_snapshot();
        let observer = document.original_backing_observer().expect("mapped source");
        let root = NativeMarkdownOwnedLedger::default();
        let editor = NativeMarkdownOwnedLedger::for_editor(&root);
        let local = root
            .include_document_backing(Some(observer.clone()))
            .expect("register source");
        let whole = root.snapshot();
        // 文稿合法存活不阻止无载荷的后端结束；最终文稿/Session仍必须等待同一mmap归还。
        assert!(editor.snapshot().verify_released().is_ok());
        assert_eq!(local.original_backings.len(), 1);
        assert_eq!(whole.original_backings.len(), 1);
        assert!(local.verify_released().is_err());
        drop(document);
        assert!(observer.mapped_bytes() > 0);
        assert!(!observer.unmap_succeeded());
        assert!(local.verify_released().is_err());
        assert!(whole.verify_released().is_err());

        // 原最后Snapshot归还后，观察者本身不保活映射；系统munmap真实成功才允许收据通过。
        drop(alias);
        assert_eq!(observer.mapped_bytes(), 0);
        assert!(observer.unmap_succeeded());
        assert!(!observer.failed());
        assert!(local.verify_released().is_ok());
        assert!(whole.verify_released().is_ok());
    }

    #[test]
    fn final_document_receipt_waits_for_published_edit_file_alias() {
        let mut file = tempfile::NamedTempFile::new().expect("独占合成源文件");
        file.write_all("源映射🙂\n".as_bytes())
            .expect("写入合成源文件");
        // 此试次准入一次大输入，原4096字节的小资源夹具不能承载其真实保存后长度。
        let mut document =
            MarkdownDocumentBuffer::open(file.path(), 262_144).expect("打开合成长编辑源");
        let root = NativeMarkdownOwnedLedger::default();
        let mut receipt = root
            .include_document_backing(document.original_backing_observer())
            .expect("登记实际原文件");
        // 模拟一次超过小段阈值的明确输入；不是读取或复制已有整篇文稿。
        let input = "x".repeat(65_537);
        document
            .replace_utf16_range_with_selection(
                0..1,
                &input,
                MarkdownTextSelection::caret(0),
                MarkdownTextSelection::caret(65_537),
            )
            .expect("发布真实文件编辑段");
        drop(input);
        let alias = document.text_snapshot();
        // 最终销毁同样从原Doc实际段表取得观察票；无Native调用或事后可失败的业务登记。
        receipt.capture_document_backings(document.backing_observers());
        assert_eq!(receipt.original_backings.len(), 2);
        assert!(receipt.verify_released().is_err());
        drop(document);
        // 同一已发布文件仍由真实快照持有；原Doc销毁不能洗成映射已成功归还。
        assert!(receipt.verify_released().is_err());
        assert!(receipt.original_backings.iter().any(|entry| {
            entry.observer.mapped_bytes() > 0 && !entry.observer.unmap_succeeded()
        }));
        drop(alias);
        assert!(receipt.verify_released().is_ok());
        assert!(receipt.original_backings.iter().all(|entry| {
            entry.observer.mapped_bytes() == 0
                && entry.observer.unmap_succeeded()
                && !entry.observer.failed()
        }));
    }

    #[test]
    fn modal_snapshot_does_not_require_live_parent_source_to_unmap() {
        // scope是原构造事实：关闭对话框可保留父Doc，但父Editor收据仍严格等待同一源归还。
        let (_file, document) = mapped_document();
        let observer = document.original_backing_observer().expect("mapped source");
        let root = NativeMarkdownOwnedLedger::default();
        let editor = NativeMarkdownOwnedLedger::for_editor(&root);
        editor
            .include_original_backing("editor.original-backing", observer.clone())
            .expect("register editor source");
        let modal_ledger = editor.modal_scope();
        let modal = modal_ledger.snapshot_modal();
        assert!(modal.original_backings.is_empty());
        assert!(modal.verify_released().is_ok());
        assert!(root.snapshot().verify_released().is_err());
        assert!(editor.snapshot().verify_released().is_err());
        assert!(!observer.unmap_succeeded());
        let (_modal_file, modal_document) = mapped_document();
        modal_ledger
            .include_original_backing(
                "modal.synthetic-source",
                modal_document.original_backing_observer().unwrap(),
            )
            .expect("register modal source");
        let modal = modal_ledger.snapshot_modal();
        assert_eq!(modal.original_backings.len(), 1);
        assert!(modal.verify_released().is_err());
        drop(modal_document);
        assert!(modal.verify_released().is_ok()); // 父源仍活；仅本modal真实构造的映射已完成munmap。
        assert!(editor.snapshot().verify_released().is_err());
        drop(document);
        assert!(root.snapshot().verify_released().is_ok());
        assert!(editor.snapshot().verify_released().is_ok());
    }

    #[test]
    fn mapping_budget_cannot_discard_live_or_failed_registration_evidence() {
        // 每次显式登记都计入原session总预算；相同observer不能凭复制绕开容量失败。
        let (_file, document) = mapped_document();
        let observer = document.original_backing_observer().expect("mapped source");
        let ledger = NativeMarkdownOwnedLedger::default();
        for _ in 0..MAX_OWNED_NATIVE_IDENTITIES {
            ledger
                .include_original_backing("editor.original-backing", observer.clone())
                .expect("within shared budget");
        }
        assert!(
            ledger
                .include_original_backing("editor.original-backing", observer.clone())
                .is_err()
        );
        assert_eq!(
            ledger
                .state
                .snapshot
                .borrow()
                .as_ref()
                .unwrap()
                .original_backings
                .len(),
            MAX_OWNED_NATIVE_IDENTITIES
        );
        drop(document);
        assert!(observer.unmap_succeeded());
        assert!(ledger.snapshot().verify_released().is_err()); // 缺失过构造记录不能因后继释放恢复完整权限。
    }

    #[test]
    fn borrowed_root_mapping_registration_leaves_parent_and_child_incomplete() {
        // 模拟原生登记重入：父账本已借出时不能漏登记映射，再把空child冒充为释放成功。
        let (_file, document) = mapped_document();
        let observer = document.original_backing_observer().expect("mapped source");
        let root = NativeMarkdownOwnedLedger::default();
        let editor = NativeMarkdownOwnedLedger::for_editor(&root);
        let result = crate::foreground_owner::with_foreground_owner(
            &root.state.snapshot,
            &root.state.registering,
            |_| 1,
            |_| editor.include_document_backing(Some(observer.clone())),
            || {},
        );
        assert!(matches!(result, Ok(Err(_))));
        drop(document);
        assert!(observer.unmap_succeeded());
        assert!(root.snapshot().verify_released().is_err());
        assert!(editor.snapshot().verify_released().is_err());
        assert!(editor.snapshot_modal().verify_released().is_err()); // 失败构造不可按scope筛掉其不完整事实。
    }

    #[test]
    fn borrowed_registration_never_looks_like_a_complete_empty_ledger() {
        // 只操作 Rust 空账本，不构造原生对象；重入必须得到不完整收据而不是假 weak-nil 成功。
        let ledger = NativeMarkdownOwnedLedger::default();
        let result = crate::foreground_owner::with_foreground_owner(
            &ledger.state.snapshot,
            &ledger.state.registering,
            |_| 1,
            |_| {
                assert!(ledger.snapshot().verify_released().is_err());
                Err::<(), _>("synthetic registration failure")
            },
            || {},
        );
        assert_eq!(result, Ok(Err("synthetic registration failure")));
        // 普通错误仍恢复原账本，不丢记录或留下永久 registering 状态。
        assert_eq!(ledger.state.registering.get(), None);
        assert!(ledger.snapshot().verify_released().is_ok());
    }

    #[test]
    fn modal_scope_reuses_editor_backing_and_the_single_session_root() {
        // 纯Rust关联证明modal不new另一份1024预算，且嵌套child仍归同一root。
        let root = NativeMarkdownOwnedLedger::default();
        let editor = NativeMarkdownOwnedLedger::for_editor(&root);
        let modal = editor.modal_scope();
        let later_editor = NativeMarkdownOwnedLedger::for_editor(&modal);
        assert!(Rc::ptr_eq(&editor.state, &modal.state));
        assert!(!Rc::ptr_eq(&root.state, &editor.state));
        assert!(Rc::ptr_eq(
            &root.state,
            &modal.root.as_ref().unwrap().upgrade().unwrap()
        ));
        assert!(Rc::ptr_eq(
            &root.state,
            &later_editor.root.as_ref().unwrap().upgrade().unwrap()
        ));
    }

    #[test]
    fn root_registration_loan_prevents_an_empty_child_or_modal_receipt() {
        // 父清单正在本地登记时，child尚无新条目不代表“零资源”；必须沿root拒绝。
        let root = NativeMarkdownOwnedLedger::default();
        let editor = NativeMarkdownOwnedLedger::for_editor(&root);
        let modal = editor.modal_scope();
        let result = crate::foreground_owner::with_foreground_owner(
            &root.state.snapshot,
            &root.state.registering,
            |_| 1,
            |_| {
                assert!(editor.snapshot().verify_released().is_err());
                assert!(modal.snapshot_modal().verify_released().is_err());
                Ok::<_, ()>(())
            },
            || {},
        );
        assert_eq!(result, Ok(Ok(())));
        assert!(editor.snapshot().verify_released().is_ok());
    }

    #[test]
    fn modal_only_snapshot_excludes_parent_controls_without_classification() {
        // 只装入nil weak元数据，不构造AppKit；scope必须来自显式构造而非对象类名。
        let root = NativeMarkdownOwnedLedger::default();
        root.state
            .snapshot
            .borrow_mut()
            .as_mut()
            .unwrap()
            .identities = vec![
            OwnedNativeIdentity {
                role: "window.root",
                object: Weak::default(),
                scope: NativeMarkdownOwnedScope::Surface,
            },
            OwnedNativeIdentity {
                role: "modal.name-input",
                object: Weak::default(),
                scope: NativeMarkdownOwnedScope::Modal,
            },
        ];
        let modal = root.snapshot_modal();
        assert_eq!(modal.identities.len(), 1);
        assert_eq!(modal.identities[0].role, "modal.name-input");
        assert_eq!(root.snapshot().identities.len(), 2);
    }
}
