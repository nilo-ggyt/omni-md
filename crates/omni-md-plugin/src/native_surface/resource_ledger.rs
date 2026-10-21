//! Bounded weak evidence for explicitly constructed Native roles. Shared field
//! editors and framework subtrees do not become project-owned by reachability.

use super::*;
use omni_markdown_core::MarkdownOriginalBackingObserver;
use std::collections::HashMap;

/// One budget covers explicitly registered identities across scopes and still
/// live old objects. It does not count every Foundation object in the process.
const MAX_OWNED_NATIVE_IDENTITIES: usize = 1_024;

#[cfg(feature = "native-close-stage-diagnostic")]
/// Plain facts from one close observation. Addresses identify diagnostic output
/// only; they are never dereferenced later or accepted as release authority.
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
/// Construction assigns scope explicitly. It selects a receipt subset without
/// exempting any Native class from actual release.
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
/// Preserve the original mapping owner's actual VM facts. A dead document Weak
/// cannot conceal a mapping still retained by a snapshot or another alias.
struct OwnedOriginalBacking {
    role: &'static str,
    observer: MarkdownOriginalBackingObserver,
    scope: NativeMarkdownOwnedScope,
}

impl OwnedOriginalBacking {
    /// Zero length or destructor progress alone is insufficient. Prior read or
    /// reclaim failure permanently prevents a successful release receipt.
    fn released(&self) -> bool {
        self.observer.unmap_succeeded()
            && self.observer.mapped_bytes() == 0
            && !self.observer.failed()
    }
}

#[derive(Clone, Default)]
/// Own only Weak references and atomic metadata. Standard controls must actually
/// become nil; source mappings require a successful real munmap.
pub(super) struct NativeMarkdownOwnedSnapshot {
    identities: Vec<OwnedNativeIdentity>,
    original_backings: Vec<OwnedOriginalBacking>,
    addresses: HashMap<usize, usize>,
    incomplete: bool,
}

