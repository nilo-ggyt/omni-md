//! 真实Host会话上的完整长文及Source专项流程；动作、终点和关闭都沿原产品会话运行。
//! 桥只保弱身份、固定样本与有限收据，不创建窗口、SDK会话或第二个主事件循环。

use super::*;

mod editing;
mod fixture;
use editing::HostedEditingProbe;
pub use fixture::{
    write_native_markdown_giant_paragraph_fixture, write_native_markdown_memory_fixture,
};

/// 一个Host激活只运行一种完整流程；专项沿原序列结束，不以前缀采样替代功能验收。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownHostedFlow {
    Full,
    SourceSession,
    SourceFolding,
    SourcePendingMode,
    SourceRelativeLink,
}

impl NativeMarkdownHostedFlow {
    /// 原静态动作表不分配或保业务资源；Host按原事件边界执行，不跳过真实验证终点。
    pub fn steps(self) -> &'static [NativeMarkdownHostedStep] {
        match self {
            Self::Full => FULL_STEPS,
            Self::SourceSession => SOURCE_SESSION_STEPS,
            Self::SourceFolding => SOURCE_FOLDING_STEPS,
            Self::SourcePendingMode => SOURCE_PENDING_STEPS,
            Self::SourceRelativeLink => SOURCE_RELATIVE_STEPS,
        }
    }
}

