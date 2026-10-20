//! 独立不可变正文值只共享持久树与实际引用的字节资源，不保活活动文档或历史。

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

#[cfg(feature = "paged-shaping-frontier")]
#[path = "bidi_paragraph.rs"]
mod bidi_paragraph;
#[path = "printable_ascii.rs"]
mod printable_ascii;
#[path = "text_checking_range.rs"]
mod text_checking_range;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use bidi_paragraph::{FrozenBidiParagraphCursor, FrozenBidiParagraphStep};

#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use printable_ascii::{FrozenPrintableAsciiSuffix, FrozenPrintableAsciiSuffixStep};

#[derive(Clone)]
/// 当前正文的不可变快照；克隆只增加一个Arc，不复制UTF-8或UTF-16正文。
///
/// 原文件使用已经独立冻结的匿名映射，编辑字节使用不可变共享段。活动文档后续
/// 编辑、Undo/Redo或销毁都不会改变这些字节；最后一个快照释放后才归还其独占资源。
pub struct MarkdownTextSnapshot {
    text: Arc<FrozenText>,
}

/// 只保留读取所需资源：原root、匿名映射的真实payload范围和root引用的编辑段。
///
/// 稀疏索引沿用原segment编号，避免为了连续索引保活已经删除、仅供历史使用的段。
struct FrozenText {
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    projection: Option<Arc<super::display_projection::DisplayProjection>>,
    root: PieceTreeLink,
    original_snapshot: Option<Arc<OriginalBacking>>,
    original_range: Range<usize>,
    edits: HashMap<usize, Arc<MarkdownEditSegment>>,
    // 只列本root实际拥有的文件资源；Heap无IO失败语义，热读取不再遍历整个稀疏段表。
    edit_backings: Vec<Arc<OriginalBacking>>,
    // 同一文档的全部capture/clone共享一个窗口；这里只分享派生缓存，不分享活动文档。
    utf16_read_cache: Arc<Mutex<read_cache::Utf16ReadCache>>,
    // 原捕获代次参与同一cache键；老值和活动文档交错读取不能用新的代次补签。
    generation: u64,
    // 草稿门不持Doc；canonical始终None。真实读取名额必须晚于树、段与健康backing归还。
    #[cfg(feature = "s2-table-window-prototype")]
    draft_guard: Option<cell_draft::FrozenDraftGuard>,
    #[cfg(feature = "s2-table-window-prototype")]
    _draft_read_claim: Option<cell_draft::ReadClaim>,
}

