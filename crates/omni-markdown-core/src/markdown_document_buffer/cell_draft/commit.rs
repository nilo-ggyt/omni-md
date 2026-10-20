//! 仅最终有限原始输入进入旧表格sanitize规则；一次canonical prepare/publish提供原Undo与失败回滚。

use super::edit::DraftSource;
use super::replacement::{CellReplacement, CellReplacementOutput};
use super::resources::CommitClaim;
use super::*;
use changes::{MarkdownObservedEditError, MarkdownTextChangeObserver};

/// 一张原输入serial的有限提交准入；不可Clone，不保活Doc、草稿root、Source或Scene。
/// 小结果保原有限String，大结果只保独立封存映射；不能由公开范围或字符串自行构造。
pub struct MarkdownCellDraftPreparedCommit {
    scope: Weak<Scope>,
    serial: u64,
    /// 范围只由封闭cell计划或真实End组合产生，外部不能自行填入跨块坐标。
    range: Range<usize>,
    replacement: CellReplacement,
    replacement_utf16: usize,
    /// 同次清洗得到的有向位置仍封存在候选内；只有真实发布或未改内容封闭后才交付调用方。
    selection: MarkdownTextSelection,
    /// 原Word首组两端或Shift锚点；同次清洗映射真实范围，禁止提交后从裸种子重新扩词。
    word_anchor: Option<MarkdownCellDraftWordAnchor>,
    unchanged: bool,
    next_cell: Option<(usize, usize)>,
    // 按字段Drop顺序，先真实释放上面的候选存储，再归还唯一准入槽及其分配收据。
    _claim: CommitClaim,
}

impl MarkdownCellDraftPreparedCommit {
    #[must_use]
    /// 追加成功后重新查询的行列；不是新cell激活权，旧window也不会因这份坐标变成当前来源。
    pub fn next_cell(&self) -> Option<(usize, usize)> {
        self.next_cell
    }

    #[cfg(test)]
    /// 实际候选映射的原子观察票不保活正文；测试不能用claim归零代替最后munmap。
    pub(super) fn replacement_backing_observer(
        &self,
    ) -> Option<crate::MarkdownOriginalBackingObserver> {
        self.replacement.backing_observer()
    }
}

#[derive(Debug)]
/// 草稿准入和原canonical事务首因分开保存；观察者/日志双失败不降成普通Stale。
pub enum MarkdownCellDraftCommitError<E> {
    Draft(MarkdownCellDraftError),
    Edit(MarkdownObservedEditError<E>),
}

impl<E> From<MarkdownCellDraftError> for MarkdownCellDraftCommitError<E> {
    fn from(error: MarkdownCellDraftError) -> Self {
        Self::Draft(error)
    }
}

