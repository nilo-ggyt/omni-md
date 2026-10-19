//! 文稿模型只持唯一Core正文；旧TextKit适配图由实际编辑后端单独创建和释放。

use super::*;

/// 唯一canonical根；构造时不分配NSString、NSTextStorage或任何排版对象。
/// 丢弃后同步归还piece tree、增量编辑段和共享历史；快照alias另由原释放收据核对。
pub(crate) struct NativeMarkdownTextModel {
    pub(super) document: SharedMarkdownDocument,
}

impl NativeMarkdownTextModel {
    /// 先建立唯一文稿根；实际后端之后只借同一allocation，不可隐式创建第二正文。
    pub(crate) fn new(document: MarkdownDocumentBuffer) -> Self {
        Self {
            document: Rc::new(RefCell::new(document)),
        }
    }

    /// 给事务适配器借用同一handle，不在模型内保存Native后端。
    pub(crate) fn document(&self) -> &SharedMarkdownDocument {
        &self.document
    }
}

/// 当前笔记的唯一文稿根；具体后端先释放，模型与原映射收据最后结束。
/// 字段顺序也约束普通早退的析构顺序，不能让仍在断图的后端失去canonical正文。
pub(crate) struct NativeMarkdownEditorSurface {
    /// Find控件及其字段/共享编辑器弱收据属于原笔记会话，不归具体TextKit排版图。
    pub(super) find_bar: RefCell<Option<Rc<NativeMarkdownFindBar>>>,
    pub(super) find_resources: RefCell<NativeMarkdownFindResources>,
    pub(super) session_field_editor: RefCell<Option<Weak<NativeMarkdownSessionFieldEditor>>>,
    pub(super) backend: NativeMarkdownTextKitBackend,
    pub(super) model: NativeMarkdownTextModel,
    /// 只保原映射的原子事实；后端局部释放不等待仍归文稿所有的mmap。
    pub(super) document_resources: NativeMarkdownOwnedSnapshot,
}