/// 具名步骤只对应既有真实操作；公开接口不能传任意整数建立未声明的输入或关闭权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownHostedStep {
    // 原401..421保留两次Source/Preview往返及三次真正Next；重复动作在静态序列中明确出现。
    /// 核原Host首次显示的完整CPU帧、既定磁盘身份和初始选择，不用挂载成功推定呈现。
    VerifyOpen,
    /// 点击原模式入口；后继验证必须消费同一次模式票和实际可见位置。
    SelectSource,
    /// 核Source模式、字段焦点、查询值及原选区，布局Pending不能冒充转换已完成。
    VerifySource,
    /// 在原会话显示实际Find栏，不创建替代输入控件或专用后端。
    ShowFind,
    /// 核栏的真实可见布局、原字段编辑器和主响应者归属。
    VerifyFindShown,
    /// 向真实查询字段输入固定第115行标记，输入前声明精确意图后态。
    EnterTableQuery,
    /// 核原字段完整值和原输入意图；不能事后捕获另一次查询补票。
    VerifyTableQuery,
    /// 沿原Next动作执行；同一动作保存真实Complete或Pending请求供后继观察。
    FindNext,
    /// 核独立预声明表格匹配、真实目标几何及当前完整CPU帧。
    VerifyTableFind,
    /// 切回同一Body的预览模式，不保旧Source图或另建排版正文。
    SelectPreview,
    /// 核原模式转换终点、可见源锚点与旧资源退休结果。
    VerifyPreview,
    /// 在同一Find字段输入长代码标记，不读取或复制整段代码。
    EnterCodeQuery,
    /// 核已验证查询与完整Input权限，保原实际字段路由。
    VerifyCodeQuery,
    /// 核第一处代码命中和当前Source帧，不把选区变化当作定位已经完成。
    VerifyCodeFind,
    /// 核预览中下一处代码命中及代码背景像素；原真实绘制门仍生效。
    VerifyCodeFindPreview,
    // 原425..438在编辑前核中文、非BMP、组合、滚动与隐藏像素，不能破坏它们的空历史前提。
    /// 空历史前态下开始原高亮用例，模式转换仍走正式按钮收据。
    HighlightSelectSource,
    /// 消费Source转换终点后采原未命中基线，不能在旧图取像素。
    HighlightSourceOrigin,
    /// 原字段有限中文输入改变查询，正文内容与历史必须保持不变。
    HighlightChineseInput,
    /// 比较同一实际中文字形的基线及匹配颜色，保未命中对照。
    VerifyHighlightChinese,
    /// 选择固定非BMP查询；不把一个代理项当成独立字符。
    HighlightNonBmpInput,
    /// 核完整非BMP匹配范围和实际高亮，不能按UTF16长度拆字形。
    VerifyHighlightNonBmp,
    /// 组合中的查询不应发布临时匹配；同时验证重复打开保留原marked客户。
    VerifyHighlightComposition,
    /// 真实滚动使旧几何失效，后继只能领取同一动作的新完整帧。
    HighlightScroll,
    /// 在新滚动位置建立实际对照，不能复用屏外旧像素。
    HighlightScrollOrigin,
    /// 核滚动后的原匹配与正文、查询和历史不变。
    VerifyHighlightScroll,
    /// 恢复原滚动再切预览；两个真实动作共享原用户意图后态。
    HighlightRestoreAndPreview,
    /// 消费预览终点后改变查询，仍使用该模式的真实字形和像素。
    HighlightPreviewInput,
    /// 核预览高亮后沿真实Hide入口隐藏，不直接改栏可见标志。
    VerifyHighlightPreviewAndHide,
    /// 核Hide之后字形与基线恢复，仅允许原栏实测高度造成的平移。
    VerifyHighlightHidden,
    // 原Unicode输入和三次历史转换最后恢复原字节；生产保存独立结算精确g+4。
    /// 从已验证的有限Find选区开始真实Unicode插入，不重设为固定caret。
    BeginEditing,
    /// 等本次输入的g+1完整帧，再核有限片段和唯一可撤销历史。
    VerifyInserted,
    /// 原正文公开命令只撤销一次已接纳输入，不直接改Core文档。
    UndoEditing,
    /// 核g+2恢复原完整revision、有向选择及真实重做可用性。
    VerifyUndo,
    /// 沿同一客户重做原输入，不能重新插入相同文字替代历史。
    RedoEditing,
    /// 核g+3恢复此前真实插入revision及准确Native选区。
    VerifyRedo,
    /// 再撤销一次恢复原文，保留同一历史链而非重载文件。
    RestoreEditing,
    /// 核g+4完整原字节与有向选择，内存恢复不能代替磁盘保存。
    VerifyRestored,
    /// 执行生产保存并绑定本次磁盘后态，不暂停自动保存或覆盖期望内容。
    SaveRestored,
    /// 流式比对真实保存文件与原canonical；完整通过后才结算旧只读收据。
    VerifySaved,
    // Source专项保Find字段、模式、退休图、输入及附件顺序，末项同栈请求返回。
    /// 在Host初始文稿建立原Find字段与固定查询，为Source接管原输入客户。
    SourceFind,
    /// 只在原查询引发的完整CPU帧后触发Source转换，拒绝旧Scene锚点。
    SourceSelectMode,
    /// 核转换后原查询、字段选择和焦点均保留。
    VerifySourceMode,
    /// 等同一原Doc的Source首帧，之后才能消费预览退休证据。
    VerifySourceFrame,
    /// 确认旧预览图真实退休，再执行原Next定位。
    SourceRetiredAndFind,
    /// 核匹配实际可见帧，不能把Find选区提交当作字体和绘图已完成。
    VerifySourceFoundFrame,
    /// 按原有限选择输入中文、emoji和组合字符，声明唯一g+1后态。
    SourceInsert,
    /// 等原输入后态的真实完整帧，拒绝中间选择帧。
    VerifySourceInsertedFrame,
    /// 核原片段、选择和根后再撤销，失败不能重放输入。
    SourceVerifyInsertAndUndo,
    /// 等原Undo的g+2帧，保持同一Doc和Source域。
    VerifySourceUndoneFrame,
    /// 核原revision已经恢复，再沿真实历史执行Redo。
    SourceVerifyUndoAndRedo,
    /// 等真实g+3帧，不以字符内容相同替代原历史后果。
    VerifySourceRedoneFrame,
    /// 核插入后果复现，再撤销一次准备恢复保存。
    SourceVerifyRedoAndRestore,
    /// 等原g+4完整帧，再允许比较文件和切换模式。
    VerifySourceRestoredFrame,
    /// 逐块核原文恢复并生产保存，更新的仅是已认证磁盘收据。
    SourceVerifyRestoreAndSave,
    /// 保原Find字段选择和源码位置切预览，原Source图必须退休。
    SourceSelectPreview,
    /// 等原预览模式票完成并核真实位置，不要求旧Source继续保活。
    VerifySourcePreview,
    /// 真实释放原Source之后再重新进入Source，防止保两张排版图。
    SourceRetiredAndReenter,
    /// 核再次接管原字段和已保存Doc，不能另建TextKit客户。
    VerifySourceReenteredMode,
    /// 消费重新进入Source的完整帧，作为后续附件输入的原权限。
    VerifySourceReenteredFrame,
    /// 原Word导航、附件复制和后续文字按实际队列顺序接纳。
    SourceAttachmentTyping,
    /// 核附件之后的输入精确后果，再撤销末次文字。
    SourceAttachmentUndoTyping,
    /// 核文字已撤销，再撤销真实附件链接，保已复制文件的明确生命周期。
    SourceAttachmentUndoLink,
    /// 核原前缀及文件仍在，再重做原链接提交。
    SourceAttachmentRedoLink,
    /// 核原链接与文字恢复结果，再沿同一历史撤销。
    SourceAttachmentUndoRedoneLink,
    /// 核附件链接再次撤销，再撤销最初前缀。
    SourceAttachmentUndoPrefix,
    /// 核原文恢复并保存，随后使用真实缺失源证明失败不提交链接。
    SourceAttachmentMissingSource,
    /// 核缺失源反馈后仍可正常输入及撤销，不把外部I/O失败封死正文。
    SourceAttachmentFailureEditable,
    /// 核最后恢复保存，排原待取消附件后同栈Return，严格关闭再核没有文件副作用。
    SourceAttachmentRestoreAndReturn,
    // 折叠沿真实菜单和gutter；每次字符及折叠代次仍由原场景核验，不从动作表造几何。
    /// 先观察预览退休，再沿原Source菜单折叠，不能直接改折叠私态。
    SourceFoldMenu,
    /// 核隐藏范围与真实gutter几何，再点击原gutter展开。
    SourceFoldHiddenAndExpand,
    /// 核展开帧后再折叠菜单，显式声明下一次fold revision。
    SourceFoldFrameAndMenu,
    /// 核真实隐藏范围后Find该范围，后继应由原Find自动展开。
    SourceFoldFindHidden,
    /// 核Find已展开与可见，再沿菜单全选，保完整canonical范围。
    SourceFoldFindAndSelectAll,
    /// 核菜单选区后展开全部，不依赖正文可见前缀猜全文。
    SourceFoldSelectAllAndExpand,
    /// 核展开帧后插原有限前缀，为后续隐藏编辑提供真实历史。
    SourceFoldExpandAndInsert,
    /// 核前缀唯一提交后再折叠，保Source和字符代次共同授权。
    SourceFoldPrefixAndFold,
    /// 在真实折叠标题末执行Word导航与输入，不把隐藏源当普通字形。
    SourceFoldWordAndType,
    /// 核Word输入后果后撤销，再观察正确折叠代次。
    SourceFoldWordUndo,
    /// 核Undo后沿真实导航回首，再重新折叠，旧几何不得续权。
    SourceFoldUndoAndRefold,
    /// 沿真实标题边缘删除Word再输入，保原GCB和canonical映射。
    SourceFoldDeleteWordAndType,
    /// 核删除及输入两次真实事务，再撤销输入。
    SourceFoldDeleteUndoType,
    /// 核删除结果仍在，再撤销删除，不能合并或重放两笔事务。
    SourceFoldDeletedUndo,
    /// 核标题字节恢复后撤销原前缀，完整历史须回到原根。
    SourceFoldUndoPrefix,
    /// 核原文保存，接纳原导航和菜单后立即Return，迟到动作不得复活。
    SourceFoldRestoreAndReturn,
    // Pending按钮和相对链接保原请求、目标安装与严格释放；不会因换成Host缩短序列。
    /// 在同一原栈接纳Source请求并取消，不能等待Ready再假造取消场景。
    SourcePendingRequestAndCancel,
    /// 核取消仍处于原预览及准确Input，旧请求不能迟到发布Source。
    VerifySourcePendingPreview,
    /// 再发一次真实Source请求，必须有独立的新模式票。
    SourcePendingRequestSource,
    /// 等新请求的原Source完整帧，不沿已取消旧请求补签。
    VerifySourcePendingFrame,
    /// 核原模式取消和旧字体资源退休，同时保原文稿及历史。
    VerifySourcePendingRetired,
    /// 最终只沿原会话正式返回，关闭后旧模式通知不能排旧步骤。
    SourcePendingReturn,
    /// 显式场景在独占库create_new目标，再向原Doc插未保存的真实链接前缀。
    RelativeLinkEdit,
    /// 等原编辑完整帧并流式核原源，只保有限片段和独立FD。
    VerifyRelativeLinkEditedFrame,
    /// 按真实链接字形生成原Down/Up，保存、拆图和目标安装均由产品执行。
    RelativeLinkMouseUp,
    /// 等预声明目标安装及完整CPU帧，旧Doc和原图资源必须真实释放。
    VerifyRelativeLinkTargetFrame,
    /// 核原未保存编辑已实际落盘，目标文件字节及元数据保持不变。
    VerifyRelativeLinkSaved,
    /// 在同一原Host窗口结束已绑定目标，关闭后分别核源保存与目标释放。
    RelativeLinkReturn,
}