impl MarkdownTextSnapshot {
    /// 表格物理行只能绑定当前 canonical 根；同内容另一文档、旧代次与局部显示值不能补签。
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(crate) fn shares_current_table_root(&self, document: &MarkdownDocumentBuffer) -> bool {
        self.text.projection.is_none()
            && self.text.generation == document.generation
            && self.text.draft_guard.is_none()
            && self.text.root.as_ref().is_some_and(|root| {
                document
                    .root
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(root, current))
            })
    }
    /// 原树与显示目录是不同读取来源；调用者必须显式选择，不能把投影的 None root 当成空文档。
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn display_projection(
        &self,
    ) -> Option<&Arc<super::display_projection::DisplayProjection>> {
        self.text.projection.as_ref()
    }

    /// 只从真实封存显示目录建立冻结值；没有活动 Doc、正文复制或显示 AVL 节点。
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(crate) fn from_display_projection(
        projection: Arc<super::display_projection::DisplayProjection>,
        generation: u64,
    ) -> Self {
        Self {
            text: Arc::new(FrozenText {
                projection: Some(projection),
                root: None,
                original_snapshot: None,
                original_range: 0..0,
                edits: HashMap::new(),
                edit_backings: Vec::new(),
                utf16_read_cache: Arc::new(Mutex::new(read_cache::Utf16ReadCache::default())),
                generation,
                draft_guard: None,
                _draft_read_claim: None,
            }),
        }
    }

    /// 摘要必须与实际读取字节同源；显示路径只取转换时已累积的真实可见标量事实。
    fn metrics(&self) -> MarkdownDocumentMetrics {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.metrics();
        }
        piece_tree_metrics(&self.text.root)
    }
    /// 只建立原树路径；混合端点的真实字节读取留给原调用方配额。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_printable_ascii_suffix(
        &self,
        range: Range<usize>,
    ) -> Result<FrozenPrintableAsciiSuffix, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        if range.start > range.end || range.end > self.utf16_len() {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(FrozenPrintableAsciiSuffix::new(self, range))
    }

    /// 健康前后核验保持同一Frozen；结束实际lease后才返回精确后缀事实。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_printable_ascii_suffix(
        &self,
        cursor: &mut FrozenPrintableAsciiSuffix,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<FrozenPrintableAsciiSuffixStep, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let result = cursor.advance(self, bytes, scalars, metadata);
        self.text.source_health()?;
        result
    }

    /// 真实B段查询只借同一不可变root；创建时不读取端点叶，细量子在advance里支付。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_bidi_paragraph(
        &self,
        position: usize,
    ) -> Result<FrozenBidiParagraphCursor, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        if position > self.utf16_len() {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(FrozenBidiParagraphCursor::new(self, position))
    }

    /// 同Frozen资源前后核验；候选端点只能在本步真实lease全部归还后交付。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_bidi_paragraph(
        &self,
        cursor: &mut FrozenBidiParagraphCursor,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<FrozenBidiParagraphStep, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let result = cursor.advance(self, bytes, scalars, metadata);
        self.text.source_health()?;
        result
    }

    pub(super) fn capture(document: &MarkdownDocumentBuffer) -> Self {
        Self::capture_root(
            document,
            &document.root,
            &document.edits,
            document.generation,
        )
    }

    /// 仅同模块受控候选可提供另一root与段表；普通Source没有此构造权限。
    /// 资源收集仍走同一实现，草稿不会复制Frozen实现或取得canonical票据。
    pub(super) fn capture_root(
        document: &MarkdownDocumentBuffer,
        root: &PieceTreeLink,
        edits: &MarkdownEditSegments,
        generation: u64,
    ) -> Self {
        let mut text = FrozenText {
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            projection: None,
            root: root.clone(),
            original_snapshot: None,
            original_range: document.original_range.clone(),
            edits: HashMap::new(),
            edit_backings: Vec::new(),
            utf16_read_cache: Arc::clone(&document.utf16_read_cache),
            generation,
            #[cfg(feature = "s2-table-window-prototype")]
            draft_guard: None,
            #[cfg(feature = "s2-table-window-prototype")]
            _draft_read_claim: None,
        };
        // 遍历只看叶的source元数据，不读取、解码或复制正文，也不访问历史与变化日志。
        retain_piece_sources(root.as_deref(), document, edits, &mut text);
        Self {
            text: Arc::new(text),
        }
    }

    /// 只读格共享原树切片；即使本格全为编辑片或为空，仍保原 Doc 来源的真实 I/O 健康事实。
    #[cfg(all(
        feature = "s2-table-window-prototype",
        any(
            feature = "paged-graphemes",
            feature = "paged-line-breaks",
            feature = "paged-shaping"
        )
    ))]
    pub(super) fn capture_table_cell_root(
        document: &MarkdownDocumentBuffer,
        root: &PieceTreeLink,
    ) -> Self {
        let mut snapshot = Self::capture_root(document, root, &document.edits, document.generation);
        let text = Arc::get_mut(&mut snapshot.text).expect("新只读格 Frozen 尚未共享");
        if text.original_snapshot.is_none() {
            // 这张格票来自原表格结构；源错误不能因切片恰无 Original piece 而退化为成功。
            text.original_snapshot = document.original_snapshot.clone();
        }
        snapshot
    }

    /// 同一Frozen构造完成后，在唯一新Arc上安装原草稿门；普通Source不能提供这些封闭参数。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(super) fn capture_draft_root(
        document: &MarkdownDocumentBuffer,
        root: &PieceTreeLink,
        edits: &MarkdownEditSegments,
        content_serial: u64,
        guard: cell_draft::FrozenDraftGuard,
        claim: cell_draft::ReadClaim,
    ) -> Self {
        let mut snapshot = Self::capture_root(document, root, edits, content_serial);
        let text = Arc::get_mut(&mut snapshot.text).expect("新Frozen尚未共享");
        text.draft_guard = Some(guard);
        text._draft_read_claim = Some(claim);
        snapshot
    }

    /// 后台metadata闸门只取得原scope弱票；不能用它捕获root或绕过Source域。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn cell_draft_source_authority(
        &self,
    ) -> Option<cell_draft::MarkdownCellDraftSourceAuthority> {
        self.text
            .draft_guard
            .as_ref()
            .map(|guard| guard.authority())
    }

    pub(crate) fn original_backing_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.text.source_health()
    }
    /// 原映射的原子健康事实，不持有正文强边；草稿保留原健康owner，canonical纯编辑值仍为None。
    #[must_use]
    pub fn original_backing_observer(&self) -> Option<crate::MarkdownOriginalBackingObserver> {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.raw().original_backing_observer();
        }
        let observer = self
            .text
            .original_snapshot
            .as_ref()
            .map(|backing| backing.observer());
        #[cfg(feature = "s2-table-window-prototype")]
        if observer.is_none() {
            // 纯编辑草稿仍保原Source健康owner；这里只交原子票，不伪造不存在的FD。
            return self
                .text
                .draft_guard
                .as_ref()
                .and_then(|guard| guard.original_backing_observer());
        }
        observer
    }

    #[must_use]
    /// 从冻结root的缓存聚合读取真实UTF-8字节数，不扫描正文。
    pub fn byte_len(&self) -> usize {
        self.metrics().byte_len
    }

    #[must_use]
    /// 从冻结root的缓存聚合读取真实UTF-16单元数，不建立编码镜像。
    pub fn utf16_len(&self) -> usize {
        self.metrics().utf16_len
    }

    /// 原逐scalar计量相等即没有代理对；这个字符域事实只证明UTF16端点，不证明GCB或段落。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn utf16_boundaries_are_scalar(&self) -> Result<bool, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let metrics = self.metrics();
        let dense = metrics.character_count == metrics.utf16_len;
        self.text.source_health()?;
        Ok(dense)
    }

    /// 定位已夹到文档内的文本检查段落范围，仅返回全局UTF-16元数据，不物化正文。
    ///
    /// CR、LF和U+2029是终止单元，NEL与U+2028仍在段内。非空请求使用末个所选单元
    /// 定位尾段；CR向前合并紧随的LF，而位于CRLF内部LF的caret保留单LF范围。
    /// 两端通过树聚合跳过正文，最多解码各一个4KiB命中叶及CR的一个后继单元。
    /// 允许请求落在低代理项；这里只观察范围，不授予编辑或整段物化权限。
    ///
    /// # Errors
    ///
    /// 倒置或越过EOF的请求在访问piece字节前返回`RangeOutsideDocument`。
    /// `NSNotFound`和部分越界的标准调整由原生调用方负责，不在此猜测或静默截断。
    pub fn text_checking_paragraph_range(
        &self,
        request: Range<usize>,
    ) -> Result<Range<usize>, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.text_checking_paragraph_range(request);
        }
        text_checking_range::paragraph_range(self.text.as_ref(), self.text.root.as_deref(), request)
    }

    /// 精确判断同一冻结root的UTF-16范围是否全为U+0020..U+007E。
    ///
    /// 全ASCII子树的任意子范围只读缓存聚合；混合子树两端的部分小片各借至多4KiB。
    /// 不复制或顺扫整个范围，也不为非零item或style范围重复读取已证明为ASCII的边叶。
    /// 空范围为true，但其位置仍必须是合法标量边界，不能以空范围绕过代理项校验。
    ///
    /// # Errors
    /// 倒置、越界或切入代理项均明确拒绝；真实源故障在查询前后核验并优先于谓词结果。
    pub fn is_printable_ascii_range(
        &self,
        range: Range<usize>,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.is_printable_ascii_range(range);
        }
        printable_ascii::query(self.text.as_ref(), self.text.root.as_deref(), range)
    }

    /// 只有全源精确ASCII事实才建立原树游标；Source authority由原布局owner提供，不另捕快照。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_ascii_word_run(
        &self,
        position: usize,
        forward: bool,
        authority: crate::MarkdownLayoutTextAuthority,
    ) -> Result<MarkdownAsciiWordRun, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(projection) = self.display_projection() {
            if position > self.utf16_len()
                || !projection.metrics().all_printable_ascii.is_printable()
            {
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            return Ok(MarkdownAsciiWordRun::new_display(
                position, forward, authority,
            ));
        }
        if position > self.utf16_len()
            || !self
                .text
                .root
                .as_ref()
                .is_none_or(|root| root.metrics().all_printable_ascii.is_printable())
        {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let run = MarkdownAsciiWordRun::new(&self.text.root, position, forward, authority);
        self.text.source_health()?;
        Ok(run)
    }

    /// 显示目录只授其同长连续 raw run；全 ASCII 证明属于已封存 display 摘要，不在构造时重扫。
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(super) fn begin_scoped_ascii_word_run(
        &self,
        position: usize,
        forward: bool,
        authority: crate::MarkdownLayoutTextAuthority,
        range: Range<usize>,
    ) -> Result<MarkdownAsciiWordRun, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        if self.display_projection().is_some()
            || range.start > position
            || position > range.end
            || range.end > self.utf16_len()
        {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(MarkdownAsciiWordRun::new_range(
            &self.text.root,
            position,
            forward,
            authority,
            range,
        ))
    }

    /// 同一冻结payload在访问前后核真实健康；每步只消费调用方剩余的原bytes/scalar/metadata。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_ascii_word_run(
        &self,
        run: &mut MarkdownAsciiWordRun,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<MarkdownAsciiWordStep, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(projection) = self.display_projection() {
            let result = run.advance_display(projection, bytes, scalars, metadata);
            self.text.source_health()?;
            return result;
        }
        let result = run.advance(self.text.as_ref(), bytes, scalars, metadata);
        self.text.source_health()?;
        result
    }

    /// 全冻结 root 的精确字母事实：None 表示含控制符/Unicode，Some 区分全可打印域是否含字母。
    /// 只读已在建叶与真实编辑时维护的聚合，不借正文、不扫描范围，也不沿用后继快照的事实。
    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn printable_ascii_has_letters(
        &self,
    ) -> Result<Option<bool>, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let class = self.metrics().all_printable_ascii;
        self.text.source_health()?; // 正聚合也不能掩盖查询期间真实到达的原映射错误。
        Ok(class.has_letters())
    }

    /// 只读这一冻结root在打开校验与真实编辑时维护的完整Unicode属性集合。
    /// 查询不捕获后继snapshot、不借正文；源故障在取值前后核验，不能被正摘要掩盖。
    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn unicode_properties(
        &self,
    ) -> Result<MarkdownUnicodeProperties, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let properties = self.metrics().unicode_properties;
        self.text.source_health()?;
        Ok(properties)
    }

    /// 将UTF-16标量边界映射到同一冻结值的字节位置；代理对内部位置仍明确拒绝。
    pub fn byte_offset_for_utf16(
        &self,
        utf16_offset: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.byte_offset_for_utf16(utf16_offset);
        }
        if utf16_offset > self.utf16_len() {
            // 在下降树前拒绝越界；EOF仍是可用于空范围的真实边界。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        byte_offset_for_utf16_in_tree(
            self.text.as_ref(),
            self.text.root.as_deref(),
            utf16_offset,
            0,
        )
    }

    /// 将UTF-8标量边界映射到冻结值的UTF-16位置；多字节标量内部不是合法边界。
    pub fn utf16_offset_for_byte(
        &self,
        byte_offset: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.utf16_offset_for_byte(byte_offset);
        }
        if byte_offset > self.byte_len() {
            // 先校验总体范围，避免叶解码器借用不存在的前缀。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        utf16_offset_for_byte_in_tree(
            self.text.as_ref(),
            self.text.root.as_deref(),
            byte_offset,
            0,
        )
    }

    /// 读取一个NSString兼容UTF-16单元；允许低代理项，EOF不是可读单元。
    pub fn utf16_code_unit_at(
        &self,
        utf16_offset: usize,
    ) -> Result<u16, MarkdownDocumentBufferError> {
        // 原匿名映射一旦失败，先前已缓存的相同单元也不能重新获得读取权限。
        self.text.source_health()?;
        if utf16_offset >= self.utf16_len() {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let mut unit = [0];
        self.copy_utf16_code_units(utf16_offset, &mut unit)?;
        Ok(unit[0])
    }

    /// 完整填充调用方真实范围；所有snapshot clone只共享一个最多8KiB的读取窗口。
    ///
    /// 代理对内部范围仍合法；只在窗口完整填充且源回收成功后发布。跨线程锁poison
    /// 明确失败，不清锁、不将部分窗口当成正文，也不回退整段或整篇副本。
    pub fn copy_utf16_code_units(
        &self,
        utf16_offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.copy_utf16_code_units(utf16_offset, destination);
        }
        let mut cache = self
            .text
            .utf16_read_cache
            .lock()
            .map_err(|_| MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)?;
        // 共享窗口必须同时匹配本冻结值的原root与原代次；另一个capture只能替换派生值。
        cache.copy(
            self.text.as_ref(),
            &self.text.root,
            self.text.generation,
            utf16_offset,
            destination,
        )
    }

    /// 顺序借用指定字节范围的有限piece；单次回调最多原4KiB叶长，正文不复制。
    ///
    /// 范围按原始UTF-8字节寻址，允许观察标量内的部分字节。需要文本边界的调用方
    /// 先使用坐标查询验证；回调失败立即停止，不读取后继叶，也不缓存回调结果。
    pub fn visit_byte_range(
        &self,
        range: Range<usize>,
        mut visit: impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.visit_byte_range(range, &mut visit);
        }
        if range.start > range.end || range.end > self.byte_len() {
            // 倒置或越界在第一次回调之前拒绝；空值仅有0..0可访问。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        visit_piece_tree_range(
            self.text.as_ref(),
            self.text.root.as_deref(),
            range,
            0,
            &mut visit,
        )
    }
}