impl NativeMarkdownEditorSurface {
    /// 先取得唯一正文及其最终释放收据，再让实际TextKit后端借用同一allocation。
    pub(crate) fn new_with_mode(
        document: MarkdownDocumentBuffer,
        frame: NSRect,
        presentation_mode: MarkdownPresentationMode,
        resource_root: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Self, String> {
        let model = NativeMarkdownTextModel::new(document);
        // 原映射不属于展示图；先登记Session总预算，并为最终文稿释放保留独立弱收据。
        // 登记失败时尚未构造TextKit，也不发布文稿根；局部模型随返回归还其原始资源。
        let original_backing = model.document().borrow().original_backing_observer();
        let document_resources = resource_root.include_document_backing(original_backing)?;
        // 后端失败同样不发布外层根；局部Native claims按原作用域归还，
        // 但归还项目claim不能直接充当框架实际dealloc或最终严格释放的证明。
        let backend = NativeMarkdownTextKitBackend::new_with_mode(
            model.document(),
            frame,
            presentation_mode,
            resource_root,
            mtm,
        )?;
        Ok(Self {
            find_bar: RefCell::new(None),
            find_resources: RefCell::new(NativeMarkdownFindResources::default()),
            session_field_editor: RefCell::new(None),
            backend,
            model,
            document_resources,
        })
    }

    /// 保存、历史和有界查询直接访问唯一模型，不经过具体展示后端。
    pub(crate) fn document(&self) -> &SharedMarkdownDocument {
        self.model.document()
    }

    /// 有限观察本笔记的实际Find字段；先结束栏owner借用，再调用可重入Native getter。
    pub(super) fn capture_find_resources(&self) -> Result<(), String> {
        let bar = self.find_bar.borrow().clone();
        let captured = NativeMarkdownFindResources::from_fields(
            bar.as_ref().map(|bar| bar.fields()).into_iter().flatten(),
        );
        self.find_resources.borrow_mut().merge(captured)
    }
}

impl Drop for NativeMarkdownEditorSurface {
    fn drop(&mut self) {
        // 普通早退也先封闭原输入；Find清空/拆挂可能重入，不能在最后保存之后追加正文。
        self.backend.stop_accepting_input();
        let bar = self.find_bar.take();
        if let Some(bar) = bar {
            // 本会话结束才清自有控件和callback；局部Rc在借用外归还，不等待下一篇笔记。
            bar.finish_accepted();
        }
        // 字段随后按声明顺序Drop：会话弱票、具体后端、最后同一canonical模型及映射收据。
    }
}

/// 具体TextKit图仅持Native与派生载荷；正文通过原storage的Weak关联访问。
/// 构造和释放均不创建文稿、重开文件或保存整篇Foundation镜像。
pub(crate) struct NativeMarkdownTextKitBackend {
    // 仅保留原子释放事实，不保像素、Native 对象、正文或 mmap；供旧 editor barrier 核真实归还。
    #[cfg(feature = "native-raster-surface-probe")]
    pub(super) raster_release: Option<super::raster_surface::NativeMarkdownRasterReleaseReceipt>,
    pub(super) scroll_view: Retained<NSScrollView>,
    pub(super) owned_ledger: Rc<NativeMarkdownOwnedLedger>,
    pub(super) gutter: Retained<NativeMarkdownGutter>,
    pub(super) text_view: Retained<PieceBackedMarkdownTextView>,
    pub(super) text_content_storage: Retained<objc2_app_kit::NSTextContentStorage>,
    // 具体后端显式持有完整manager/container网络；临时AppKit挂接回调不接管这些强owner，
    // 项目claim归还与框架实际dealloc信号仍是两个独立事件。
    pub(super) text_layout_manager: Retained<NSTextLayoutManager>,
    pub(super) text_container: Retained<NSTextContainer>,
    // 只有这个TextKit后端持真实storage；新分页后端构造纯模型时没有这条隐含强边。
    pub(super) text_storage: Retained<PieceBackedMarkdownTextStorage>,
}

impl NativeMarkdownTextKitBackend {
    /// 按原依赖顺序构造完整TextKit图；只借外层文稿，失败由局部Native owner归还资源。
    fn new_with_mode(
        document: &SharedMarkdownDocument,
        frame: NSRect,
        presentation_mode: MarkdownPresentationMode,
        resource_root: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Self, String> {
        // 具体后端只登记其Native局部收据；身份先进入Session总预算，原映射另归外层文稿。
        let owned_ledger = Rc::new(NativeMarkdownOwnedLedger::for_editor(resource_root));
        // 弱适配器明确归旧后端；构造纯模型、PagedInput或新视图均不会触发TextKit。
        let text_storage = PieceBackedMarkdownTextStorage::new(Rc::clone(document), mtm);
        text_storage.set_presentation_mode(presentation_mode);
        // 在交给NSTextView前先构造完整TextKit 2图，后端只借同一canonical文稿。
        // macOS 27实测先让view创建默认图、再替换storage会破坏编辑/排版记账，
        // 单字符编辑也可能报警并保留旧storage，因此这里始终先装配原图再创建view。
        #[cfg(feature = "native-markdown-runtime-probe")]
        let text_content_storage =
            super::content_storage_probe::NativeMarkdownContentStorageProbe::new_if_requested(
                &text_storage,
                &owned_ledger,
                mtm,
            )?;
        #[cfg(not(feature = "native-markdown-runtime-probe"))]
        let text_content_storage = objc2_app_kit::NSTextContentStorage::new();
        let text_layout_manager = NSTextLayoutManager::new();
        let text_container = NSTextContainer::initWithSize(
            NSTextContainer::alloc(),
            NSSize::new(frame.size.width, f64::MAX),
        );
        // 不安装项目fragment factory：标准系统fragment负责正文排版与生命周期。
        // 先建立primary manager和container归属，再安装canonical弱适配器。
        // 这些已取得的非空Native构造结果直接用于view初始化，不在之后查询或替建另一图。
        text_content_storage.addTextLayoutManager(&text_layout_manager);
        text_content_storage.setPrimaryTextLayoutManager(Some(&text_layout_manager));
        text_layout_manager.setTextContainer(Some(&text_container));
        let text_storage_observer =
            ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*text_content_storage);
        text_storage_observer.setTextStorage(Some(&text_storage));
        #[cfg(feature = "native-markdown-runtime-probe")]
        super::content_storage_probe::install_fragment_range_probe(
            &text_content_storage,
            &text_layout_manager,
            mtm,
        )?; // 仅明确合成实验在首次 NSTextView 初始化前接入；默认不分配或改变 delegate。
        // 指定initializer接收已经挂接的container；初始化、编辑和后续拆图沿用同一storage，
        // 具体后端不通过view默认构造另建排版网络。
        let text_view = PieceBackedMarkdownTextView::new(
            &text_storage,
            &text_container,
            &text_layout_manager,
            &owned_ledger,
            mtm,
        );
        text_storage.bind_text_view_once(&text_view)?; // 仅原composition root绑定单个Weak，清理不依赖子视图遍历。

        let scroll_view = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll_view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        scroll_view.setHasVerticalScroller(true);
        scroll_view.setHasHorizontalScroller(false);
        // 可见滚动区域承担不透明底色；正文视图在同一底色上绘制原生文字和局部装饰。
        scroll_view.setDrawsBackground(true);
        scroll_view.setBackgroundColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.075, 0.084, 0.11, 1.0,
        ));
        #[cfg(feature = "native-raster-surface-probe")]
        let raster_release = if super::raster_surface::NativeMarkdownRasterSurface::requested() {
            // 在首次挂接滚动正文和原生排版之前注册 factory，不能靠事后失效替换已缓存片段。
            // 唯一 owner 先进入原 view 槽；后续同步回调仍沿同一文档与生命周期。
            let raster = super::raster_surface::NativeMarkdownRasterSurface::install(
                &text_view,
                &text_storage,
                &text_layout_manager,
                &scroll_view,
                &owned_ledger,
                mtm,
            )?;
            let receipt = raster.release_receipt();
            *text_view.ivars().raster_surface.borrow_mut() = Some(raster);
            Some(receipt)
        } else {
            None // 普通诊断与正式路径均不创建光栅状态或第二释放收据。
        };
        let document_frame = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
        configure_markdown_text_view(&text_view, document_frame);
        text_view.setDelegate(Some(ProtocolObject::from_ref(&*text_view)));
        // SAFETY: 当前局部Retained持有主线程view，成功后由具体后端接管；
        // content storage的delegate关系为弱引用，不增加反向强owner。
        unsafe { text_content_storage.setDelegate(Some(ProtocolObject::from_ref(&*text_view))) };