impl NativeMarkdownHostedStep {
    /// 专项末项在同一动作内正式返回；Host须先绑定关闭收据，之后只等原释放，不能重排旧步骤。
    pub fn closes_surface(self) -> bool {
        matches!(
            self,
            Self::SourceAttachmentRestoreAndReturn
                | Self::SourceFoldRestoreAndReturn
                | Self::SourcePendingReturn
                | Self::RelativeLinkReturn
        )
    }

    /// 编码只在内部复用原完整用例；外部仍只消费具名业务动作，不另建一份输入实现。
    fn original_stage(self) -> Option<i16> {
        match self {
            Self::HighlightSelectSource => Some(425),
            Self::HighlightSourceOrigin => Some(426),
            Self::HighlightChineseInput => Some(427),
            Self::VerifyHighlightChinese => Some(428),
            Self::HighlightNonBmpInput => Some(429),
            Self::VerifyHighlightNonBmp => Some(430),
            Self::VerifyHighlightComposition => Some(431),
            Self::HighlightScroll => Some(432),
            Self::HighlightScrollOrigin => Some(433),
            Self::VerifyHighlightScroll => Some(434),
            Self::HighlightRestoreAndPreview => Some(435),
            Self::HighlightPreviewInput => Some(436),
            Self::VerifyHighlightPreviewAndHide => Some(437),
            Self::VerifyHighlightHidden => Some(438),
            Self::SourceFind => Some(440),
            Self::SourceSelectMode => Some(441),
            Self::VerifySourceMode => Some(442),
            Self::VerifySourceFrame => Some(443),
            Self::SourceRetiredAndFind => Some(445),
            Self::VerifySourceFoundFrame => Some(446),
            Self::SourceInsert => Some(447),
            Self::VerifySourceInsertedFrame => Some(448),
            Self::SourceVerifyInsertAndUndo => Some(449),
            Self::VerifySourceUndoneFrame => Some(450),
            Self::SourceVerifyUndoAndRedo => Some(451),
            Self::VerifySourceRedoneFrame => Some(452),
            Self::SourceVerifyRedoAndRestore => Some(453),
            Self::VerifySourceRestoredFrame => Some(454),
            Self::SourceVerifyRestoreAndSave => Some(455),
            Self::SourceSelectPreview => Some(456),
            Self::VerifySourcePreview => Some(457),
            Self::SourceRetiredAndReenter => Some(458),
            Self::VerifySourceReenteredMode => Some(459),
            Self::VerifySourceReenteredFrame => Some(460),
            Self::SourceAttachmentTyping => Some(461),
            Self::SourceAttachmentUndoTyping => Some(462),
            Self::SourceAttachmentUndoLink => Some(463),
            Self::SourceAttachmentRedoLink => Some(464),
            Self::SourceAttachmentUndoRedoneLink => Some(465),
            Self::SourceAttachmentUndoPrefix => Some(466),
            Self::SourceAttachmentMissingSource => Some(467),
            Self::SourceAttachmentFailureEditable => Some(468),
            Self::SourceAttachmentRestoreAndReturn => Some(469),
            Self::SourceFoldMenu => Some(540),
            Self::SourceFoldHiddenAndExpand => Some(541),
            Self::SourceFoldFrameAndMenu => Some(542),
            Self::SourceFoldFindHidden => Some(543),
            Self::SourceFoldFindAndSelectAll => Some(544),
            Self::SourceFoldSelectAllAndExpand => Some(545),
            Self::SourceFoldExpandAndInsert => Some(546),
            Self::SourceFoldPrefixAndFold => Some(547),
            Self::SourceFoldWordAndType => Some(548),
            Self::SourceFoldWordUndo => Some(549),
            Self::SourceFoldUndoAndRefold => Some(550),
            Self::SourceFoldDeleteWordAndType => Some(551),
            Self::SourceFoldDeleteUndoType => Some(552),
            Self::SourceFoldDeletedUndo => Some(553),
            Self::SourceFoldUndoPrefix => Some(554),
            Self::SourceFoldRestoreAndReturn => Some(555),
            Self::SourcePendingRequestAndCancel => Some(560),
            Self::VerifySourcePendingPreview => Some(561),
            Self::SourcePendingRequestSource => Some(562),
            Self::VerifySourcePendingFrame => Some(563),
            Self::VerifySourcePendingRetired => Some(564),
            Self::SourcePendingReturn => Some(565),
            Self::RelativeLinkEdit => Some(570),
            Self::VerifyRelativeLinkEditedFrame => Some(571),
            Self::RelativeLinkMouseUp => Some(572),
            Self::VerifyRelativeLinkTargetFrame => Some(573),
            Self::VerifyRelativeLinkSaved => Some(574),
            Self::RelativeLinkReturn => Some(575),
            _ => None,
        }
    }
}