/// 只收集当前root实际引用的资源；树形与正文仍复用原Arc，不重建叶或扫描字节。
fn retain_piece_sources(
    root: Option<&MarkdownPieceTreeNode>,
    document: &MarkdownDocumentBuffer,
    edits: &MarkdownEditSegments,
    frozen: &mut FrozenText,
) {
    let Some(root) = root else {
        // 空root没有正文资源，不因活动文档仍保留历史映射就将其带入快照。
        return;
    };
    match root {
        MarkdownPieceTreeNode::OriginalSpan(_) => {
            // Span只压缩原始小片元数据，仍共享原匿名payload，不持活动Doc或另建映射。
            if frozen.original_snapshot.is_none() {
                frozen.original_snapshot = Some(Arc::clone(
                    document
                        .original_snapshot
                        .as_ref()
                        .expect("原始Span必须有不可变映射"),
                ));
            }
        }
        MarkdownPieceTreeNode::Leaf(piece) => match piece.source {
            MarkdownPieceSource::Original => {
                if frozen.original_snapshot.is_none() {
                    // 多个原始叶只共同保留同一个独立匿名映射，不复制或重新映射正文。
                    frozen.original_snapshot = Some(Arc::clone(
                        document
                            .original_snapshot
                            .as_ref()
                            .expect("an original piece requires its immutable mapping"),
                    ));
                }
            }
            MarkdownPieceSource::EditSegment(index) => {
                // 同一编辑段可能被多个分裂叶使用；只按原编号保留一个Arc。
                frozen.edits.entry(index).or_insert_with(|| {
                    let segment = &edits.segments[index];
                    if let MarkdownEditSegment::Frozen(backing) = segment.as_ref() {
                        // 同一编号只在首次真正引用时登记一次backing；这是本Frozen的资源强边。
                        frozen.edit_backings.push(Arc::clone(backing));
                    }
                    Arc::clone(segment)
                });
            }
        },
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            // AVL高度限制递归栈；顺序只影响元数据收集，不触发任何正文读取。
            retain_piece_sources(Some(left.as_ref()), document, edits, frozen);
            retain_piece_sources(Some(right.as_ref()), document, edits, frozen);
        }
    }
}

