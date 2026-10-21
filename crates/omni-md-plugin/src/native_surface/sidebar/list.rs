//! 当前有限元数据页的原生列表；滚动与行复用不读取笔记正文，也不推进目录游标。

use super::*;
use objc2_app_kit::{
    NSTableColumn, NSTableView, NSTableViewColumnAutoresizingStyle, NSTableViewDataSource,
    NSTableViewDelegate, NSTableViewStyle, NSUserInterfaceItemIdentification,
};
use objc2_foundation::NSIndexSet;

/// 搜索副行只改变当前结果页的真实高度；普通目录和笔记沿原30pt行高。
const SEARCH_ROW_HEIGHT: f64 = 48.0;

struct NativeMarkdownSidebarSearchRowIvars {
    title: Retained<NSTextField>,
    subtitle: Retained<NSTextField>,
}

define_class!(
    /// 同一个复用行只持两项可见标签；文件身份仍只归原表的有限metadata页。
    // SAFETY: 主线程NSView子类只有明确的两个控件强边，不接管事件或文本输入协议。
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeMarkdownSidebarSearchRowIvars]
    struct NativeMarkdownSidebarSearchRow;

    // SAFETY: NSObjectProtocol没有新增方法；视图构造和清理均在原AppKit主线程。
    unsafe impl NSObjectProtocol for NativeMarkdownSidebarSearchRow {}
);