// Full保原21步定位、14步高亮和10步编辑；giant同样消费这张完整表，不能跳过411或419。
const FULL_STEPS: &[NativeMarkdownHostedStep] = &[
    NativeMarkdownHostedStep::VerifyOpen,
    NativeMarkdownHostedStep::SelectSource,
    NativeMarkdownHostedStep::VerifySource,
    NativeMarkdownHostedStep::ShowFind,
    NativeMarkdownHostedStep::VerifyFindShown,
    NativeMarkdownHostedStep::EnterTableQuery,
    NativeMarkdownHostedStep::VerifyTableQuery,
    NativeMarkdownHostedStep::FindNext,
    NativeMarkdownHostedStep::VerifyTableFind,
    NativeMarkdownHostedStep::SelectPreview,
    NativeMarkdownHostedStep::VerifyPreview,
    NativeMarkdownHostedStep::SelectSource,
    NativeMarkdownHostedStep::VerifySource,
    NativeMarkdownHostedStep::EnterCodeQuery,
    NativeMarkdownHostedStep::VerifyCodeQuery,
    NativeMarkdownHostedStep::FindNext,
    NativeMarkdownHostedStep::VerifyCodeFind,
    NativeMarkdownHostedStep::SelectPreview,
    NativeMarkdownHostedStep::VerifyPreview,
    NativeMarkdownHostedStep::FindNext,
    NativeMarkdownHostedStep::VerifyCodeFindPreview,
    NativeMarkdownHostedStep::HighlightSelectSource,
    NativeMarkdownHostedStep::HighlightSourceOrigin,
    NativeMarkdownHostedStep::HighlightChineseInput,
    NativeMarkdownHostedStep::VerifyHighlightChinese,
    NativeMarkdownHostedStep::HighlightNonBmpInput,
    NativeMarkdownHostedStep::VerifyHighlightNonBmp,
    NativeMarkdownHostedStep::VerifyHighlightComposition,
    NativeMarkdownHostedStep::HighlightScroll,
    NativeMarkdownHostedStep::HighlightScrollOrigin,
    NativeMarkdownHostedStep::VerifyHighlightScroll,
    NativeMarkdownHostedStep::HighlightRestoreAndPreview,
    NativeMarkdownHostedStep::HighlightPreviewInput,
    NativeMarkdownHostedStep::VerifyHighlightPreviewAndHide,
    NativeMarkdownHostedStep::VerifyHighlightHidden,
    NativeMarkdownHostedStep::BeginEditing,
    NativeMarkdownHostedStep::VerifyInserted,
    NativeMarkdownHostedStep::UndoEditing,
    NativeMarkdownHostedStep::VerifyUndo,
    NativeMarkdownHostedStep::RedoEditing,
    NativeMarkdownHostedStep::VerifyRedo,
    NativeMarkdownHostedStep::RestoreEditing,
    NativeMarkdownHostedStep::VerifyRestored,
    NativeMarkdownHostedStep::SaveRestored,
    NativeMarkdownHostedStep::VerifySaved,
];