impl PieceByteSource for FrozenText {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = &self.projection {
            return projection.health();
        }
        #[cfg(feature = "s2-table-window-prototype")]
        let guarded = if let Some(guard) = &self.draft_guard {
            // 原受控capture给guard与Original叶同一Doc backing，且纯编辑raw仍保原Source健康。
            guard.backing_health()?;
            true
        } else {
            false
        };
        #[cfg(not(feature = "s2-table-window-prototype"))]
        let guarded = false;
        if !guarded && let Some(backing) = self.original_snapshot.as_deref() {
            backing.health()?; // 原映射保持既有优先级，不以编辑段的健康代替它。
        }
        for backing in &self.edit_backings {
            // 只核本root实际引用的资源；真实封存段失败也写回同草稿首因，不保活活动Doc。
            backing
                .health()
                .map_err(MarkdownDocumentBufferError::from)
                .inspect_err(|&error| {
                    #[cfg(feature = "s2-table-window-prototype")]
                    if let Some(guard) = &self.draft_guard {
                        guard.record_source(error);
                    }
                    #[cfg(not(feature = "s2-table-window-prototype"))]
                    let _ = error;
                })?;
        }
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(guard) = &self.draft_guard {
            guard.require_content()?; // 全部真实字节资源健康后只核权限，不重读同一个backing。
        }
        Ok(())
    }
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.read_piece_range(piece, 0..piece.byte_len)
    }

    /// 新Word路径传原叶内的实际范围；原完整piece读取继续使用上方原入口。
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.read_piece_range(piece, local)
    }

    fn with_piece_bytes<R>(
        &self,
        piece: &MarkdownPiece,
        visit: impl FnOnce(&[u8]) -> Result<R, MarkdownDocumentBufferError>,
    ) -> Result<R, MarkdownDocumentBufferError> {
        let bytes = self.piece_bytes(piece)?;
        let result = visit(&bytes);
        // 原lease归还错误优先；最后一个叶的回调也可能撤草稿，不能因没有后继叶而漏核。
        bytes.finish()?;
        #[cfg(feature = "s2-table-window-prototype")]
        if self.draft_guard.is_some() {
            self.source_health()?;
        }
        result
    }
}