impl NativeMarkdownSidebarSearchRow {
    /// 构造发生在实际可见行请求内；离屏行由同一NSTableView复用，不建立摘要缓存。
    fn new(
        width: f64,
        ledger: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>, String> {
        let title = Self::label(
            13.0,
            NSRect::new(NSPoint::new(8.0, 27.0), NSSize::new(width - 16.0, 18.0)),
            mtm,
        );
        let subtitle = Self::label(
            11.0,
            NSRect::new(NSPoint::new(8.0, 7.0), NSSize::new(width - 16.0, 16.0)),
            mtm,
        );
        let allocated =
            Self::alloc(mtm).set_ivars(NativeMarkdownSidebarSearchRowIvars { title, subtitle });
        // SAFETY: 两个自有标签已初始化；原NSView指定初始化器只接收有限实际列宽与固定行高。
        let row: Retained<Self> = unsafe {
            msg_send![super(allocated), initWithFrame: NSRect::new(NSPoint::ZERO, NSSize::new(width, SEARCH_ROW_HEIGHT))]
        };
        row.setIdentifier(Some(ns_string!("OmniMarkdownSearchRow")));
        row.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        row.setClipsToBounds(true);
        row.addSubview(&row.ivars().title);
        row.addSubview(&row.ivars().subtitle);
        // 容器、两label和各自cell都沿原会话账本；登记失败时局部整行归还，不发布半行。
        ledger.include("sidebar.search-row", &row)?;
        ledger.include_control("sidebar.row", &row.ivars().title)?;
        ledger.include_control("sidebar.row", &row.ivars().subtitle)?;
        Ok(row)
    }

    /// 两个独立单行cell分别截断，宽度变化不让长标题挤掉摘要或扩张行高。
    fn label(size: f64, frame: NSRect, mtm: MainThreadMarker) -> Retained<NSTextField> {
        let label = NSTextField::labelWithString(ns_string!(""), mtm);
        label.setFrame(frame);
        label.setFont(Some(&NSFont::systemFontOfSize(size)));
        label.setEditable(false);
        label.setSelectable(false);
        label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        if let Some(cell) = label.cell() {
            cell.setUsesSingleLineMode(true);
            cell.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
        }
        label
    }
}

/// 一个会话只构造一对列表和滚动容器；翻页只替换当前页的纯元数据。
pub(in crate::native_surface) struct NativeMarkdownSidebarList {
    pub(in crate::native_surface) table: Retained<NativeMarkdownSidebarTable>,
    scroll: Retained<NSScrollView>,
}

/// dataSource回调只访问这一份有限投影，不反借前台surface或目录扫描器。
pub(in crate::native_surface) struct NativeMarkdownSidebarTableIvars {
    rows: RefCell<Vec<NativeMarkdownSidebarRow>>,
    active_path: RefCell<Option<String>>,
    /// 真实浏览目录独立于打开的正文；翻页、折叠和搜索不将它重置为正文父目录。
    selected_directory: RefCell<String>,
    selection_revision: Cell<u64>,
    revision: Cell<u64>,
    closed: Cell<bool>,
    ledger: std::rc::Weak<NativeMarkdownOwnedLedger>,
    /// 仅跟踪已创建行的弱身份，关闭时清空AppKit复用池中可能暂存的标签。
    labels: RefCell<Vec<Weak<NSTextField>>>,
}

define_class!(
    /// AppKit负责可见行、滚动、方向键与类型选择；项目只提供当前页内容与既有动作。
    // SAFETY: 主线程NSTableView子类使用公开dataSource/delegate及精确声明的selector签名。
    #[unsafe(super = NSTableView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeMarkdownSidebarTableIvars]
    pub(in crate::native_surface) struct NativeMarkdownSidebarTable;

    impl NativeMarkdownSidebarTable {
        #[unsafe(method(createMarkdownNoteInSelectedDirectory:))]
        fn create_note_in_selected_directory(&self, _sender: &AnyObject) {
            self.enqueue_directory_action(NativeMarkdownSidebarAction::CreateNote);
        }

        #[unsafe(method(createMarkdownDirectoryInSelectedDirectory:))]
        fn create_directory_in_selected_directory(&self, _sender: &AnyObject) {
            self.enqueue_directory_action(NativeMarkdownSidebarAction::CreateDirectory);
        }

        #[unsafe(method(importMarkdownNoteInSelectedDirectory:))]
        fn import_note_in_selected_directory(&self, _sender: &AnyObject) {
            self.enqueue_directory_action(NativeMarkdownSidebarAction::ImportNote);
        }

        #[unsafe(method(activateMarkdownSidebarSelection:))]
        fn activate_selection(&self, _sender: Option<&AnyObject>) {
            if let Ok(row) = usize::try_from(self.selectedRow()) {
                // 点击与原按钮相同，只排队一次原目录/笔记动作，不在native回调栈切换正文。
                self.enqueue_activation(row);
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if matches!(event.keyCode(), 36 | 76)
                && event.modifierFlags().intersection(NSEventModifierFlags::DeviceIndependentFlagsMask).is_empty()
            {
                // 列表获得焦点后，Return执行当前行；不是Host快捷键或新全局注册。
                if let Ok(row) = usize::try_from(self.selectedRow()) {
                    self.enqueue_activation(row);
                }
            } else {
                // 方向键、Tab和按名称选择由NSTableView保持原生行为，不重新解释文本编辑键。
                // SAFETY: 原始NSEvent同步转给同一NSTableView superclass实现。
                unsafe { let _: () = msg_send![super(self), keyDown: event]; }
            }
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, event: &NSEvent) -> Option<Retained<NSMenu>> {
            self.context_menu(event)
        }
    }

    // SAFETY: NSObjectProtocol没有额外方法；所有实例和消息都限制在AppKit主线程。
    unsafe impl NSObjectProtocol for NativeMarkdownSidebarTable {}
    // SAFETY: NSTableViewDelegate的公开父协议要求此conformance；不实现额外文本编辑动作。
    unsafe impl NSControlTextEditingDelegate for NativeMarkdownSidebarTable {}

    // SAFETY: dataSource同步返回当前有限页数量，不访问磁盘或借用外部UI所有者。
    unsafe impl NSTableViewDataSource for NativeMarkdownSidebarTable {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            self.ivars().rows.borrow().len() as isize
        }
    }

    // SAFETY: viewFor回调的NSView返回所有权符合协议；Native调用前已归还metadata借用。
    unsafe impl NSTableViewDelegate for NativeMarkdownSidebarTable {
        #[unsafe(method(tableView:heightOfRow:))]
        fn height_of_row(&self, _table: &NSTableView, row: isize) -> f64 {
            // 原表逐行读取当前页metadata；不按可见高度改变Core页容量或目录滚动行为。
            if usize::try_from(row).ok().and_then(|index| self.ivars().rows.borrow().get(index).map(|row| row.subtitle.is_some())).unwrap_or(false) {
                SEARCH_ROW_HEIGHT
            } else {
                sidebar_geometry::ROW_HEIGHT
            }
        }

        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn row_view(&self, table: &NSTableView, _column: Option<&NSTableColumn>, row: isize) -> Option<Retained<NSView>> {
            self.make_row_view(table, row)
        }

        #[unsafe(method_id(tableView:typeSelectStringForTableColumn:row:))]
        fn type_select_title(&self, _table: &NSTableView, _column: Option<&NSTableColumn>, row: isize) -> Option<Retained<NSString>> {
            self.type_select_string(row)
        }
    }
);

impl NativeMarkdownSidebarTable {
    /// 原生按名称选择只比较当前页的真实标题；不含显示缩进，也不触发文档打开。
    fn type_select_string(&self, row: isize) -> Option<Retained<NSString>> {
        if self.ivars().closed.get() {
            return None;
        }
        let index = usize::try_from(row).ok()?;
        let title = self.ivars().rows.borrow().get(index)?.title.clone();
        Some(NSString::from_str(&title))
    }