const SOURCE_SESSION_STEPS: &[NativeMarkdownHostedStep] = &[
    NativeMarkdownHostedStep::SourceFind,
    NativeMarkdownHostedStep::SourceSelectMode,
    NativeMarkdownHostedStep::VerifySourceMode,
    NativeMarkdownHostedStep::VerifySourceFrame,
    NativeMarkdownHostedStep::SourceRetiredAndFind,
    NativeMarkdownHostedStep::VerifySourceFoundFrame,
    NativeMarkdownHostedStep::SourceInsert,
    NativeMarkdownHostedStep::VerifySourceInsertedFrame,
    NativeMarkdownHostedStep::SourceVerifyInsertAndUndo,
    NativeMarkdownHostedStep::VerifySourceUndoneFrame,
    NativeMarkdownHostedStep::SourceVerifyUndoAndRedo,
    NativeMarkdownHostedStep::VerifySourceRedoneFrame,
    NativeMarkdownHostedStep::SourceVerifyRedoAndRestore,
    NativeMarkdownHostedStep::VerifySourceRestoredFrame,
    NativeMarkdownHostedStep::SourceVerifyRestoreAndSave,
    NativeMarkdownHostedStep::SourceSelectPreview,
    NativeMarkdownHostedStep::VerifySourcePreview,
    NativeMarkdownHostedStep::SourceRetiredAndReenter,
    NativeMarkdownHostedStep::VerifySourceReenteredMode,
    NativeMarkdownHostedStep::VerifySourceReenteredFrame,
    NativeMarkdownHostedStep::SourceAttachmentTyping,
    NativeMarkdownHostedStep::SourceAttachmentUndoTyping,
    NativeMarkdownHostedStep::SourceAttachmentUndoLink,
    NativeMarkdownHostedStep::SourceAttachmentRedoLink,
    NativeMarkdownHostedStep::SourceAttachmentUndoRedoneLink,
    NativeMarkdownHostedStep::SourceAttachmentUndoPrefix,
    NativeMarkdownHostedStep::SourceAttachmentMissingSource,
    NativeMarkdownHostedStep::SourceAttachmentFailureEditable,
    NativeMarkdownHostedStep::SourceAttachmentRestoreAndReturn,
];