impl FrozenText {
    /// 只调整实际lease范围，不派生摘要不符的伪piece；完整读取与Word共用同一资源边界。
    fn read_piece_range(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.source_health()?;
        if local.start > local.end || local.end > piece.byte_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        match piece.source {
            MarkdownPieceSource::Original => {
                let backing = self
                    .original_snapshot
                    .as_ref()
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                let start = self
                    .original_range
                    .start
                    .checked_add(piece.byte_start)
                    .and_then(|start| start.checked_add(local.start))
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                backing
                    .lease(start..start + local.len())
                    .map(PieceBytes::Original)
                    .map_err(Into::into)
            }
            MarkdownPieceSource::EditSegment(index) => {
                let segment = self
                    .edits
                    .get(&index)
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                segment.read(
                    piece.byte_start + local.start..piece.byte_start + local.end,
                    true,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "paged-shaping-frontier"))]
mod ascii_word_request_tests {
    use std::cell::RefCell;
    use std::io::Write;

    use super::*;

    /// 只观察同一FrozenText真实返回的lease范围，不在WordStep上复述预算计数。
    struct RequestedLeases<'a> {
        text: &'a FrozenText,
        requests: RefCell<Vec<Range<usize>>>,
    }

    impl PieceByteSource for RequestedLeases<'_> {
        fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
            self.text.source_health()
        }