impl<E: std::fmt::Display> std::fmt::Display for MarkdownCellDraftCommitError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Draft(error) => error.fmt(f),
            Self::Edit(error) => error.fmt(f),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for MarkdownCellDraftCommitError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Draft(error) => Some(error),
            Self::Edit(error) => Some(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 首词组与真实首击位置属于同一次草稿输入；只映射坐标，不重新查询或扩大词组。
/// Shift锚点以空范围及同一个首击位置表达，实际角色仍由原输入手势拥有者保持。
pub struct MarkdownCellDraftWordAnchor {
    /// 原词组的真实两端；裁去首尾空白后可以真实收敛为空，不转选相邻内容。
    pub range: MarkdownTextSelection,
    /// 首击落点用于同词内往返拖选的原方向判断，不能用词组中心或后继指针位置替代。
    pub origin_utf16: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 这次原文档提交的真实结果坐标；不是后继Source权限，也不负责移动canonical选区。
/// 调用方须与原Body提交结果一起核对当前代际，不能把旧位置重新授权给较新的正文。
pub struct MarkdownCellDraftCommitReceipt {
    /// 原草稿有向anchor/head经过同次清洗后，在本次canonical正文中的UTF16位置。
    pub selection: MarkdownTextSelection,
    /// 原Word首组两端经过同趟清洗后的canonical范围；Shift锚点以相同的两端表示。
    /// trim后允许真实空组，不从空组重新选择邻词；None表示本次提交没有Word手势。
    pub word_anchor: Option<MarkdownCellDraftWordAnchor>,
    /// 仅真实末格追加才有下一格查询坐标；激活仍必须取得新一代的真实行/格来源票。
    pub next_cell: Option<(usize, usize)>,
}

#[derive(Debug)]
/// 保留原一次事务及未改内容语义，同时交付这次实际清洗的端点，避免关闭草稿后重读整格。
pub struct MarkdownCellDraftCommit<O> {
    /// None不运行观察者、不移动canonical选区，也不制造空Undo；receipt仍来自已封闭的同次草稿。
    pub edit: Option<(MarkdownHistoryTextEdit, O)>,
    pub receipt: MarkdownCellDraftCommitReceipt,
}

/// 失败仍保留原草稿；只有真实publish或unchanged封闭成功才返回结果和原端点收据。
pub type MarkdownCellDraftCommitResult<O> = Result<
    MarkdownCellDraftCommit<<O as MarkdownTextChangeObserver>::Output>,
    MarkdownCellDraftCommitError<<O as MarkdownTextChangeObserver>::Error>,
>;

impl PendingCellDraft {
    fn replacement(
        &self,
        document: &MarkdownDocumentBuffer,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<
        (
            CellReplacementOutput,
            MarkdownTextSelection,
            Option<MarkdownCellDraftWordAnchor>,
        ),
        MarkdownCellDraftError,
    > {
        self.require(document)?;
        let (_, prefix) = self.plan.replacement_for(document)?;
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        let metrics = piece_tree_metrics(&self.root);
        if word_anchor.is_some_and(|anchor| {
            anchor.range.anchor_utf16 > metrics.utf16_len
                || anchor.range.head_utf16 > metrics.utf16_len
                || anchor.origin_utf16 > metrics.utf16_len
        }) {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
        } // 这里只接受原child的真实两端；越界在写候选前拒绝，不钳成邻词或新Source权限。
        let mut sanitized = CellReplacementOutput::new(metrics, prefix)?;
        // 固定保存输入选区两端、首词组两端及首击位置，不建逐字符表，也不再遍历长格。
        // 缺格前缀已经写入；局部0须落在前缀之后，两个端点都保留各自方向。
        let mut selection = MarkdownTextSelection::caret(sanitized.utf16_len());
        let mut mapped_word_anchor = word_anchor.map(|_| MarkdownCellDraftWordAnchor {
            range: selection,
            origin_utf16: sanitized.utf16_len(),
        });
        let mut input_utf16 = 0;
        let mut odd_backslashes = false;
        let mut last_is_space = false;
        let mut started = false;
        // 前缀不参与正文trim；只保最后非空白标量后的计量，长尾空白直接写盘后截去。
        let mut trimmed_end = (sanitized.len(), sanitized.utf16_len());
        let result = visit_piece_tree_range(
            &source,
            self.root.as_deref(),
            0..piece_tree_metrics(&self.root).byte_len,
            0,
            &mut |bytes| {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)?;
                for scalar in text.chars() {
                    // 同趟经过原标量时认证首组两端；代理对内部不是有效端点。
                    // 任一端点无效只丢本候选，原草稿、选择和输入许可仍可继续使用。
                    let next_utf16 = input_utf16 + scalar.len_utf16();
                    if word_anchor.is_some_and(|anchor| {
                        [
                            anchor.range.anchor_utf16,
                            anchor.range.head_utf16,
                            anchor.origin_utf16,
                        ]
                        .into_iter()
                        .any(|point| input_utf16 < point && point < next_utf16)
                    }) {
                        return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
                    }
                    // CR/LF折为空格且连续分隔不重复；反斜线奇偶状态跨真实4KiB叶延续。
                    let needs_space = !last_is_space;
                    let mut emit = |value: char| -> Result<(), MarkdownDocumentBufferError> {
                        last_is_space = value == ' '; // 即使全局前导空白被丢弃，也保留原ends_with空格判定。
                        if !started && value.is_whitespace() {
                            return Ok(()); // 只跳过全局开头，不能分别trim每个piece的内部空白。
                        }
                        started = true;
                        let mut encoded = [0; 4];
                        sanitized.push_text(value.encode_utf8(&mut encoded))?;
                        if !value.is_whitespace() {
                            trimmed_end = (sanitized.len(), sanitized.utf16_len());
                            if trimmed_end.0 > document.maximum_document_bytes {
                                // 已确认会保留的正文超真实文档上限；临时尾空白不误占最终准入。
                                return Err(MarkdownDocumentBufferError::DocumentTooLarge);
                            }
                        }
                        Ok(())
                    };
                    match scalar {
                        '\r' | '\n' => {
                            if needs_space {
                                emit(' ')?;
                            }
                            odd_backslashes = false;
                        }
                        '|' if !odd_backslashes => {
                            emit('\\')?;
                            emit('|')?;
                            odd_backslashes = false;
                        }
                        '\\' => {
                            emit('\\')?;
                            odd_backslashes = !odd_backslashes;
                        }
                        _ => {
                            emit(scalar)?;
                            odd_backslashes = false;
                        }
                    }
                    // 一个原始标量的全部转义或折行完成后才落点；CRLF中间点和合并后的LF末点可重合。
                    // 草稿输入已验证Unicode边界，因此不会把补出的反斜线或代理项中间位置冒充原caret。
                    input_utf16 = next_utf16;
                    if input_utf16 == self.selection.anchor_utf16 {
                        selection.anchor_utf16 = sanitized.utf16_len();
                    }
                    if input_utf16 == self.selection.head_utf16 {
                        selection.head_utf16 = sanitized.utf16_len();
                    }
                    if let Some(original) = word_anchor {
                        let mapped = mapped_word_anchor.as_mut().expect("原首组有同趟映射槽");
                        if original.range.anchor_utf16 == input_utf16 {
                            mapped.range.anchor_utf16 = sanitized.utf16_len();
                        }
                        if original.range.head_utf16 == input_utf16 {
                            mapped.range.head_utf16 = sanitized.utf16_len();
                        }
                        if original.origin_utf16 == input_utf16 {
                            mapped.origin_utf16 = sanitized.utf16_len();
                        }
                    }
                }
                Ok(())
            },
        );
        if let Err(error) = result {
            self.require(document)?; // 原Source首错优先于本次输出容量或候选文件失败。
            return Err(match error {
                MarkdownDocumentBufferError::DocumentTooLarge => {
                    MarkdownCellDraftError::CommitLimit
                }
                other => other.into(),
            });
        }
        // 旧Unicode trim只执行一次；真实匿名文件截尾并定位EOF后，末格追加仍可续写同一输出。
        sanitized.truncate(trimmed_end.0, trimmed_end.1)?;
        // 尾部Unicode空白真正截去后，两端分别钳到保留正文末端；反向选择方向不互换。
        // 绝对坐标在候选阶段检查溢出，发布后不再增加任何可能失败的坐标计算或Source读取。
        selection.anchor_utf16 = self
            .range
            .start
            .checked_add(selection.anchor_utf16.min(trimmed_end.1))
            .ok_or(MarkdownCellDraftError::CommitLimit)?;
        selection.head_utf16 = self
            .range
            .start
            .checked_add(selection.head_utf16.min(trimmed_end.1))
            .ok_or(MarkdownCellDraftError::CommitLimit)?;
        let word_anchor = mapped_word_anchor
            .map(|anchor| {
                Ok::<_, MarkdownCellDraftError>(MarkdownCellDraftWordAnchor {
                    range: MarkdownTextSelection {
                        anchor_utf16: self
                            .range
                            .start
                            .checked_add(anchor.range.anchor_utf16.min(trimmed_end.1))
                            .ok_or(MarkdownCellDraftError::CommitLimit)?,
                        head_utf16: self
                            .range
                            .start
                            .checked_add(anchor.range.head_utf16.min(trimmed_end.1))
                            .ok_or(MarkdownCellDraftError::CommitLimit)?,
                    },
                    origin_utf16: self
                        .range
                        .start
                        .checked_add(anchor.origin_utf16.min(trimmed_end.1))
                        .ok_or(MarkdownCellDraftError::CommitLimit)?,
                })
            })
            .transpose()?; // 空白首组可能收敛为空；保住这个真实结果，不能改选相邻内容。
        self.require(document)?;
        Ok((sanitized, selection, word_anchor))
    }

    fn unchanged(
        &self,
        document: &MarkdownDocumentBuffer,
        replacement: &CellReplacement,
    ) -> Result<bool, MarkdownCellDraftError> {
        let start = document.byte_offset_for_utf16(self.range.start)?;
        let end = document.byte_offset_for_utf16(self.range.end)?;
        if end - start != replacement.len() {
            return Ok(false);
        }
        let mut cursor = 0;
        let mut same = true;
        visit_piece_tree_range(
            document,
            document.root.as_deref(),
            start..end,
            0,
            &mut |bytes| {
                // 继续完成原范围读取，真实I/O失败不能被提前发现的文本差异遮住。
                same &= replacement.matches_bytes(cursor, bytes)?;
                cursor += bytes.len();
                Ok(())
            },
        )?;
        self.require(document)?;
        Ok(same && cursor == replacement.len())
    }
}

impl MarkdownDocumentBuffer {
    /// 消费原输入胶囊预备有限sanitize；允许两个真实Scene reader继续持有原内容。
    /// marked必须先沿原输入法协议结束；任何失败不改草稿、选择、历史、读取名额或canonical。
    pub fn prepare_cell_draft_commit(
        &self,
        edit: super::edit::MarkdownCellDraftEditCapsule,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        if pending.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive);
        }
        self.prepare_cell_draft_commit_inner(pending, word_anchor)
    }

    fn prepare_cell_draft_commit_inner(
        &self,
        pending: &PendingCellDraft,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        // 名额先于任何String分配取得；后续失败的局部String先Drop，claim才实际开放下一张票。
        let mut claim = pending.scope.reserve_commit()?;
        let candidate =
            pending
                .replacement(self, word_anchor)
                .and_then(|(output, selection, word_anchor)| {
                    let (replacement, units) = output.seal(&mut claim)?;
                    Ok((replacement, units, selection, word_anchor))
                });
        let (replacement, replacement_utf16, selection, word_anchor) = match candidate {
            Ok(value) => value,
            Err(error) => {
                pending.require(self)?; // 匿名候选I/O失败也不能遮住原Source同时到达的真实首因。
                return Err(error);
            }
        };
        claim.allocated(replacement.capacity());
        let unchanged = pending.unchanged(self, &replacement)?;
        pending.require(self)?;
        Ok(MarkdownCellDraftPreparedCommit {
            scope: Arc::downgrade(&pending.scope),
            serial: pending.serial,
            range: pending.range.clone(),
            replacement,
            replacement_utf16,
            selection,
            word_anchor,
            unchanged,
            next_cell: None,
            _claim: claim,
        })
    }

    /// 将原末格草稿与真实表尾追加组合成同一准入票；尚存Scene reader不阻止有限准备。
    /// marked、原输入serial、同Doc来源与真实VerifiedTableEnd必须同时有效；失败保留原raw草稿。
    pub fn prepare_cell_draft_row_append(
        &self,
        edit: super::edit::MarkdownCellDraftEditCapsule,
        terminal: &VerifiedMarkdownTableRowSource,
        authority: MarkdownViewportSourceAuthority,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        if pending.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive);
        }
        let prepared = (|| {
            let end = terminal
                .terminal_end_for(self, authority)
                .map_err(|_| MarkdownCellDraftError::Stale)?
                .ok_or(MarkdownTableWindowError::ContextPending)?;
            let (header, row, columns) = terminal
                .coordinate_for(self, authority)
                .map_err(|_| MarkdownCellDraftError::Stale)?;
            let context = &pending.plan.cell.context;
            if header != context.header.start
                || row != context.row_ordinal
                || columns != context.columns
            {
                // 原 End 属于另一真实行/表时不借给当前草稿；同长范围、同一插入锚点也不够。
                return Err(MarkdownTableWindowError::InvalidSource.into());
            }
            // 唯一名额先于任何有限补格或sanitize String分配；Busy不额外制造候选副本。
            let claim = pending.scope.reserve_commit()?;
            // 当前 raw 端点及补列事实来自激活时的原票；只补本次真实 End，不回读巨格或重建完整窗口。
            let mut cell = pending.plan.cell.clone();
            cell.context.end = Some(end.clone());
            let target = self.plan_table_cell_source_replacement(cell, "")?;
            self.prepare_cell_draft_append_inner(pending, &target, claim, word_anchor)
        })();
        if prepared.is_err() {
            // 表格源适配器返回Table错误时仍核原健康门；真实reclaim/read首错不能降成无效坐标。
            pending.require(self)?;
        }
        prepared
    }

    /// 原有长格在激活时已取得同源End；末格追加消费该原plan，不要求整行预览Ready。
    pub fn prepare_cell_draft_original_row_append(
        &self,
        edit: super::edit::MarkdownCellDraftEditCapsule,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        if pending.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive);
        }
        let claim = pending.scope.reserve_commit()?;
        let prepared =
            self.prepare_cell_draft_append_inner(pending, &pending.plan, claim, word_anchor);
        if prepared.is_err() {
            pending.require(self)?;
        }
        prepared
    }

    /// 两个入口只区别真实 End 来自后继 terminal 还是原格计划；sanitize、尾部和唯一 publish 共用。
    fn prepare_cell_draft_append_inner(
        &self,
        pending: &PendingCellDraft,
        target: &MarkdownTableCellEditPlan,
        mut claim: CommitClaim,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        let (target_range, target_prefix) = target.replacement_for(self)?;
        let (original_range, original_prefix) = pending.plan.replacement_for(self)?;
        if target_range != original_range || target_prefix != original_prefix {
            return Err(MarkdownTableWindowError::InvalidSource.into());
        }
        // 同一规则清洗后仍不seal；真实尾部和新行直接续写，长cell不会为Tab再复制一份。
        let (mut replacement, selection, word_anchor) = pending.replacement(self, word_anchor)?;
        let tail = self.verified_cell_row_append_tail_for_plan(target)?;
        let next_cell = tail.next_cell();
        let range = tail.replacement_range_for(self)?.clone();
        if replacement
            .len()
            .checked_add(tail.byte_len())
            .is_none_or(|bytes| bytes > self.maximum_document_bytes)
        {
            return Err(MarkdownDocumentBufferError::DocumentTooLarge.into()); // 已知真实输出超过live上限，不先写巨型表尾。
        }
        replacement.prepare_suffix(tail.byte_len())?;
        tail.write_to(self, &mut |bytes| replacement.push_bytes(bytes))?;
        let (replacement, replacement_utf16) = replacement.seal(&mut claim)?;
        claim.allocated(replacement.capacity());
        pending.require(self)?;
        Ok(MarkdownCellDraftPreparedCommit {
            scope: Arc::downgrade(&pending.scope),
            serial: pending.serial,
            range,
            replacement,
            replacement_utf16,
            selection, // 原草稿端点在真实追加尾部前已经封定，不能被新行字节移到下一格。
            word_anchor,
            unchanged: false,
            next_cell: Some(next_cell),
            _claim: claim,
        })
    }

    /// 原nonce、表格源与所有旧读取payload归还后，只提交最终sanitize replacement一次。
    /// 兼容入口仍先拒绝Reader；保留原marked行为，只有新prepare要求先完成输入法组合。
    /// observer仅可执行纯Rust；任何失败恢复同一草稿，不发布root/g/选区/history。
    pub fn commit_cell_draft_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        token: &MarkdownCellDraft,
        observer: O,
    ) -> MarkdownCellDraftCommitResult<O> {
        let pending = self.active_cell_draft(token)?;
        if pending.scope.has_readers() {
            return Err(MarkdownCellDraftError::ReadersBusy.into());
        }
        let prepared = self.prepare_cell_draft_commit_inner(pending, None)?;
        self.commit_prepared_cell_draft_observed(prepared, observer)
    }

    /// 消费同一原准入票，只在两个Scene reader实际归还后执行原一步canonical事务。
    /// 不重sanitize或重捕后继输入；外来、旧serial、Source失败和Reader忙均保留原草稿。
    pub fn commit_prepared_cell_draft_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        mut prepared: MarkdownCellDraftPreparedCommit,
        observer: O,
    ) -> MarkdownCellDraftCommitResult<O> {
        let pending = self.active_cell_draft_scope(&prepared.scope)?;
        if pending.serial != prepared.serial {
            return Err(MarkdownCellDraftError::Stale.into());
        }
        if pending.scope.has_readers() {
            return Err(MarkdownCellDraftError::ReadersBusy.into());
        }
        if let Err(error) = prepared.replacement.health() {
            pending.require(self)?; // 原Source首错优先；候选损坏不能取消仍可编辑的原raw分支。
            return Err(MarkdownCellDraftError::from(error).into());
        }
        let Some(CellDraftSlot::Active(pending)) = self.cell_draft.take() else {
            unreachable!("已核唯一活动草稿");
        };
        if prepared.unchanged {
            // 与原字段关闭未改草稿一致；不调用观察者，不产生空变化日志或Undo。
            pending.scope.cancelled.store(true, Ordering::Release);
            drop(pending);
            return Ok(MarkdownCellDraftCommit {
                edit: None,
                receipt: MarkdownCellDraftCommitReceipt {
                    selection: prepared.selection,
                    word_anchor: prepared.word_anchor,
                    next_cell: prepared.next_cell,
                },
            });
        }
        let result = (|| {
            let after = prepared
                .range
                .start
                .checked_add(prepared.replacement_utf16)
                .ok_or_else(|| {
                    MarkdownCellDraftCommitError::Draft(
                        MarkdownDocumentBufferError::DocumentTooLarge.into(),
                    )
                })?;
            let input = self.prepare_text_input_replacement_observed(
                prepared.range.clone(),
                prepared.replacement.input(prepared.replacement_utf16),
                self.selection,
                MarkdownTextSelection::caret(after),
                observer,
            );
            let (input, output) = match input {
                Ok(value) => value,
                Err(error) => {
                    pending.require(self)?; // 事务准备失败后原Source首因仍以原Draft类别报告。
                    return Err(MarkdownCellDraftCommitError::Edit(error));
                }
            };
            // observer可能让同共享Source出现sticky错误；仍在原publish前再次认证草稿来源。
            if let Err(error) = pending.require(self) {
                self.cancel_prepared_text_input(&input)
                    .map_err(|error| MarkdownCellDraftCommitError::Draft(error.into()))?;
                return Err(error.into());
            }
            match self.publish_text_input(&input) {
                Ok(edit) => {
                    if matches!(&prepared.replacement, CellReplacement::Frozen(_)) {
                        // 同一实际文件已成为已发布段；从此由Doc iterator覆盖历史及最终释放。
                        // Inline的临时映射已在seal真实释放，不能在这里冒充转归。
                        prepared._claim.release_backing_ticket();
                    }
                    Ok((edit, output))
                }
                Err(error) => {
                    // 真实候选尚未publish；原cancel先Drop其root，再回退canonical append和日志。
                    self.cancel_prepared_text_input(&input)
                        .map_err(|cleanup| MarkdownCellDraftCommitError::Draft(cleanup.into()))?;
                    Err(MarkdownCellDraftCommitError::Draft(error.into()))
                }
            }
        })();
        match result {
            Ok(value) => {
                pending.scope.cancelled.store(true, Ordering::Release);
                // canonical只保最终替换段；原raw branch真实Drop，不把2MiB垃圾段移入会话库存。
                drop(pending);
                Ok(MarkdownCellDraftCommit {
                    edit: Some(value),
                    receipt: MarkdownCellDraftCommitReceipt {
                        selection: prepared.selection,
                        word_anchor: prepared.word_anchor,
                        next_cell: prepared.next_cell,
                    },
                })
            }
            Err(error) => {
                self.cell_draft = Some(CellDraftSlot::Active(pending));
                Err(error)
            }
        }
    }
}