    /// 菜单构造在普通Rust返回值中处理关闭、失页和登记失败。
    fn context_menu(&self, event: &NSEvent) -> Option<Retained<NSMenu>> {
        if self.ivars().closed.get() {
            return None; // 已关闭的复用视图不再生成文件动作。
        }
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        let row = usize::try_from(self.rowAtPoint(point)).ok()?;
        let revision = self.ivars().revision.get();
        let kind = self.ivars().rows.borrow().get(row)?.kind;
        if !self.permits(revision, row) {
            return None; // 只接受当前真实命中行，不用最后一个点击行代替空白区域。
        }
        let ledger = self.ivars().ledger.upgrade()?;
        let target = NativeMarkdownSidebarMenuTarget::new(row, revision, self, self.mtm());
        let menu = native_markdown_sidebar_menu(&target, kind, self.mtm());
        if let Err(error) = ledger
            .include("sidebar.context.target", &target)
            .and_then(|()| ledger.include_menu("sidebar.context.menu", &menu))
        {
            present_native_markdown_error(&error);
            return None; // 未登记的目标和菜单只归本栈，失败时不交给AppKit跟踪。
        }
        Some(menu)
    }
    /// 仅当前可见行请求标签；普通Rust helper保留可失败的复用与资源登记。
    fn make_row_view(&self, table: &NSTableView, row: isize) -> Option<Retained<NSView>> {
        let index = usize::try_from(row).ok()?;
        let metadata = self.ivars().rows.borrow().get(index).cloned()?;
        let active =
            self.ivars().active_path.borrow().as_deref() == Some(metadata.relative_path.as_str());
        let revision = self.ivars().revision.get();
        if metadata.subtitle.is_some() {
            // 搜索行独立复用两标签；目录行仍走下方原单label构造/呈现。
            return self.make_search_row_view(table, metadata, active, revision);
        }
        // SAFETY: 标识符仅属于本表注册的NSTextField；没有nib或需要额外owner的工厂。
        let reused = unsafe {
            table.makeViewWithIdentifier_owner(ns_string!("OmniMarkdownSidebarRow"), None)
        };
        let field = match reused {
            Some(view) => view.downcast::<NSTextField>().ok()?,
            None => {
                let field = NSTextField::labelWithString(ns_string!(""), self.mtm());
                field.setIdentifier(Some(ns_string!("OmniMarkdownSidebarRow")));
                field.setFont(Some(&NSFont::systemFontOfSize(13.0)));
                field.setEditable(false);
                field.setSelectable(false);
                field.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
                if let Some(cell) = field.cell() {
                    // 列表宽度变化只影响尾部省略，文件相对路径和完整AX文本不被截短。
                    cell.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
                }
                let ledger = self.ivars().ledger.upgrade()?;
                if let Err(error) = ledger.include_control("sidebar.row", &field) {
                    present_native_markdown_error(&error);
                    return None; // 未进入原资源账的行不能发布为可见内容。
                }
                let mut labels = self.ivars().labels.take();
                labels.retain(|label| label.load().is_some());
                labels.push(Weak::from_retained(&field));
                *self.ivars().labels.borrow_mut() = labels;
                field
            }
        };
        if self.ivars().closed.get() || self.ivars().revision.get() != revision {
            return None; // 创建/复用期间若新页或关闭接管，不把旧文本写入后继行。
        }
        field.setStringValue(&NSString::from_str(&native_markdown_sidebar_button_title(
            &metadata,
        )));
        field.setTextColor(Some(&native_markdown_note_color(
            active && metadata.kind == NativeMarkdownSidebarEntryKind::Note,
        )));
        // 当前打开的笔记与原生表格选择是两件事；焦点转入正文后，字重仍标明实际文稿。
        // 复用行每次都重设普通/当前字重，避免旧笔记的强调样式串到下一页或目录行。
        let weight = if active && metadata.kind == NativeMarkdownSidebarEntryKind::Note {
            // SAFETY: 框架公开固定字重值进程内有效，样式不保留本行或业务对象。
            unsafe { NSFontWeightSemibold }
        } else {
            // SAFETY: 与当前笔记分支同一公开常量合同，复用行必须恢复普通字重。
            unsafe { NSFontWeightRegular }
        };
        field.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, weight)));
        field.setToolTip(Some(&NSString::from_str(&metadata.relative_path)));
        let kind = match metadata.kind {
            NativeMarkdownSidebarEntryKind::Root => "笔记库",
            NativeMarkdownSidebarEntryKind::Directory => {
                if metadata.is_collapsed {
                    "已折叠文件夹"
                } else {
                    "已展开文件夹"
                }
            }
            NativeMarkdownSidebarEntryKind::Note => {
                if active {
                    "当前笔记"
                } else {
                    "笔记"
                }
            }
        };
        field.setAccessibilityLabel(Some(&NSString::from_str(&format!(
            "{kind}：{}",
            metadata.title
        ))));
        Some(field.into_super().into_super())
    }

    /// 同一搜索结果的原title/路径/摘要分别呈现；原行身份、菜单与激活仍由table页票决定。
    fn make_search_row_view(
        &self,
        table: &NSTableView,
        metadata: NativeMarkdownSidebarRow,
        active: bool,
        revision: u64,
    ) -> Option<Retained<NSView>> {
        // SAFETY: 只有本类用此identifier创建搜索容器，不读取系统未知复用视图的子树。
        let reused = unsafe {
            table.makeViewWithIdentifier_owner(ns_string!("OmniMarkdownSearchRow"), None)
        };
        let row = match reused {
            Some(view) => view.downcast::<NativeMarkdownSidebarSearchRow>().ok()?,
            None => {
                let ledger = self.ivars().ledger.upgrade()?;
                let width = table.bounds().size.width;
                let row = match NativeMarkdownSidebarSearchRow::new(width, &ledger, self.mtm()) {
                    Ok(row) => row,
                    Err(error) => {
                        present_native_markdown_error(&error);
                        return None; // 原ledger失败不发布未完整登记的行。
                    }
                };
                // 与普通标签共用原弱清理列表；复用池即使暂存容器，也不能留下旧业务文本。
                let mut labels = self.ivars().labels.take();
                labels.retain(|label| label.load().is_some());
                labels.push(Weak::from_retained(&row.ivars().title));
                labels.push(Weak::from_retained(&row.ivars().subtitle));
                *self.ivars().labels.borrow_mut() = labels;
                row
            }
        };
        if self.ivars().closed.get() || self.ivars().revision.get() != revision {
            return None; // 构造/复用期间已换页时，不把旧搜索值写进后继显示。
        }
        let title = &row.ivars().title;
        let subtitle = &row.ivars().subtitle;
        let current = || !self.ivars().closed.get() && self.ivars().revision.get() == revision;
        // 每份Native临时值和setter前后均保原页票；重入换页后不再改后继复用字段。
        let native_title =
            native_markdown_table_intent_step(&current, || NSString::from_str(&metadata.title))
                .ok()?;
        native_markdown_table_intent_step(&current, || title.setStringValue(&native_title)).ok()?;
        let native_subtitle = native_markdown_table_intent_step(&current, || {
            NSString::from_str(metadata.subtitle.as_deref().expect("搜索行具有副行"))
        })
        .ok()?;
        native_markdown_table_intent_step(&current, || subtitle.setStringValue(&native_subtitle))
            .ok()?;
        let title_color =
            native_markdown_table_intent_step(&current, || native_markdown_note_color(active))
                .ok()?;
        native_markdown_table_intent_step(&current, || title.setTextColor(Some(&title_color)))
            .ok()?;
        let subtitle_color = native_markdown_table_intent_step(&current, || {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.60, 0.64, 0.71, 1.0)
        })
        .ok()?;
        native_markdown_table_intent_step(&current, || {
            subtitle.setTextColor(Some(&subtitle_color))
        })
        .ok()?;
        let description = native_markdown_table_intent_step(&current, || {
            NSString::from_str(&metadata.search_description())
        })
        .ok()?;
        native_markdown_table_intent_step(&current, || title.setToolTip(Some(&description)))
            .ok()?;
        native_markdown_table_intent_step(&current, || subtitle.setToolTip(Some(&description)))
            .ok()?;
        native_markdown_table_intent_step(&current, || {
            title.setAccessibilityLabel(Some(&description))
        })
        .ok()?;
        let subtitle_description = native_markdown_table_intent_step(&current, || {
            NSString::from_str(&format!(
                "路径与摘要：{}",
                metadata.subtitle.as_deref().expect("搜索行具有副行")
            ))
        })
        .ok()?;
        native_markdown_table_intent_step(&current, || {
            subtitle.setAccessibilityLabel(Some(&subtitle_description))
        })
        .ok()?;
        Some(row.into_super())
    }

    /// 只校验原列表/页/行身份，不将复用标签的地址当成业务文件身份。
    pub(in crate::native_surface) fn permits(&self, revision: u64, row: usize) -> bool {
        self.permits_origin(&NativeMarkdownSidebarActionOrigin::Row {
            row_index: row,
            revision,
        })
    }

    /// 只读捕获真实目录选择；没有原生getter，也不要求目录仍出现在当前有限页中。
    pub(in crate::native_surface) fn directory_origin(&self) -> NativeMarkdownSidebarActionOrigin {
        NativeMarkdownSidebarActionOrigin::SelectedDirectory {
            relative_path: self.ivars().selected_directory.borrow().clone(),
            selection_revision: self.ivars().selection_revision.get(),
        }
    }

    /// 一次明确目录选择或成功收据撤销旧按钮请求，即使路径随后又被选回也不能复活。
    fn select_directory(&self, path: &str) {
        if self.ivars().closed.get() {
            return; // 已拆图的目录不能被迟到安装/收据重新填入。
        }
        self.ivars().selection_revision.set(
            self.ivars()
                .selection_revision
                .get()
                .checked_add(1)
                .expect("原生目录选择代次不能耗尽"),
        );
        *self.ivars().selected_directory.borrow_mut() = path.to_owned();
    }

    /// 普通回调检查唯一前台；真正take surface的文件边界使用同一纯来源判断。
    pub(in crate::native_surface) fn permits_origin(
        &self,
        origin: &NativeMarkdownSidebarActionOrigin,
    ) -> bool {
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            active
                .as_ref()
                .is_some_and(|surface| self.origin_matches_surface(surface, origin))
        })
    }

    /// 原页是元数据身份，目录选择代次是用户意图；不借用全局Active所以可用于原mutation收口。
    pub(in crate::native_surface) fn origin_matches_surface(
        &self,
        surface: &NativeMarkdownWindowSurface,
        origin: &NativeMarkdownSidebarActionOrigin,
    ) -> bool {
        if self.ivars().closed.get() || !std::ptr::eq(&*surface.sidebar_list.table, self) {
            return false; // 同名目录和同号页不能跨会话继承权限。
        }
        match origin {
            // 原右键动作仍要求原页及实际行，不将目录浏览状态混入该行的权限。
            NativeMarkdownSidebarActionOrigin::Row {
                row_index,
                revision,
            } => {
                self.ivars().revision.get() == *revision
                    && *row_index < self.ivars().rows.borrow().len()
            }
            // 独立目录选择不属于某页；合法保存刷新/翻页可以继续原意图，换目录仍按原代次撤权。
            NativeMarkdownSidebarActionOrigin::SelectedDirectory {
                relative_path,
                selection_revision,
            } => {
                self.ivars().selection_revision.get() == *selection_revision
                    && *self.ivars().selected_directory.borrow() == *relative_path
            }
        }
    }

    /// 三个固定按钮在第一个原生操作前固定当前目录，随后复用原菜单的排队和文件流程。
    fn enqueue_directory_action(&self, action: NativeMarkdownSidebarAction) {
        let origin = self.directory_origin();
        if !self.permits_origin(&origin) {
            return; // 已关闭或暂借的原列表不能为后继会话创建文件。
        }
        enqueue_native_markdown_sidebar_action(Weak::from(self), origin, action, self.mtm());
    }

    fn enqueue_activation(&self, row: usize) {
        let revision = self.ivars().revision.get();
        if !self.permits(revision, row) {
            return; // 迟到事件不能打开已被新页替代的同号笔记。
        }
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("侧栏选择在AppKit主队列执行");
            if !weak
                .get(mtm)
                .load()
                .is_some_and(|table| table.permits(revision, row))
            {
                return; // 排队期间原页或前台已变化，不获取后继文件的动作权限。
            }
            if let Err(error) = activate_native_markdown_sidebar_row(row) {
                present_native_markdown_error(&error);
                eprintln!("Markdown sidebar activation failed: {error}");
            }
        });
    }

    /// 标签的复用池可能暂存离屏行；先复制弱句柄，再逐个清空，避免借用跨Native消息。
    fn clear_labels(&self) {
        let labels = self.ivars().labels.borrow().clone();
        for label in labels {
            if let Some(label) = label.load() {
                label.setStringValue(ns_string!(""));
                label.setToolTip(None);
                label.setAccessibilityLabel(None);
            }
        }
    }
}