const SOURCE_FOLDING_STEPS: &[NativeMarkdownHostedStep] = &[
    NativeMarkdownHostedStep::SourceFind,
    NativeMarkdownHostedStep::SourceSelectMode,
    NativeMarkdownHostedStep::VerifySourceMode,
    NativeMarkdownHostedStep::VerifySourceFrame,
    NativeMarkdownHostedStep::SourceFoldMenu,
    NativeMarkdownHostedStep::SourceFoldHiddenAndExpand,
    NativeMarkdownHostedStep::SourceFoldFrameAndMenu,
    NativeMarkdownHostedStep::SourceFoldFindHidden,
    NativeMarkdownHostedStep::SourceFoldFindAndSelectAll,
    NativeMarkdownHostedStep::SourceFoldSelectAllAndExpand,
    NativeMarkdownHostedStep::SourceFoldExpandAndInsert,
    NativeMarkdownHostedStep::SourceFoldPrefixAndFold,
    NativeMarkdownHostedStep::SourceFoldWordAndType,
    NativeMarkdownHostedStep::SourceFoldWordUndo,
    NativeMarkdownHostedStep::SourceFoldUndoAndRefold,
    NativeMarkdownHostedStep::SourceFoldDeleteWordAndType,
    NativeMarkdownHostedStep::SourceFoldDeleteUndoType,
    NativeMarkdownHostedStep::SourceFoldDeletedUndo,
    NativeMarkdownHostedStep::SourceFoldUndoPrefix,
    NativeMarkdownHostedStep::SourceFoldRestoreAndReturn,
];

const SOURCE_PENDING_STEPS: &[NativeMarkdownHostedStep] = &[
    NativeMarkdownHostedStep::SourceFind,
    NativeMarkdownHostedStep::SourcePendingRequestAndCancel,
    NativeMarkdownHostedStep::VerifySourcePendingPreview,
    NativeMarkdownHostedStep::SourcePendingRequestSource,
    NativeMarkdownHostedStep::VerifySourcePendingFrame,
    NativeMarkdownHostedStep::VerifySourcePendingRetired,
    NativeMarkdownHostedStep::SourcePendingReturn,
];

const SOURCE_RELATIVE_STEPS: &[NativeMarkdownHostedStep] = &[
    NativeMarkdownHostedStep::VerifyOpen,
    NativeMarkdownHostedStep::RelativeLinkEdit,
    NativeMarkdownHostedStep::VerifyRelativeLinkEditedFrame,
    NativeMarkdownHostedStep::RelativeLinkMouseUp,
    NativeMarkdownHostedStep::VerifyRelativeLinkTargetFrame,
    NativeMarkdownHostedStep::VerifyRelativeLinkSaved,
    NativeMarkdownHostedStep::RelativeLinkReturn,
];

/// 只持原弱正文、标量收据及原动作的有限请求；实际session与Native强图继续完全归Host。
pub struct NativeMarkdownHostedWorkload {
    workload: NativeMarkdownMemoryWorkload,
    editing: RefCell<Option<HostedEditingProbe>>,
    editing_started: Cell<bool>,
    editing_completed: Cell<bool>,
    editing_failed: Cell<bool>,
}

impl NativeMarkdownHostedWorkload {
    /// 观察已由真实Host激活的指定合成笔记；路径不符立即失败，不切换笔记或打开新会话。
    pub fn observe_active_document(
        path: &Path,
        flow: NativeMarkdownHostedFlow,
    ) -> Result<Self, String> {
        require_main_thread()?;
        let path = path
            .canonicalize()
            .map_err(|_| "无法解析Hosted合成笔记路径")?;
        let workload = NativeMarkdownMemoryWorkload {
            flow,
            path,
            facts: RefCell::new(None),
            cpu_frame: RefCell::new(None),
            pending_mode: RefCell::new(None),
            pending_find: RefCell::new(None),
            pending_query: Cell::new(None),
            verified_query: Cell::new(None),
            find_highlights: RefCell::new(None),
            source_session: RefCell::new(None),
            relative_link: RefCell::new(None),
        };
        // 先核实际document.path和磁盘基线，再固定原Weak；之后VerifyOpen不得重签另一窗口。
        let facts = workload.capture_open_facts()?;
        *workload.facts.borrow_mut() = Some(facts);
        workload.verify_unchanged()?;
        Ok(Self {
            workload,
            editing: RefCell::new(None),
            editing_started: Cell::new(false),
            editing_completed: Cell::new(false),
            editing_failed: Cell::new(false),
        })
    }