impl NativeMarkdownOwnedSnapshot {
    /// Summarize the existing Weak set for opt-in close diagnostics. Counts are
    /// observations, not release receipts or another registration graph.
    #[cfg(feature = "native-close-stage-diagnostic")]
    pub(super) fn diagnostic_alive_roles(&self) -> (bool, Vec<(&'static str, usize, usize)>) {
        let mut roles = std::collections::BTreeMap::<&'static str, (usize, usize)>::new();
        for entry in &self.identities {
            // The original set is bounded. Temporary Weak upgrades end in the
            // caller's local pool, with no ledger borrow across Native work.
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

    /// Observe only the two originally registered sidebar roles. The caller
    /// consumes these in one local pool; returned text contains scalar facts,
    /// without traversing framework subtrees or retaining a second identity set.
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
                    // Original construction role and actual type both identify
                    // the table; public getters inspect relations/counts, not row text.
                    // SAFETY: This Weak upgrade retains the main-thread table,
                    // without a mutable TLS or ledger borrow across these getters.
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
        drop(table); // Do not carry this temporary Native claim into another role or stage.
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
        drop(column); // Getter values and Weak upgrades belong only to this observation.
        format!("table=[{table_facts}] column=[{column_facts}]")
    }

    /// Observe selected existing Weak entries in the initial close pool. This
    /// adds no observer and leaves strict nil release requirements unchanged.
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
                continue; // Construction registered each role; class names cannot expand ownership.
            }
            let Some(object) = entry.object.load() else {
                continue; // A nil Weak invents no identity; diagnostics never replace original release verification.
            };
            if identities.len() == 8 {
                truncated = true;
                continue; // Record truncation without inspecting additional live objects' classes or lengths.
            }
            let name = object.class().name().to_string_lossy();
            let mut end = name.len().min(128);
            while !name.is_char_boundary(end) {
                end -= 1; // Bound UTF-8 class metadata without reading object text or descriptions.
            }
            let class = name[..end].to_owned();
            let length = if entry.role == "editor.raster.line-string" {
                object.downcast_ref::<NSString>().map(NSString::length)
            } else {
                None // Layers receive no string getter; even known string roles require actual type validation.
            };
            identities.push(NativeMarkdownCloseRasterIdentity {
                role: entry.role,
                address: std::ptr::from_ref(&*object).addr(),
                class,
                length,
            }); // Move scalar facts and short Rust strings; temporary Native claims end with this iteration.
        }
        (identities, truncated)
    }

    /// Register only an object the caller actually constructed or configured;
    /// reachability and class names do not establish ownership.
    fn include(
        &mut self,
        role: &'static str,
        object: &AnyObject,
        scope: NativeMarkdownOwnedScope,
    ) -> Result<(), String> {
        let address = std::ptr::from_ref(object).addr();
        if self.addresses.get(&address).is_some_and(|index| {
            // A live original Weak proves the same lifetime; address reuse must
            // create another record after the earlier object actually ended.
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

    /// Mappings and Native identities share one session budget. Reclaim only
    /// historical slots whose original resources have actually been released.
    fn make_room(&mut self) -> Result<(), String> {
        if self.identities.len() + self.original_backings.len() == MAX_OWNED_NATIVE_IDENTITIES {
            // Native entries require actual nil; mappings additionally require
            // no retained failure and a successful final munmap.
            self.identities
                .retain(|entry| entry.object.load().is_some());
            self.original_backings.retain(|entry| !entry.released());
            self.addresses.clear();
            for (index, entry) in self.identities.iter().enumerate() {
                if let Some(object) = entry.object.load() {
                    // Rebuild Native address lookup only; mapping observers
                    // contain no source address that can be reused as authority.
                    self.addresses
                        .insert(std::ptr::from_ref(&*object).addr(), index);
                }
            }
        }
        if self.identities.len() + self.original_backings.len() == MAX_OWNED_NATIVE_IDENTITIES {
            // Preserve permanent incompleteness after rejecting a resource.
            // Later cleanup cannot certify a snapshot that omitted construction.
            self.incomplete = true;
            return Err("笔记自有资源登记超过容量上限".into());
        }
        Ok(())
    }

    /// Explicitly register a mapping at construction. Observer clones share
    /// fixed atomics without copying or retaining the mapped payload.
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

    /// Once construction ends, release the address index and retain only bounded
    /// role/Weak evidence. Sealing does not claim those objects are already nil.
    pub(super) fn seal(&mut self) {
        self.addresses = HashMap::new();
    }

    /// Capture actual mappings while the final document is still live. Published
    /// edit-file pieces and history owe the same real release as the original file.
    pub(super) fn capture_document_backings(
        &mut self,
        observers: impl Iterator<Item = MarkdownOriginalBackingObserver>,
    ) {
        // This final receipt publishes no business state and performs no Native
        // work or new construction admission. Replace the initial-file set with
        // the document's complete actual backing set, including published edits.
        self.original_backings = observers
            .map(|observer| OwnedOriginalBacking {
                role: "editor.document-backing",
                observer,
                scope: NativeMarkdownOwnedScope::Editor,
            })
            .collect();
    }

    /// Attach actual destruction observation only to explicit standard objects.
    /// Independent type-specific proof owns any retained empty TextKit shell.
    pub(super) fn track_remaining(
        &self,
        cohort: &mut NativeMarkdownDeallocationCohort,
        mtm: MainThreadMarker,
    ) {
        for entry in &self.identities {
            if let Some(object) = entry.object.load() {
                // The marker retains no reverse object edge; the transient
                // strong upgrade ends with this iteration.
                cohort.track(&object, mtm);
            }
        }
    }

    /// Verify every original role. A nil parent cannot substitute for actual
    /// release of independently retained fields, cells, or menus.
    pub(super) fn verify_released(&self) -> Result<(), String> {
        if self.incomplete {
            // A reentrant snapshot taken during registration lacks complete
            // authority; an empty loaned slot is not a released resource graph.
            return Err("笔记自有资源快照不完整".into());
        }
        let roles: Vec<_> = self
            .identities
            .iter()
            .filter_map(|entry| {
                // Failure reports static roles only, without text, paths, or private class names.
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
            // Dead document/Native Weak references cannot certify that the last
            // shared snapshot actually returned its original mapping.
            return Err(format!("笔记源映射未完成真实释放：{mappings:?}"));
        }
        Ok(())
    }
}

#[cfg(feature = "native-raster-surface-probe")]
/// Original Weak identity of a fixed source literal observed before opening a
/// document, without a retained string, text payload, or process-wide cache.
struct NativeMarkdownPreexistingLiteral {
    original: Weak<NSString>,
    eligible_after_pool: bool,
    observations: Cell<usize>,
}

/// One backing owns the actual Weak set. Modal handles share it rather than
/// creating another independent identity budget.
struct NativeMarkdownOwnedLedgerState {
    snapshot: RefCell<Option<NativeMarkdownOwnedSnapshot>>,
    registering: Cell<Option<usize>>,
    // Retain root/local registration failure so later snapshots cannot succeed
    // after an original mapping was omitted.
    mapping_registration_failed: Cell<bool>,
    #[cfg(feature = "native-raster-surface-probe")]
    preexisting_literal: Option<NativeMarkdownPreexistingLiteral>,
}

/// The session root owns admission; a backend retains local Native receipts and
/// the outer document owns mapping receipts. Parent links remain Rust Weak and
/// do not prolong the foreground graph or canonical model.
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
    /// Call only at actual WindowSurface construction, before pager, document
    /// open, or text getters. Default deliberately grants no retroactive literal
    /// baseline to a test or probe that already opened a document.
    pub(super) fn for_native_session_before_document_open() -> Self {
        let mut ledger = Self::default();
        #[cfg(feature = "native-raster-surface-probe")]
        if super::raster_surface::NativeMarkdownRasterSurface::requested() {
            // Independently construct the fixed literal, without copying user
            // text, a surviving line, or an already observed Native value.
            let original = autoreleasepool(|_| {
                let literal = NSString::from_str("\n");
                Weak::from_retained(&literal)
            });
            // The original strong claim and local pool have ended. A nil Weak
            // permanently prevents borrowing eligibility for this session.
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
        let _ = &mut ledger; // Ordinary builds construct no Foundation baseline and perform no additional Native read.
        ledger
    }

    #[cfg(feature = "native-raster-surface-probe")]
    /// Recognize only a standard getter's exact preexisting identity as borrowed.
    /// Explicitly owned objects retain their original strict Weak contract.
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
            return Ok(false); // Compare identity only for the fixed LF's length, without reading characters.
        }
        let root = match self.root.as_ref() {
            Some(root) => root.upgrade().ok_or("原session基线所属账本已结束")?,
            None => Rc::clone(&self.state), // This root is the session's original ledger.
        };
        let Some(baseline) = root.preexisting_literal.as_ref() else {
            return Ok(false); // Local/default ledgers have no baseline and cannot recapture existing business values.
        };
        if !baseline.eligible_after_pool {
            return Ok(false); // Equal text or reused addresses cannot revive a baseline whose original Weak ended.
        }
        let actual = baseline.original.load();
        if !current() {
            drop(actual);
            return Err("加载预存原Weak时光栅票已撤销".into());
        }
        let same = actual
            .as_deref()
            .is_some_and(|original| std::ptr::eq::<NSString>(original, string));
        drop(actual); // End the temporary Native claim on this stack without a ledger borrow.
        if !current() {
            return Err("归还预存原Weak临时claim后光栅票已撤销".into());
        }
        if !same {
            return Ok(false); // Another identity with equal text retains all original owned/deallocation requirements.
        }
        let count = baseline
            .observations
            .get()
            .checked_add(1)
            .ok_or("借用字面量观察计数已耗尽")?;
        baseline.observations.set(count); // Fixed diagnostic state retains neither line objects nor another identity collection.
        if count <= 8 {
            eprintln!(
                "native_raster_surface event=borrowed_literal role={role} before_document_open=true original_weak_identity_match=true utf16_units=1 observations={count} owned=false formal=false"
            );
        }
        Ok(true)
    }

    /// Create a backend-local Native handoff set, separate from the outer
    /// document's mapping receipt. Every new identity still uses session admission.
    pub(super) fn for_editor(parent: &Self) -> Self {
        let root = parent.root.clone().unwrap_or_else(|| {
            // Surface itself is the root; Weak shares registration authority
            // without extending the Native/session lifetime.
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

    /// Give the existing editor/root backing an explicit modal construction scope,
    /// without creating another snapshot owner or quota.
    pub(super) fn modal_scope(&self) -> Self {
        Self {
            state: Rc::clone(&self.state),
            root: self.root.clone(),
            scope: NativeMarkdownOwnedScope::Modal,
        }
    }

    /// Weak upgrades and temporary destruction may reenter Native code. Lend the
    /// ledger before registration so callbacks never cross a RefMut.
    pub(super) fn include(&self, role: &'static str, object: &AnyObject) -> Result<(), String> {
        if let Some(root) = &self.root {
            // Admit at the root first. If local registration fails, preserve
            // the root's Weak evidence rather than rolling back actual construction.
            let root = root.upgrade().ok_or("笔记会话资源已不可用")?;
            Self::include_in(&root, role, object, self.scope)?;
        }
        Self::include_in(&self.state, role, object, self.scope)
    }

    /// Admit the original document mapping under the session budget while keeping
    /// its final release receipt separate from backend-local Native identities.
    pub(super) fn include_document_backing(
        &self,
        observer: Option<MarkdownOriginalBackingObserver>,
    ) -> Result<NativeMarkdownOwnedSnapshot, String> {
        let mut document = NativeMarkdownOwnedSnapshot::default();
        if let Some(observer) = observer {
            // Register actual session ownership first; root rejection must not
            // publish a document receipt missing its original mapping.
            self.include_original_backing("editor.original-backing", observer.clone())?;
            document.include_original_backing("editor.original-backing", observer, self.scope)?;
        } // No original mapping means no such release obligation, without inferring that document text is empty.
        document.seal();
        Ok(document)
    }

    /// Register immediately after actual document acquisition: root admission
    /// precedes writing the explicit owner's local receipt, as for Native objects.
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
                    // A rejected or loaned root leaves both parent and child
                    // unable to treat an unregistered source mapping as absent.
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

    /// Lend the unique snapshot before capacity reclaim, keeping Native Weak
    /// upgrades outside the RefCell borrow.
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

    /// Lend each backing's local owner before Weak/Native operations; reentry
    /// observes Busy rather than an alias to the mutable snapshot.
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
                // The snapshot is exclusively local, so Native Weak creation/
                // destruction executes without a RefCell borrow.
                snapshot.include(role, object, scope)
            },
            || {},
        )
        .map_err(|_| "笔记资源登记仍在处理中".to_owned())?
    }

    /// Register a configured control and its actual cell, without taking over
    /// framework subtrees. Partial registration retains existing Weak evidence;
    /// its caller must stop publication and clean up the incomplete owner.
    pub(super) fn include_control(
        &self,
        role: &'static str,
        control: &NSControl,
    ) -> Result<(), String> {
        let cell = control.cell();
        // Register the control first; later cell rejection cannot erase this
        // evidence of an already constructed original role.
        self.include(role, control)?;
        if let Some(cell) = cell {
            // Cells may independently retain configured text/actions and must
            // therefore enter the same actual identity ledger.
            self.include(role, &cell)?;
        }
        Ok(())
    }

    /// Register the configured popup/context menu and every actual item. A later
    /// item failure preserves prior Weak evidence; the caller must clean the
    /// incomplete owner instead of publishing a partially registered menu.
    pub(super) fn include_menu(&self, role: &'static str, menu: &NSMenu) -> Result<(), String> {
        let items = menu.itemArray();
        if items.count() > MAX_OWNED_NATIVE_IDENTITIES {
            // Reject the complete oversized array; truncation cannot authorize
            // publishing an incompletely registered menu.
            return Err("笔记菜单超过资源数量上限".into());
        }
        // Capacity or reentry can reject any later item. Propagate the failure
        // without erasing admitted roles or publishing a truncated menu.
        self.include(role, menu)?;
        for index in 0..items.count() {
            // Obtain the actual item before lending the ledger; its Native
            // getter never overlaps a mutable snapshot borrow.
            let item = items.objectAtIndex(index);
            self.include(role, &item)?;
        }
        Ok(())
    }

    /// Copy Weak evidence only, without extending project Native lifetimes.
    pub(super) fn snapshot(&self) -> NativeMarkdownOwnedSnapshot {
        if let Some(root) = &self.root {
            let Some(root) = root.upgrade() else {
                // Without the original shared admission owner, a local receipt
                // cannot certify a new complete graph.
                return Self::incomplete_snapshot();
            };
            if root.registering.get().is_some() {
                // Root registration may precede the child's record. Its local
                // empty snapshot therefore cannot certify completeness yet.
                return Self::incomplete_snapshot();
            }
            if root.mapping_registration_failed.get() {
                return Self::incomplete_snapshot(); // A missing shared mapping permanently prevents a complete child receipt.
            }
        }
        Self::snapshot_in(&self.state)
    }

    /// Represent incompleteness explicitly rather than certifying release from
    /// a fabricated zero-role set.
    fn incomplete_snapshot() -> NativeMarkdownOwnedSnapshot {
        NativeMarkdownOwnedSnapshot {
            incomplete: true,
            ..NativeMarkdownOwnedSnapshot::default()
        }
    }

    /// Operations that retain their parent editor acquire only modal roles,
    /// without waiting for legitimate live parent controls to deallocate.
    pub(super) fn snapshot_modal(&self) -> NativeMarkdownOwnedSnapshot {
        let mut snapshot = self.snapshot();
        snapshot.identities.retain(|entry| {
            // Explicit construction scope, not class name/visibility/timing,
            // determines which original controls belong to this modal receipt.
            entry.scope == NativeMarkdownOwnedScope::Modal
        });
        snapshot.original_backings.retain(|entry| {
            // The parent document may outlive a modal. This scope selection
            // does not exempt any mapping from the editor's final close receipt.
            entry.scope == NativeMarkdownOwnedScope::Modal
        });
        snapshot.seal();
        snapshot
    }

    /// Weak cloning may enter the runtime too. Perform the entire copy on the
    /// exclusively local owner, outside a RefCell borrow.
    fn snapshot_in(state: &NativeMarkdownOwnedLedgerState) -> NativeMarkdownOwnedSnapshot {
        if state.mapping_registration_failed.get() {
            return Self::incomplete_snapshot(); // Retain permanent failure evidence instead of falling back to an empty set.
        }
        crate::foreground_owner::with_foreground_owner(
            &state.snapshot,
            &state.registering,
            |_| 1,
            |snapshot| {
                // Copy bounded Weak/scalar evidence; the caller obtains no
                // strong Native owner through this operation.
                Ok::<_, ()>(snapshot.clone())
            },
            || {},
        )
        .ok()
        .and_then(Result::ok)
        .unwrap_or_else(|| {
            // Reentry during registration/copy receives incomplete evidence,
            // never an apparently complete empty graph.
            Self::incomplete_snapshot()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Acquire a real mapping in a private temporary file, without constructing
    /// AppKit objects or accessing the user's document directory.
    fn mapped_document() -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
        let mut file = tempfile::NamedTempFile::new().expect("synthetic original source");
        file.write_all("源映射🙂\n".as_bytes())
            .expect("write source");
        let document = MarkdownDocumentBuffer::open(file.path(), 4096).expect("open source");
        (file, document)
    }

    #[test]
    fn source_alias_keeps_actual_mapping_unreleased_after_document_drop() {
        // Document destruction need not release the mapping: an actual snapshot
        // can still legitimately own the same immutable source inode.
        let (_file, document) = mapped_document();
        let alias = document.text_snapshot();
        let observer = document.original_backing_observer().expect("mapped source");
        let root = NativeMarkdownOwnedLedger::default();
        let editor = NativeMarkdownOwnedLedger::for_editor(&root);
        let local = root
            .include_document_backing(Some(observer.clone()))
            .expect("register source");
        let whole = root.snapshot();
        // A live document does not prevent an empty backend from ending, while
        // document/session receipts still owe the same real mapping release.
        assert!(editor.snapshot().verify_released().is_ok());
        assert!(local.verify_released().is_err());
        drop(document);
        assert!(observer.mapped_bytes() > 0);
        assert!(!observer.unmap_succeeded());
        assert!(local.verify_released().is_err());
        assert!(whole.verify_released().is_err());

        // The observer does not retain the mapping after its final real snapshot
        // ends. Only the actual successful munmap permits release receipts.
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
        // Admit this explicit large input; the smaller mapping fixture's limit
        // would not accommodate the actual resulting canonical document.
        let mut document =
            MarkdownDocumentBuffer::open(file.path(), 262_144).expect("打开合成长编辑源");
        let root = NativeMarkdownOwnedLedger::default();
        let mut receipt = root
            .include_document_backing(document.original_backing_observer())
            .expect("登记实际原文件");
        // Submit an explicit new input exceeding the inline-piece threshold,
        // without reading or copying an existing whole document.
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
        // Final destruction observes the original document's actual backing set,
        // without Native work or a new fallible business registration after commit.
        receipt.capture_document_backings(document.backing_observers());
        assert!(receipt.verify_released().is_err());
        drop(document);
        // The real snapshot retains published file pieces. Document destruction
        // cannot be relabeled as successful final mapping release.
        assert!(receipt.verify_released().is_err());
        drop(alias);
        assert!(receipt.verify_released().is_ok());
    }

    #[test]
    fn modal_snapshot_does_not_require_live_parent_source_to_unmap() {
        // Construction scope permits closing a modal while its parent document
        // lives; the editor's final receipt still waits for that actual source.
        let (_file, document) = mapped_document();
        let observer = document.original_backing_observer().expect("mapped source");
        let root = NativeMarkdownOwnedLedger::default();
        let editor = NativeMarkdownOwnedLedger::for_editor(&root);
        editor
            .include_original_backing("editor.original-backing", observer.clone())
            .expect("register editor source");
        let modal_ledger = editor.modal_scope();
        let modal = modal_ledger.snapshot_modal();
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
        assert!(modal.verify_released().is_err());
        drop(modal_document);
        assert!(modal.verify_released().is_ok()); // Only this modal's actual mapping ended; its parent source remains live.
        assert!(editor.snapshot().verify_released().is_err());
        drop(document);
        assert!(root.snapshot().verify_released().is_ok());
        assert!(editor.snapshot().verify_released().is_ok());
    }
}