        scroll_view.setDocumentView(Some(&text_view));
        // 具体后端和scroll持有gutter；它回链正文的关系为Weak，视口元数据保持有限。
        let gutter = NativeMarkdownGutter::new(&scroll_view, &text_view, &owned_ledger, mtm)?;
        let _ = text_view.ivars().gutter.set(Weak::from_retained(&gutter));
        scroll_view.setVerticalRulerView(Some(&gutter));
        scroll_view.setHasVerticalRuler(true);
        scroll_view.setRulersVisible(presentation_mode == MarkdownPresentationMode::Source);
        text_view.schedule_visible_table_layout_update();

        owned_ledger.include("editor.scroll", &scroll_view)?;
        #[cfg(feature = "native-block-fragment-diagnostic")]
        super::preview_blocks::diagnostic::install(&text_view, &text_content_storage);
        Ok(Self {
            #[cfg(feature = "native-raster-surface-probe")]
            raster_release,
            scroll_view,
            owned_ledger,
            gutter,
            text_view,
            text_content_storage,
            text_layout_manager,
            text_container,
            text_storage,
        })
    }

    /// 只借本后端真实持有的scroll供挂载或释放观察，不移交独立生命周期权限。
    pub(crate) fn scroll_view(&self) -> &NSScrollView {
        &self.scroll_view
    }

    /// 窗口只借NSView完成原焦点交接；挂载层不取得TextKit专有的文本或布局能力。
    pub(crate) fn input_view(&self) -> &NSView {
        self.text_view.as_super().as_super().as_super()
    }

    /// TextKit对照验收借其真实文本协议；普通窗口焦点交接使用上面的NSView接口。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(crate) fn text_view(&self) -> &NSTextView {
        &self.text_view
    }

    /// 只给旧TextKit observer借用这个实际后端的storage，不在canonical模型内保存适配器。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(crate) fn text_storage(&self) -> &NSTextStorage {
        &self.text_storage
    }

    /// 先封闭具体后端的输入及派生任务，禁止拆图中迟到回调追加未保存编辑。
    /// 本步骤不销毁外层模型、清除Core历史或单独证明Native资源已经释放。
    pub(super) fn stop_accepting_input(&self) {
        let accepted = self.text_storage.ivars().accepts_edits.replace(false);
        self.text_view.ivars().textkit_graph_teardown.set(true);
        self.text_view.cancel_pending_mode_viewport();
        self.text_view.ivars().folds.seal();
        // 撤销权限后立即take/drop有限背景的全部容量，迟到draw不能重新发布。
        self.text_view.release_preview_blocks();
        #[cfg(feature = "native-raster-surface-probe")]
        {
            let raster = self.text_view.ivars().raster_surface.borrow_mut().take();
            if let Some(raster) = raster {
                // 先移出唯一槽再断观察/图层关系；空槽和 sealed 标量阻止重入重新创建正文载荷。
                if let Err(error) = raster.shutdown() {
                    eprintln!(
                        "native_raster_surface event=shutdown_failed error={error:?} formal=false"
                    );
                }
            }
        }
        if !accepted {
            // 重复封闭仍完成上面的派生载荷清理；只有首次撤权继续清原生选区和编辑策略，
            // 避免拆图重入再次执行这些Native setter。
            return;
        }
        // 仍有有效 container/manager 且尚可选择时清 superclass caret，再撤销原生编辑策略。
        self.text_view.clear_native_selection_before_release();
        // 这些setter可触发同步回调；原输入和图权限已经封闭，不能向仍存活的模型提交新编辑。
        self.text_view.setEditable(false);
        self.text_view.setSelectable(false);
    }

    /// 只改变派生源码/预览展示，保留原始Markdown字节和真实选区。
    pub(crate) fn set_presentation_mode(
        &self,
        mode: MarkdownPresentationMode,
        controller: &Retained<NativeMarkdownController>,
    ) -> Result<(), String> {
        let _payload_use = PayloadUseGuard::enter(&self.text_view.ivars().payload_use_depth);
        // 新模式动作先撤旧Find/模式目标；相同模式不重新发布定位或清理已有选择。
        if self
            .text_view
            .authorized_mode_action(controller, &self.text_storage, mode)
            .is_none()
        {
            self.text_view.cancel_pending_find_reveal();
        }
        if self.text_storage.presentation_mode() == mode {
            return Ok(());
        }
        let transition = self
            .text_view
            .capture_mode_viewport_transition(controller, &self.text_storage, mode)
            .map_err(|_| "无法取得当前可见源位置，展示模式未切换".to_owned())?;
        let preflight = (|| {
            transition
                .native_step(&self.text_view, || {
                    self.text_view.commit_composition_before_release()
                })
                .map_err(|_| "展示模式切换期间笔记或输入已变更".to_owned())??;
            // 活cell只在其真实observed receipt上推进原票；不在提交后的当前gen重新捕获目标。
            transition
                .native_step(&self.text_view, || {
                    self.text_view
                        .commit_active_table_cell_with_mode(&transition)
                })
                .map_err(|_| "表格提交期间展示模式动作已被新的输入撤销".to_owned())??;
            transition
                .native_step(&self.text_view, || {
                    self.text_view.invalidate_visible_table_layouts(false)
                })
                .map_err(|_| "展示模式切换期间笔记或输入已变更".to_owned())??;
            transition
                .native_step(&self.text_view, || self.text_view.unfold_source())
                .map_err(|_| "展示模式切换已被新的输入撤销".to_owned())?;
            Ok::<(), String>(())
        })();
        if let Err(error) = preflight {
            transition.cancel(&self.text_view);
            return Err(error); // 属性尚未改变，窗口原有错误分支可保持旧popup和原始草稿。
        }
        // 模式切换先丢旧背景容量，Source往返不能复用同代次旧矩形。
        self.text_view.release_preview_blocks();
        if !transition.begin_change(&self.text_view) {
            transition.cancel(&self.text_view);
            return Err("展示模式切换已被新的输入撤销".into());
        }
        #[cfg(feature = "native-block-fragment-diagnostic")]
        self.text_view.begin_mode_attribute_diagnostic();
        let applied = transition.native_step(&self.text_view, || {
            self.text_storage.set_presentation_mode(mode)
        });
        #[cfg(feature = "native-block-fragment-diagnostic")]
        self.text_storage
            .ivars()
            .mode_attribute_diagnostic
            .report(0);
        if self.text_storage.presentation_mode() != mode {
            transition.cancel(&self.text_view);
            return Err("展示模式切换在属性发布前已被撤销".into());
        }
        // 属性写入后仍完成同owner的ruler补偿；输入撤权只取消旧viewport后果，不能单独回退popup。
        if !transition.application_current(&self.text_view) {
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 模式已写入但原owner/doc已撤权，不能以退出时slot为空解释成定位成功。
            eprintln!("native_mode_completion_failure stage=after_attributes_authority");
            transition.cancel(&self.text_view);
            return Ok(()); // 已关闭/换owner，不再访问Native或把旧模式错误写回后继窗口。
        }
        self.scroll_view
            .setRulersVisible(mode == MarkdownPresentationMode::Source);
        if !transition.application_current(&self.text_view) {
            #[cfg(feature = "native-markdown-runtime-probe")]
            // ruler调用可重入关闭/交接，必要补偿之后仍须保留原动作撤销的真实结果。
            eprintln!("native_mode_completion_failure stage=after_ruler_authority");
            transition.cancel(&self.text_view);
            return Ok(());
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let Err(error) = &applied {
            // 只报告已发生的属性调用失败；短路语义不变，不因诊断额外执行finish_change。
            eprintln!("native_mode_completion_failure stage=attributes error={error:?}");
        }
        // 属性与ruler已完成后，以原Changing票据粗定位一次；失败不进入Ready或重领后继输入权限。
        // 最终screenY仍由原有有界executor恢复/认证，不能把粗定位返回当作完成收据。
        if applied.is_err()
            || transition
                .coarse_reveal_before_ready(&self.text_view)
                .is_err()
            || transition.finish_change(&self.text_view).is_err()
        {
            transition.fail(&self.text_view);
            return Ok(()); // 实际模式已改变；仅定位失败，不让popup反映不存在的旧模式。
        }
        self.text_view.schedule_visible_table_layout_update();
        Ok(())
    }

    /// Inserts one small source table with valid block spacing through the canonical text-input path.
    pub(crate) fn insert_default_markdown_table(&self) {
        self.text_view.insert_default_markdown_table();
    }
}