        fn piece_bytes(
            &self,
            piece: &MarkdownPiece,
        ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
            let bytes = self.text.piece_bytes(piece)?;
            self.requests
                .borrow_mut()
                .push(piece.byte_start..piece.byte_start + bytes.len());
            Ok(bytes) // 若Word退回完整叶入口，真实lease长度会使下面的小预算断言失败。
        }

        fn piece_range_bytes(
            &self,
            piece: &MarkdownPiece,
            local: Range<usize>,
        ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
            let start = piece.byte_start + local.start;
            let bytes = self.text.piece_range_bytes(piece, local)?;
            self.requests.borrow_mut().push(start..start + bytes.len());
            Ok(bytes) // 返回的仍是实际mapping lease，测试没有复制或合成这一段正文。
        }
    }

    #[test]
    fn ascii_word_requests_only_remaining_quantum_from_real_frozen_leases_in_both_directions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("词组真实请求.md");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"prefix ").unwrap();
        let block = [b'x'; ORIGINAL_PIECE_BYTES];
        let word_bytes = 2 * 1024 * 1024;
        for _ in 0..word_bytes / block.len() {
            file.write_all(&block).unwrap();
        }
        file.write_all(b"!").unwrap();
        drop(file);
        let document = MarkdownDocumentBuffer::open(&path, 4 * 1024 * 1024).unwrap();
        let mapping = document.original_backing_observer().unwrap();
        let source = crate::MarkdownLayoutTextSource::capture(&document);
        let snapshot = source.snapshot_for_bidi(); // 克隆同一冻结payload，不另capture Source或正文。
        let observed = RequestedLeases {
            text: snapshot.text.as_ref(),
            requests: RefCell::new(Vec::new()),
        };
        for (maximum_bytes, maximum_scalars, maximum_metadata) in
            [(1, 256, 1), (4096, 1, 256), (7, 256, 256), (4096, 256, 256)]
        {
            for forward in [false, true] {
                let head = if forward { 7 } else { 7 + word_bytes };
                let mut run = source.begin_ascii_word_run(head, forward).unwrap();
                let mut finished = false;
                let mut actual_bytes = 0;
                for _ in 0..2 * ORIGINAL_PIECE_BYTES + 512 {
                    observed.requests.borrow_mut().clear();
                    let step = run
                        .advance(&observed, maximum_bytes, maximum_scalars, maximum_metadata)
                        .unwrap();
                    let requests = observed.requests.borrow();
                    for request in requests.iter() {
                        assert!(!request.is_empty());
                        assert!(request.len() <= maximum_bytes.min(maximum_scalars));
                    }
                    let requested = requests.iter().map(Range::len).sum::<usize>();
                    assert_eq!(step.bytes, requested); // 真正lease请求与量子费用一致，含提前遇到终止字符的范围。
                    assert!(step.bytes <= maximum_bytes && step.scalars <= maximum_scalars);
                    assert!(step.metadata <= maximum_metadata);
                    actual_bytes += requested;
                    if step.complete {
                        assert_eq!(step.position, if forward { 7 + word_bytes } else { 7 });
                        finished = true;
                        break;
                    }
                }
                assert!(finished);
                assert!(actual_bytes > 0 && actual_bytes <= 2 * ORIGINAL_PIECE_BYTES + 256);
            } // 正向从local起点借，反向从local之前借；budget1不能暗中取得原4KiB叶。
        }
        assert!(!document.can_undo() && !document.is_dirty());
        drop(observed);
        drop((snapshot, source, document));
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
    }
}