impl NativeMarkdownSidebarList {
    pub(in crate::native_surface) fn new(
        rows: &[NativeMarkdownSidebarRow],
        selected_path: Option<&str>,
        content: &NSView,
        ledger: &Rc<NativeMarkdownOwnedLedger>,
        mtm: MainThreadMarker,
    ) -> Result<Self, String> {
        let frame = sidebar_geometry::list_frame(content.bounds().size.height);
        let table =
            NativeMarkdownSidebarTable::alloc(mtm).set_ivars(NativeMarkdownSidebarTableIvars {
                rows: RefCell::new(Vec::new()),
                active_path: RefCell::new(None),
                selected_directory: RefCell::new(
                    selected_path
                        .map(native_markdown_note_parent)
                        .unwrap_or_default(),
                ),
                selection_revision: Cell::new(0),
                revision: Cell::new(0),
                closed: Cell::new(false),
                ledger: Rc::downgrade(ledger),
                labels: RefCell::new(Vec::new()),
            });
        // SAFETY: 原table只初始化一次；所有dataSource调用发生在Rust ivars完整构造之后。
        let table: Retained<NativeMarkdownSidebarTable> = unsafe {
            msg_send![super(table), initWithFrame: NSRect::new(NSPoint::ZERO, frame.size)]
        };
        let column =
            NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), ns_string!("note"));
        column.setWidth(frame.size.width);
        table.addTableColumn(&column);
        table.setHeaderView(None);
        table.setCornerView(None);
        table.setAllowsColumnReordering(false);
        table.setAllowsColumnResizing(false);
        table.setColumnAutoresizingStyle(
            NSTableViewColumnAutoresizingStyle::LastColumnOnlyAutoresizingStyle,
        );
        table.setAllowsMultipleSelection(false);
        table.setAllowsEmptySelection(true);
        table.setAllowsColumnSelection(false);
        table.setAllowsTypeSelect(true);
        table.setStyle(NSTableViewStyle::Plain);
        table.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        table.setRowHeight(sidebar_geometry::ROW_HEIGHT);
        table.setIntercellSpacing(NSSize::new(0.0, 2.0));
        table.setBackgroundColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.06, 0.067, 0.086, 1.0,
        ));
        table.setAccessibilityLabel(Some(ns_string!("笔记列表")));
        // SAFETY: table由唯一会话保活，dataSource/delegate/target均为weak且关闭时显式断开。
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*table)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*table)));
            table.setTarget(Some(&*table));
            table.setAction(Some(sel!(activateMarkdownSidebarSelection:)));
        }
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        scroll.setAutoresizingMask(NSAutoresizingMaskOptions::ViewHeightSizable);
        scroll.setDocumentView(Some(&table));
        ledger.include("sidebar.scroll", &scroll)?;
        ledger.include("sidebar.clip", &scroll.contentView())?;
        ledger.include("sidebar.table", &table)?;
        ledger.include("sidebar.column", &column)?;
        let list = Self { table, scroll };
        list.replace(rows, selected_path)?;
        content.addSubview(&list.scroll);
        Ok(list)
    }

    /// 翻页只替换这13项投影；程序安装选择不会触发click action或另开文档。
    pub(in crate::native_surface) fn replace(
        &self,
        rows: &[NativeMarkdownSidebarRow],
        selected_path: Option<&str>,
    ) -> Result<(), String> {
        if self.table.ivars().closed.get() {
            return Err("笔记列表已经关闭".into());
        }
        if rows.len() > sidebar_geometry::MINIMUM_PAGE_ROWS {
            return Err("笔记列表超出当前页容量".into());
        }
        let revision = self
            .table
            .ivars()
            .revision
            .get()
            .checked_add(1)
            .ok_or("笔记列表已结束，请重新打开")?;
        self.table.ivars().revision.set(revision);
        *self.table.ivars().rows.borrow_mut() = rows.to_vec();
        self.table.clear_labels();
        if self.table.ivars().closed.get() || self.table.ivars().revision.get() != revision {
            return Err("笔记列表已经变化".into());
        }
        self.select_active(selected_path);
        if self.table.ivars().closed.get() || self.table.ivars().revision.get() != revision {
            return Err("笔记列表已经变化".into());
        }
        self.scroll.contentView().scrollToPoint(NSPoint::ZERO);
        self.scroll
            .reflectScrolledClipView(&self.scroll.contentView());
        Ok(())
    }

    /// 当前笔记只更新正文色标；原生选中行仍对应独立浏览目录，不被刷新偷换成正文项。
    pub(in crate::native_surface) fn select_active(&self, path: Option<&str>) {
        if self.table.ivars().closed.get() {
            return; // 文档安装回调迟到不能让已关闭列表重新加载旧标题。
        }
        let revision = self.table.ivars().revision.get();
        let selection_revision = self.table.ivars().selection_revision.get();
        *self.table.ivars().active_path.borrow_mut() = path.map(str::to_owned);
        let selected = {
            let directory = self.table.ivars().selected_directory.borrow();
            self.table.ivars().rows.borrow().iter().position(|row| {
                row.kind != NativeMarkdownSidebarEntryKind::Note && row.relative_path == *directory
            })
        };
        self.table.reloadData();
        if self.table.ivars().closed.get()
            || self.table.ivars().revision.get() != revision
            || self.table.ivars().selection_revision.get() != selection_revision
        {
            return; // reloadData可重入换页/选择/关闭，旧位置不能选中后继页的同号条目。
        }
        match selected {
            Some(index) => self.table.selectRowIndexes_byExtendingSelection(
                &NSIndexSet::indexSetWithIndex(index),
                false,
            ),
            None => {
                // SAFETY: nil sender只表示程序取消列表选择，不执行文件动作或编辑事务。
                unsafe {
                    self.table.deselectAll(None);
                }
            }
        }
    }

    pub(in crate::native_surface) fn clear(&self) -> Result<(), String> {
        self.replace(&[], None)
    }

    /// 明确点击目录或成功安装正文才更新实际浏览目的地；此处不发送Native消息。
    pub(in crate::native_surface) fn select_directory(&self, path: &str) {
        self.table.select_directory(path);
    }

    /// 只消费同一原mutation的成功收据；失败、取消和无关操作都不移动浏览目录。
    pub(in crate::native_surface) fn apply_directory_mutation(
        &self,
        mutation: &NativeMarkdownFileMutation,
        result: &NativeMarkdownFileMutationResult,
    ) {
        let directory = self.table.ivars().selected_directory.borrow().clone();
        if let Some(path) = native_markdown_directory_after_mutation(&directory, mutation, result) {
            self.select_directory(&path);
        }
    }

    /// 关闭先撤回调和metadata，再断实际容器关系；复用缓存不保文件名或菜单目标。
    pub(in crate::native_surface) fn detach(&self) {
        if self.table.ivars().closed.replace(true) {
            return; // 显式关闭和Rust Drop共享同一清理，不再向已拆图发送第二轮消息。
        }
        // 关闭归还整份有限页及路径容量；只clear会让被框架暂存的table继续拥有旧Vec缓冲。
        // take先结束槽借用再析构原值，后续Native拆图不跨任何metadata借用。
        drop(self.table.ivars().rows.take());
        drop(self.table.ivars().active_path.take());
        drop(self.table.ivars().selected_directory.take());
        // SAFETY: 原唯一table所有者在主线程断开全部weak协议/动作入口。
        unsafe {
            self.table.setAction(None);
            self.table.setTarget(None);
            self.table.setDelegate(None);
            self.table.setDataSource(None);
        }
        self.table.clear_labels();
        // 所有真实标签先清空文字、路径提示和AX，再归还弱身份数组及其容量。
        drop(self.table.ivars().labels.take());
        self.table.reloadData();
        // table拥有其列，不能把列的归还推迟到table本体析构。复制本次真实列集合，
        // 避免removeTableColumn修改原数组时仍遍历它；临时列强边只活到本轮移除结束。
        let columns = self.table.tableColumns().to_vec();
        for column in columns {
            self.table.removeTableColumn(&column);
        }
        self.scroll.setDocumentView(None);
        self.table.removeFromSuperview();
        self.scroll.removeFromSuperview();
    }
}