impl Drop for NativeMarkdownTextKitBackend {
    fn drop(&mut self) {
        // 唯一外层EditorSurface已封闭原输入并结束自身Find；这里只对称拆具体TextKit图。
        #[cfg(feature = "native-block-fragment-diagnostic")]
        super::preview_blocks::diagnostic::uninstall(&self.text_view, &self.text_content_storage);
        // 先take/drop折叠与装饰容量，再清gutter并断开ruler关系，派生视口载荷不跨后端续命。
        let folds = self.text_view.ivars().folds.replace(Vec::new());
        drop(folds);
        let decorations = self
            .text_view
            .ivars()
            .source_decorations
            .replace(Vec::new());
        drop(decorations);
        self.text_view.ivars().source_decorations_dirty.set(false);
        #[cfg(feature = "native-input-trace")]
        self.text_view.release_input_trace();
        self.gutter.clear();
        self.scroll_view.setRulersVisible(false);
        self.scroll_view.setVerticalRulerView(None);
        // 断TextKit前取消原可见表格排版及待激活状态；排队工作只持Weak，
        // 配合已封闭的图权限，不能在拆图后重建旧编辑会话。
        self.text_view
            .ivars()
            .table_layout_update_scheduled
            .set(false);
        self.text_view.ivars().table_layout_rebuilding.set(false);
        self.text_view.invalidate_visible_table_layout_cache();
        self.text_view
            .ivars()
            .pending_table_cell_activation
            .set(None);
        self.text_view.ivars().active_table_cell.borrow_mut().take();
        let cell_editor = self.text_view.ivars().cell_editor.borrow_mut().take();
        if let Some(editor) = cell_editor {
            // 只清理已经懒建的真实字段；没有字段时不为清理创建字段或共享编辑器。
            editor.abortEditing();
            editor.setHidden(true);
            editor.setStringValue(ns_string!(""));
            // 隐藏字段仍属于旧后端，必须在最后owner归还前断delegate、target/action及挂载关系。
            // SAFETY: 局部Retained和后端的主线程text view在断开关系时仍持有此真实字段。
            unsafe {
                editor.setDelegate(None);
                editor.setTarget(None);
                editor.setAction(None);
            }
            editor.removeFromSuperview();
        }
        // 组合输入必须已由上层保存/responder预检提交；Drop不再调用unmarkText，
        // 否则可能在最后一次保存后追加正文，导致未保存数据随模型一起丢弃。
        // 模型仍存活时完成Foundation属性收尾；本适配器的fixAttributesInRange:为有限空操作，
        // 传递真实长度不生成整篇Foundation镜像。
        // 外层始终先释放本后端、最后释放模型；仅在本句临时升级原storage的Weak，
        // 取得长度后即归还临时Rc，不把正文保留到后续Native属性收尾调用中。
        let document_length = self
            .text_storage
            .document()
            .expect("TextKit backend teardown precedes its canonical model")
            .borrow()
            .utf16_len();
        self.text_storage
            .ensureAttributesAreFixedInRange(NSRange::new(0, document_length));
        // 对称注销NSTextView初始化时登记的drag类型，结束延迟系统事务对旧view的关联。
        self.text_view.unregisterDraggedTypes();
        // Native owner逐步归还期间，旧responder继续拒绝用户编辑。
        self.text_view.setEditable(false);
        self.text_view.setSelectable(false);
        // 先结束Find对旧responder的关系，再移除documentView和delegate；
        // drag注册已在上一阶段对称注销，不借系统drag回调伪造拆图步骤。
        self.text_view.setUsesFindBar(false);
        self.scroll_view.setDocumentView(None);
        self.text_view.setDelegate(None);
        // TextKit 2可能暴露兼容manager/storage关系；先清delegate、viewport回链和layout queue，
        // 再归还强owner，避免延迟排版重新挂接旧会话。
        // SAFETY: 具体后端此时仍持有这两个主线程对象，先结束回调关系再释放其owner。
        unsafe {
            self.text_content_storage.setDelegate(None);
            self.text_layout_manager.setLayoutQueue(None);
        }
        self.text_layout_manager.setDelegate(None);
        // 即使AppKit临时从manager拆除了container，具体后端仍持有并清理这一个实际container。
        self.text_container.setTextView(None);
        let text_viewport_controller = self.text_layout_manager.textViewportLayoutController();
        text_viewport_controller.setDelegate(None);
        // 此处不查询NSTextView.layoutManager，该兼容getter可能在拆TextKit 2时新建或切入TextKit 1。
        // 已存在的兼容manager随后只通过原storage枚举对称拆除。

        let text_storage_observer =
            ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*self.text_content_storage);
        let legacy_layout_manager_count = self.text_storage.layoutManagers().count();
        native_markdown_trace! {
            eprintln!(
                "[probe] drop graph before view_manager={} view_content={} storage={} manager={} container={} legacy_managers={}",
                self.text_view.textLayoutManager().is_some(),
                self.text_view.textContentStorage().is_some(),
                text_storage_observer.textStorage().is_some(),
                self.text_layout_manager.textContentManager().is_some(),
                self.text_layout_manager.textContainer().is_some(),
                legacy_layout_manager_count,
            );
        }
        // 仅处理view当前仍公开的manager/container关系；框架已拆除时不补建替代图。
        if let Some(text_layout_manager) = self.text_view.textLayoutManager()
            && let Some(text_container) = text_layout_manager.textContainer()
        {
            // manager强持有container；先清container的借用view回链，再解除布局目标归属。
            text_container.setTextView(None);
            native_markdown_trace! {
                eprintln!(
                    "[probe] drop after container view=nil manager={} content={}",
                    self.text_view.textLayoutManager().is_some(),
                    self.text_view.textContentStorage().is_some(),
                );
            }
            text_layout_manager.setTextContainer(None);
            native_markdown_trace! {
                eprintln!(
                    "[probe] drop after manager container=nil manager={} content={}",
                    self.text_view.textLayoutManager().is_some(),
                    self.text_view.textContentStorage().is_some(),
                );
            }
        }
        // 观察关系先与旧 backing store 断开，再用真实空 attributed string 清除 fallback。
        // 外层model此时仍持有唯一正文；本后端只有原storage的Weak关联，
        // 任何同步读取都必须遵守已封闭的客户端权限，断图不提前销毁该模型。
        text_storage_observer.setTextStorage(None);
        let empty = NSAttributedString::initWithString(NSAttributedString::alloc(), ns_string!(""));
        self.text_content_storage.setAttributedString(Some(&empty));
        // 已撤 delegate、queue 和 container，再解除 content manager 的布局消费者。
        self.text_content_storage.setPrimaryTextLayoutManager(None);
        self.text_content_storage
            .removeTextLayoutManager(&self.text_layout_manager);
        // 兼容 TextKit 1 同样先按 storage 的公开 owner 操作解除关系，不调用可能新建兼容图的 getter。
        let detached_legacy_layout_managers = self.text_storage.detach_legacy_layout_managers();
        native_markdown_trace! {
            eprintln!(
                "[probe] drop after content storage=nil manager={} content={} detached_legacy_managers={}",
                self.text_view.textLayoutManager().is_some(),
                self.text_view.textContentStorage().is_some(),
                detached_legacy_layout_managers,
            );
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] drop graph after view_manager={} view_content={} storage={} manager={} container={} managers={} primary={}",
                self.text_view.textLayoutManager().is_some(),
                self.text_view.textContentStorage().is_some(),
                text_storage_observer.textStorage().is_some(),
                self.text_layout_manager.textContentManager().is_some(),
                self.text_layout_manager.textContainer().is_some(),
                self.text_content_storage.textLayoutManagers().len(),
                self.text_content_storage
                    .primaryTextLayoutManager()
                    .is_some(),
            );
        }
        // 原生关系断开后 take/drop 全部存储 payload；框架持有空壳也无旧业务容量。
        self.text_storage.release_payload();
        self.text_view.release_payload();
        // SAFETY: 空字典是有效属性值；已封闭 view 不再将它发布到 canonical 正文。
        unsafe {
            self.text_view
                .setTypingAttributes(&MarkdownTextAttributes::new());
        }
        self.text_layout_manager.setTextSelections(&NSArray::new());
    }
}