    /// 复用原工作负载生产动作与检查；调用者用真实事件边界安排后继验证，不执行嵌套run loop。
    pub fn step(&self, step: NativeMarkdownHostedStep) -> Result<(), String> {
        require_main_thread()?;
        if let Some(stage) = step.original_stage() {
            return match stage {
                425..=438 => self.workload.exercise_find_highlights(stage),
                440..=469 | 560..=565 => self.workload.exercise_source_session(stage),
                540..=555 => self.workload.exercise_source_folding(stage),
                570..=575 => self.workload.exercise_relative_link(stage),
                _ => unreachable!("原具名步骤只签发有限专项阶段"),
            };
        }
        match step {
            NativeMarkdownHostedStep::VerifyOpen => self.workload.verify_open(), // 原401。
            NativeMarkdownHostedStep::SelectSource => self.workload.change_mode(true), // 原402/412。
            NativeMarkdownHostedStep::VerifySource => self.workload.verify_mode(true), // 原403/413。
            NativeMarkdownHostedStep::ShowFind => self.workload.show_find(), // 原404，插件内部入口。
            NativeMarkdownHostedStep::VerifyFindShown => self.workload.verify_find_shown(), // 原405。
            NativeMarkdownHostedStep::EnterTableQuery => self.workload.enter_query(false), // 原406。
            NativeMarkdownHostedStep::VerifyTableQuery => self.workload.verify_query(false), // 原407。
            NativeMarkdownHostedStep::FindNext => self.workload.find_next(), // 原408/416/420。
            NativeMarkdownHostedStep::VerifyTableFind => self.workload.verify_find(false, false), // 原409。
            NativeMarkdownHostedStep::SelectPreview => self.workload.change_mode(false), // 原410/418。
            NativeMarkdownHostedStep::VerifyPreview => self.workload.verify_mode(false), // 原411/419。
            NativeMarkdownHostedStep::EnterCodeQuery => self.workload.enter_query(true), // 原414。
            NativeMarkdownHostedStep::VerifyCodeQuery => self.workload.verify_query(true), // 原415。
            NativeMarkdownHostedStep::VerifyCodeFind => self.workload.verify_find(true, false), // 原417。
            NativeMarkdownHostedStep::VerifyCodeFindPreview => {
                self.workload.verify_find(true, true)
            } // 原421。
            step @ (NativeMarkdownHostedStep::BeginEditing
            | NativeMarkdownHostedStep::VerifyInserted
            | NativeMarkdownHostedStep::UndoEditing
            | NativeMarkdownHostedStep::VerifyUndo
            | NativeMarkdownHostedStep::RedoEditing
            | NativeMarkdownHostedStep::VerifyRedo
            | NativeMarkdownHostedStep::RestoreEditing
            | NativeMarkdownHostedStep::VerifyRestored
            | NativeMarkdownHostedStep::SaveRestored
            | NativeMarkdownHostedStep::VerifySaved) => self.editing_step(step),
            _ => unreachable!("专项步骤已经在原用例入口消费"),
        }
    }

    /// 下一验证只观察动作保存的原票；返回 false 后必须等同一请求终点，不能自行重排检查。
    pub fn observe_step_terminal(
        &self,
        step: NativeMarkdownHostedStep,
        observer: impl FnOnce() + 'static,
    ) -> Result<bool, String> {
        require_main_thread()?;
        if let Some(stage) = step.original_stage() {
            // 所等的是下个动作的原先决终点；末项将自行关闭，关闭后不再给旧步骤续权。
            let observed = match stage {
                426..=438 => {
                    self.workload.observe_memory_terminal(stage, observer)?;
                    true
                }
                // 原模式按钮只接纳异步动作；先等同一Source完整帧，再核模式与字段焦点。
                // 442/459不能仅让出一个事件便视为完成，旧Preview帧也无权唤醒它们。
                441
                | 442
                | 443
                | 445
                | 446
                | 448
                | 450
                | 452
                | 454
                | 457
                | 458
                | 459
                | 460
                | 462..=469
                | 561
                | 563
                | 564 => {
                    self.workload
                        .observe_source_session_terminal(stage, observer)?;
                    true
                }
                540..=555 => {
                    self.workload
                        .observe_source_folding_terminal(stage, observer)?;
                    true
                }
                571 | 573 => {
                    self.workload
                        .observe_relative_link_terminal(stage, observer)?;
                    true
                }
                _ => {
                    drop(observer);
                    false
                }
            };
            return Ok(!observed);
        }
        let stage = match step {
            NativeMarkdownHostedStep::VerifyOpen => Some(401),
            NativeMarkdownHostedStep::VerifySource => Some(403),
            NativeMarkdownHostedStep::VerifyPreview => Some(411),
            NativeMarkdownHostedStep::VerifyTableFind => Some(409),
            NativeMarkdownHostedStep::VerifyCodeFind => Some(417),
            NativeMarkdownHostedStep::VerifyCodeFindPreview => Some(421),
            _ => None,
        };
        if let Some(stage) = stage {
            self.workload.observe_memory_terminal(stage, observer)?;
            return Ok(false); // 同步已有真实帧也由一次原通知排事件，不在本栈抢跑检查。
        }
        if matches!(
            step,
            NativeMarkdownHostedStep::VerifyInserted
                | NativeMarkdownHostedStep::VerifyUndo
                | NativeMarkdownHostedStep::VerifyRedo
                | NativeMarkdownHostedStep::VerifyRestored
                | NativeMarkdownHostedStep::VerifySaved
        ) {
            // 已接受的实际编辑票拥有目标 Input；先移出槽再登记 Native weak 与一次帧通知，
            // 不能在 RefCell 借用中重入，也不能借原只读 g0 事实拒绝合法的 g+1..4。
            let probe = self
                .editing
                .borrow_mut()
                .take()
                .ok_or("编辑终点没有原编辑票")?;
            let observed = probe.observe_terminal(observer);
            *self.editing.borrow_mut() = Some(probe);
            observed?;
            return Ok(false);
        }
        drop(observer); // 无异步后果的动作仍沿原下一独立事件，不凭排队次数推断像素完成。
        Ok(true)
    }