impl Drop for NativeMarkdownSidebarList {
    fn drop(&mut self) {
        self.detach(); // 构造或安装失败也归还同一真实图，不另建空列表替代。
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// 原Full验收只复用当前NSApplication/主线程与原账本，核真实列表组件；不冒称工作区端到端或整产品验收。
pub(in crate::native_surface) fn verify_search_rows(
    parent: &NSView,
    ledger: &Rc<NativeMarkdownOwnedLedger>,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    autoreleasepool(|_| {
        // 固定有限页走与真实搜索结果相同的唯一metadata映射，不读文件、不构造正文或新窗口。
        let rows: Vec<_> = (0..sidebar_geometry::MINIMUM_PAGE_ROWS)
            .map(|index| {
                NativeMarkdownSidebarRow::from_search_result(
                    omni_markdown_core::MarkdownSearchResult {
                        relative_path: format!("原目录{index:02}/同名.md"),
                        title: "中文长标题、原始空格与 emoji 😀".repeat(3),
                        snippet: Some("保留完整命中摘要，宽度改变时只按各自行尾省略。".repeat(5)),
                    },
                )
            })
            .collect();
        let list = NativeMarkdownSidebarList::new(
            &rows,
            Some(&rows[0].relative_path),
            parent,
            ledger,
            mtm,
        )?;
        for width in [224.0, 144.0, 224.0] {
            // 真实滚动容器/列宽变化使AppKit重排同一复用行，不直接伪造两个label的最终frame。
            let size = list.scroll.frame().size;
            list.scroll.setFrameSize(NSSize::new(width, size.height));
            list.scroll.tile();
            list.table.layoutSubtreeIfNeeded();
            for index in [0usize, 1] {
                let view = list
                    .table
                    .viewAtColumn_row_makeIfNecessary(0, index as isize, true)
                    .ok_or("搜索结果未生成真实行视图")?;
                let row = view
                    .downcast::<NativeMarkdownSidebarSearchRow>()
                    .map_err(|_| "搜索结果未使用两行容器")?;
                let frame = row.bounds();
                let title = &row.ivars().title;
                let subtitle = &row.ivars().subtitle;
                let title_frame = title.frame();
                let subtitle_frame = subtitle.frame();
                let contains = |rect: NSRect| {
                    rect.origin.x >= 0.0
                        && rect.origin.y >= 0.0
                        && rect.size.width > 0.0
                        && rect.size.height > 0.0
                        && rect.origin.x + rect.size.width <= frame.size.width
                        && rect.origin.y + rect.size.height <= frame.size.height
                };
                // SDK明确rectOfRow包含intercellSpacing，内容view不含；两者必须分别保持真实48pt与原间隔。
                if frame.size.height != SEARCH_ROW_HEIGHT
                    || list.table.rectOfRow(index as isize).size.height
                        != SEARCH_ROW_HEIGHT + list.table.intercellSpacing().height
                    || frame.size.width > list.scroll.contentView().bounds().size.width
                    || !contains(title_frame)
                    || !contains(subtitle_frame)
                    || NSIntersectsRect(title_frame, subtitle_frame)
                    || title_frame.size.width != frame.size.width - 16.0
                    || subtitle_frame.size.width != frame.size.width - 16.0
                {
                    return Err(format!(
                        "搜索两行的真实行高、窄列宽或标签边界不一致：requested_width={width} row={index} scroll={:?} clip={:?} table={:?} row_rect={:?} bounds={frame:?} title={title_frame:?} subtitle={subtitle_frame:?}",
                        list.scroll.frame(),
                        list.scroll.contentView().bounds(),
                        list.table.frame(),
                        list.table.rectOfRow(index as isize),
                    ));
                }
                for label in [title, subtitle] {
                    let cell = label.cell().ok_or("搜索标签没有实际文本cell")?;
                    if !cell.usesSingleLineMode()
                        || cell.lineBreakMode() != objc2_app_kit::NSLineBreakMode::ByTruncatingTail
                        || label.toolTip().is_none_or(|value| {
                            value.to_string() != rows[index].search_description()
                        })
                    {
                        return Err("搜索两行没有各自尾截断或完整Tooltip".into());
                    }
                }
                if title.stringValue().to_string() != rows[index].title
                    || subtitle.stringValue().to_string()
                        != rows[index].subtitle.as_deref().unwrap()
                    || title
                        .accessibilityLabel()
                        .is_none_or(|value| value.to_string() != rows[index].search_description())
                    || subtitle
                        .accessibilityLabel()
                        .is_none_or(|value| !value.to_string().contains(&rows[index].relative_path))
                {
                    return Err("搜索标题、原路径、完整摘要或无障碍身份被重写".into());
                }
            }
        }
        // 实际选中第二个重名项后仍对应第二条原路径；本组件不伪造Active会话或执行文件打开。
        list.table
            .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(1), false);
        let selected =
            usize::try_from(list.table.selectedRow()).map_err(|_| "搜索行无法实际选择")?;
        if list
            .table
            .ivars()
            .rows
            .borrow()
            .get(selected)
            .map(|row| row.relative_path.as_str())
            != Some(rows[1].relative_path.as_str())
        {
            return Err("重名搜索结果的真实选择丢失原相对路径".into());
        }
        let last = (rows.len() - 1) as isize;
        list.table.scrollRowToVisible(last);
        if !NSIntersectsRect(
            list.table.rectOfRow(last),
            list.scroll.contentView().documentVisibleRect(),
        ) {
            return Err("搜索末行不能通过真实滚动到达".into());
        }
        let previous_labels = list.table.ivars().labels.borrow().clone();
        let (_, ordinary) = native_markdown_sidebar_rows(
            MarkdownWorkspaceSnapshot {
                directories: Vec::new(),
                notes: Vec::new(),
            },
            "原目录",
        );
        list.replace(&ordinary, None)?;
        let ordinary_view = list
            .table
            .viewAtColumn_row_makeIfNecessary(0, 0, true)
            .ok_or("返回目录后没有实际原单行视图")?;
        if ordinary_view.bounds().size.height != sidebar_geometry::ROW_HEIGHT
            || list.table.rectOfRow(0).size.height
                != sidebar_geometry::ROW_HEIGHT + list.table.intercellSpacing().height
        {
            return Err("返回目录后仍使用搜索行高度".into());
        }
        for weak in previous_labels {
            if let Some(label) = weak.load()
                && (label.stringValue().length() != 0
                    || label.toolTip().is_some()
                    || label.accessibilityLabel().is_some())
            {
                return Err("换页后复用池仍持原搜索业务文本".into());
            }
        }
        list.detach();
        // 原Full关闭阶段继续严格核本ledger中的容器/label/cell真实nil；这里不清账或豁免控件。
        eprintln!(
            "native_sidebar_search event=two_line_component_pass widths=224,144,224 rows=13 original_path_selection=true scroll_last=true ordinary_height_restored=true old_labels_empty=true formal=false whole_product=false"
        );
        Ok(())
    })
}