/// Configures bounded plain-source editing, native input and accessibility without an independent undo/checking model.
pub(super) fn configure_markdown_text_view(text_view: &NSTextView, frame: NSRect) {
    // Scroll owns vertical overflow; fixed horizontal tracking wraps source without a document-sized native buffer.
    text_view.setFrame(frame);
    text_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    text_view.setMinSize(NSSize::new(0.0, frame.size.height));
    text_view.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
    text_view.setVerticallyResizable(true);
    text_view.setHorizontallyResizable(false);
    text_view.setTextContainerInset(NSSize::new(36.0, 32.0));
    // Native accessibility reads the same canonical virtual string as editing.
    text_view.setAccessibilityLabel(Some(ns_string!("Markdown 编辑器")));
    text_view.setAccessibilityHelp(Some(ns_string!(
        "编辑 Markdown。使用“文档”菜单设置格式、查找替换和管理文件。"
    )));
    text_view.setEditable(true);
    text_view.setSelectable(true);
    text_view.setRichText(false);
    text_view.setImportsGraphics(false);
    // Support plain-text drags plus explicit confined file imports; rich-text graphics
    // may not create a second mutable attributed document behind the piece tree.
    // SAFETY: AppKit exports immutable process-lifetime pasteboard type identifiers.
    let drag_types = unsafe {
        [
            objc2_app_kit::NSPasteboardTypeFileURL,
            NSPasteboardTypeString,
        ]
    };
    text_view.registerForDraggedTypes(&NSArray::from_slice(&drag_types));
    // 原生文字与局部装饰透出滚动容器底色，正文不另建背景图像或独立文字副本。
    text_view.setDrawsBackground(false);
    text_view.setBackgroundColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.075, 0.084, 0.11, 1.0,
    ));
    text_view.setInsertionPointColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.45, 0.66, 1.0, 1.0,
    )));
    // NSUndoManager would retain a duplicate history. Use only the Rust piece tree's
    // 128 structurally shared revisions and notify TextKit through exact responder actions.
    text_view.setAllowsUndo(false);
    // 项目自有 Find 栏提供完整输入与历史归属；不创建使用未知 editor/Undo 的标准 finder UI。
    text_view.setUsesFindBar(false);
    super::text_input_policy::configure_literal_text_input(text_view);
    // SAFETY: initUsingTextLayoutManager(true) creates the owned container; these public
    // layout constraints are changed only while its text view is alive.
    if let Some(text_container) = unsafe { text_view.textContainer() } {
        // A live container follows viewport width while allowing lazy vertical growth.
        text_container.setWidthTracksTextView(true);
        text_container.setHeightTracksTextView(false);
    }
    if let Some(text_layout_manager) = text_view.textLayoutManager() {
        // Retain TextKit's protection against suspicious native layout requests for giant/untrusted source.
        text_layout_manager.setLimitsLayoutForSuspiciousContents(true);
    }
}