    /// 只在Host原三秒期限已失败后打印同一原Body现成事实；不推进队列或消费真实失败槽。
    pub fn diagnose_deadline(&self, step: NativeMarkdownHostedStep) {
        let stage = step.original_stage().unwrap_or(match step {
            NativeMarkdownHostedStep::VerifyOpen => 401,
            NativeMarkdownHostedStep::VerifySource => 403,
            NativeMarkdownHostedStep::VerifyPreview => 411,
            NativeMarkdownHostedStep::VerifyTableFind => 409,
            NativeMarkdownHostedStep::VerifyCodeFind => 417,
            NativeMarkdownHostedStep::VerifyCodeFindPreview => 421,
            _ => 0,
        });
        self.workload.diagnose_frame_deadline(stage);
    }

    /// 原编辑票先移出RefCell再执行Native；失败永久记住，不能重跑乱序步骤伪造成功。
    fn editing_step(&self, step: NativeMarkdownHostedStep) -> Result<(), String> {
        if self.editing_failed.get() {
            return Err("长文编辑验收已有失败，不能恢复旧步骤权限".into());
        }
        let result = if step == NativeMarkdownHostedStep::BeginEditing {
            if self.editing_started.replace(true) {
                Err("长文编辑验收不能重复开始".into())
            } else {
                HostedEditingProbe::begin(&self.workload).map(|probe| {
                    *self.editing.borrow_mut() = Some(probe);
                })
            }
        } else {
            let probe = self.editing.borrow_mut().take();
            match probe {
                Some(mut probe) => {
                    let result = probe.step(&self.workload, step);
                    if result.is_ok() && probe.completed() {
                        self.editing_completed.set(true); // 只在VerifySaved完整通过后记完成。
                    }
                    *self.editing.borrow_mut() = Some(probe);
                    result
                }
                None => Err("长文编辑步骤缺少原BeginEditing票据".into()),
            }
        };
        if result.is_err() {
            self.editing_failed.set(true); // 结束所有Native/RefCell借用后固定首个失败。
        }
        result
    }

    fn require_editing_completion(&self) -> Result<(), String> {
        if self.editing_failed.get()
            || (self.editing_started.get() && !self.editing_completed.get())
        {
            return Err("长文编辑尚未恢复保存并完成验证，不能报告正常返回成功".into());
        }
        Ok(())
    }

    /// 只给仍属原会话的真实surface发送正式ExitToLauncher；不终止应用，不伪造Host回执。
    pub fn request_return(&self) -> Result<(), String> {
        require_main_thread()?;
        // 失败清理也必须能结束本原会话；成功字节和历史仍由每步及关闭核验独立证明。
        // 同一Doc可以合法处于g+1或g+4，不能用只读g0检查阻止真实保存与释放。
        let editing = self.editing.borrow_mut().take();
        drop(editing); // 严格关闭之前释放有限片段和所有动作票；没有正文或Native强owner转交completion。
        self.workload.request_return()
    }

    /// 核原Native资源与只读磁盘事实；Host另核matching close、session Drop和LauncherReady。
    pub fn verify_native_closed(&self) -> Result<(), String> {
        require_main_thread()?;
        self.require_editing_completion()?;
        self.workload.verify_native_closed()?;
        if self.workload.flow == NativeMarkdownHostedFlow::SourceSession {
            self.workload.verify_source_attachment_canceled()?;
        }
        Ok(())
    }
}

/// TLS空槽只有在产品主线程才有意义；后台线程不能靠自己的空TLS伪报整个Native图已关闭。
fn require_main_thread() -> Result<(), String> {
    MainThreadMarker::new()
        .map(|_| ())
        .ok_or_else(|| "Hosted工作负载只能在真实AppKit主线程调用".to_owned())
}