#[cfg(test)]
mod canonical_model_tests {
    use std::io::Write;

    use super::{MarkdownDocumentBuffer, NativeMarkdownTextModel, Rc};

    #[test]
    fn canonical_model_constructs_and_releases_without_a_native_thread() {
        // 工作线程没有MainThreadMarker；纯文稿根不能在内部偷偷创建受主线程约束的Native对象。
        std::thread::spawn(|| {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            let chunk = [b'a'; 4_096];
            for _ in 0..64 {
                file.write_all(&chunk).unwrap();
            } // 固定栈缓冲创建原始文件，不为测试构造整篇String。
            let document = MarkdownDocumentBuffer::open(file.path(), 524_288).unwrap();
            let mapping = document.original_backing_observer().unwrap();
            let model = NativeMarkdownTextModel::new(document);
            let weak = Rc::downgrade(model.document());
            assert_eq!(model.document().borrow().utf16_len(), 262_144);
            assert_eq!(mapping.mapped_bytes(), 262_144);
            // 没有Native后端和冻结alias续命；丢弃唯一根就必须同步归还原映射。
            drop(model);
            assert!(weak.upgrade().is_none());
            assert_eq!(mapping.mapped_bytes(), 0);
            assert!(mapping.unmap_succeeded());
            assert!(!mapping.failed());
        })
        .join()
        .unwrap();
    }
}
