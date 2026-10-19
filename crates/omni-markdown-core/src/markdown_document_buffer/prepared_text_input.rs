//! 在原生输入进入super之前完成所有可失败准备；primitive仅消费同一文档的唯一提交权限。

use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};

use super::changes::{MarkdownObservedEditError, MarkdownTextChangeObserver};
use super::command_transaction::{self, PreparedCommand, SealedCommand};
use super::*;

mod composition;
pub(super) use composition::CompositionGroup;
pub use composition::MarkdownTextInputComposition;

/// 同一事务的两种真实输入存储；封存文件只能由Core准备者持有，不增加公开裸范围提交协议。
pub(super) enum TextInputReplacement<'a> {
    Text(&'a str),
    Frozen {
        backing: Arc<OriginalBacking>,
        utf16: usize,
    },
}

impl TextInputReplacement<'_> {
    /// 两条入口都用实际extent计算同一个结果文档与会话append预算。
    fn byte_len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Frozen { backing, .. } => backing.len(),
        }
    }

    /// 文件输入的UTF16计量来自同一次标量清洗；没有整文件解码String。
    fn utf16_len(&self) -> usize {
        match self {
            Self::Text(text) => text.encode_utf16().count(),
            Self::Frozen { utf16, .. } => *utf16,
        }
    }
}

#[derive(Debug)]
struct InputNonce {
    cancelled: AtomicBool,
}

#[derive(Debug)]
/// 唯一输入票据不保活文档、正文树、mmap或日志；丢弃只标记原候选取消，不重入文档。
///
/// 调用方在正常的无借用出口显式cancel；异常丢弃后须drain，才能声称候选资源已释放。
pub struct MarkdownPreparedTextInput {
    nonce: Weak<InputNonce>,
}

impl Drop for MarkdownPreparedTextInput {
    fn drop(&mut self) {
        if let Some(nonce) = self.nonce.upgrade() {
            // 仅原nonce可被标记；已发布或已替换的票据不能撤销后继候选。
            nonce.cancelled.store(true, Ordering::Release);
        }
    }
}

/// 空替换只准备选区，不伪造正文日志、generation或history。
enum PendingCommand {
    Characters(SealedCommand),
    SelectionOnly {
        after: MarkdownTextSelection,
        metrics: MarkdownReplacementMetrics,
    },
}

/// 所有资源都属于唯一document；token只有Weak，空文档也通过独立nonce隔离。
pub(super) struct PendingTextInput {
    nonce: Arc<InputNonce>,
    original_root: PieceTreeLink,
    original_generation: u64,
    original_selection: MarkdownTextSelection,
    command: PendingCommand,
    composition: Option<composition::CompositionPlan>,
}

impl PendingTextInput {
    fn matches(&self, token: &MarkdownPreparedTextInput) -> bool {
        // Weak::ptr_eq不升级正文所有权，也不会把两个空root当作同一文档。
        Weak::ptr_eq(&token.nonce, &Arc::downgrade(&self.nonce))
    }

    fn discard(self, document: &mut MarkdownDocumentBuffer) {
        if let PendingCommand::Characters(prepared) = self.command {
            // 候选可能引用新append尾；先结束其所有tree引用，再恢复追加边界并释放封存日志。
            let SealedCommand {
                command,
                checkpoint,
                changes,
                ..
            } = prepared;
            drop(command);
            checkpoint.rollback(document);
            document.change_journal.release(changes);
        }
        // composition的有限Heap COW尚未安装；随计划Drop归还，原checkpoint仍只回退真实append。
        // 仅选区的候选没有正文或日志资源；nonce随当前唯一slot一起结束。
    }
}

impl MarkdownDocumentBuffer {
    /// 读取只见旧canonical；写入口须先拒绝未消费候选，取消标记也不能当成已释放资源。
    pub(crate) fn ensure_text_input_idle(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        #[cfg(feature = "s2-table-window-prototype")]
        if self.cell_draft.is_some() {
            // 草稿虽不发布正文，也独占原canonical编辑基线；取消后仍活的读取根不能提前腾位。
            return Err(MarkdownDocumentBufferError::TextInputPrepared);
        }
        if self.prepared_text_input.is_some() {
            return Err(MarkdownDocumentBufferError::TextInputPrepared);
        }
        Ok(())
    }

    /// 报告文档是否仍实际拥有候选；包括token已丢弃但尚未显式排空的情况。
    #[must_use]
    pub fn has_prepared_text_input(&self) -> bool {
        self.prepared_text_input.is_some()
    }

    /// 排空被Drop标记的原候选，不运行观察者、追加或可失败日志I/O。
    ///
    /// 返回true仅表示本次确实释放了一项取消候选；仍活票据的候选保持不变。
    pub fn drain_cancelled_text_input(&mut self) -> bool {
        if !self
            .prepared_text_input
            .as_ref()
            .is_some_and(|pending| pending.nonce.cancelled.load(Ordering::Acquire))
        {
            // 没有候选或票据仍活时不能猜测取消，也不能清空合法输入。
            return false;
        }
        let pending = self
            .prepared_text_input
            .take()
            .expect("the cancellation check observed the unique pending slot");
        pending.discard(self);
        true
    }

    /// 仅取消同一nonce；外来或旧票据不能截断另一个候选的append或释放其日志。
    ///
    /// # Errors
    /// 当前没有匹配候选时返回权限失效；该拒绝不改动任何文档状态。
    pub fn cancel_prepared_text_input(
        &mut self,
        token: &MarkdownPreparedTextInput,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if !self
            .prepared_text_input
            .as_ref()
            .is_some_and(|pending| pending.matches(token))
        {
            // 必须在take之前核验，不能用错误票据顺手排空原候选或后继候选。
            return Err(MarkdownDocumentBufferError::PreparedTextInputStale);
        }
        let pending = self
            .prepared_text_input
            .take()
            .expect("the identity check observed the unique pending slot");
        pending.discard(self);
        Ok(())
    }

    /// 先校验Unicode、实际容量、append预算、派生观察结果与变化日志，再返回不可见候选。
    ///
    /// token与Output由调用方一起保存；Output不进入文档，正文仍只归唯一pending所有。
    /// 所有外部回调必须为纯Rust。此函数返回后、publish前，canonical root/g/选区/history
    /// 不变，普通读取也不暴露候选；禁止用临时root交换验证候选。
    ///
    /// # Errors
    /// 范围、容量、观察者或日志失败沿共享事务回滚；若日志清理也失败保留双重typed原因。
    pub fn prepare_text_input_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        observer: O,
    ) -> Result<(MarkdownPreparedTextInput, O::Output), MarkdownObservedEditError<O::Error>> {
        self.prepare_text_input_replacement_observed(
            range,
            TextInputReplacement::Text(replacement),
            selection_before,
            selection_after,
            observer,
        )
    }

    /// 原字符串输入与封存Cell结果共用唯一pending、日志、候选root和发布；只在读取存储处区分。
    pub(super) fn prepare_text_input_replacement_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: TextInputReplacement<'_>,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        observer: O,
    ) -> Result<(MarkdownPreparedTextInput, O::Output), MarkdownObservedEditError<O::Error>> {
        self.prepare_text_input_replacement_composition_observed(
            range,
            replacement,
            selection_before,
            selection_after,
            None,
            observer,
        )
    }

    /// 普通str、Frozen及真实组合共用一次长度计量与候选准备；Option只携本次IME归属。
    fn prepare_text_input_replacement_composition_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: TextInputReplacement<'_>,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        composition: Option<MarkdownTextInputComposition>,
        observer: O,
    ) -> Result<(MarkdownPreparedTextInput, O::Output), MarkdownObservedEditError<O::Error>> {
        self.ensure_text_input_idle()?;
        if range.start > range.end || range.end > self.utf16_len() {
            // 倒置或越界先拒绝，不能让长度差或后续split掩盖无效坐标。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
        }
        self.validate_selection(selection_before)?;
        let byte_start = self.byte_offset_for_utf16(range.start)?;
        let byte_end = self.byte_offset_for_utf16(range.end)?;
        let inserted = replacement.utf16_len();
        let metrics = MarkdownReplacementMetrics::new(range.start, range.len(), inserted)?;
        let result_units = self
            .utf16_len()
            .checked_sub(range.len())
            .and_then(|length| length.checked_add(inserted))
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        self.validate_selection_for_result(selection_after, result_units)?;
        if let TextInputReplacement::Text(text) = &replacement {
            // 显式str沿原前置标量校验；封存文件的两个端点在原候选树上按有限piece校验。
            self.validate_post_edit_utf16_boundary(
                selection_after.anchor_utf16,
                &range,
                text,
                inserted,
            )?;
            self.validate_post_edit_utf16_boundary(
                selection_after.head_utf16,
                &range,
                text,
                inserted,
            )?;
        }
        let mut composition = composition
            .map(|composition| self.plan_composition::<O::Error>(&range, inserted, composition))
            .transpose()?; // 同一次真实UTF16计量供净日志使用，不为组合再扫描一遍新增输入。

        let (command, output) = if range.is_empty() && replacement.byte_len() == 0 {
            // 真正无正文变化仍让观察者校验显式head，但prepare绝不提前移动canonical选区。
            let output = observer
                .finish(
                    changes::MarkdownPreparedText {
                        document: self,
                        root: &self.root,
                    },
                    selection_after,
                )
                .map_err(MarkdownObservedEditError::Observer)?;
            (
                PendingCommand::SelectionOnly {
                    after: selection_after,
                    metrics,
                },
                output,
            )
        } else {
            let result_bytes = self
                .byte_len()
                .checked_sub(byte_end - byte_start)
                .and_then(|length| length.checked_add(replacement.byte_len()))
                .filter(|length| *length <= self.maximum_document_bytes)
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
            // 容量不足在进入super前失败；满history会先弹旧项，已有容量无需再扩大。
            if self.undo.len() < MAX_EDIT_HISTORY_ENTRIES {
                self.undo
                    .try_reserve(1)
                    .map_err(|_| MarkdownDocumentBufferError::EditHistoryUnavailable)?;
            }
            let (mut prepared, output) = command_transaction::prepare_command_observed(
                self,
                selection_before,
                observer,
                |document, observer| {
                    // 使用常规事务同一个真实变化流；观察者拒绝先于正文append。
                    changes::emit(document, observer, range, inserted)?;
                    let inserted = match replacement {
                        TextInputReplacement::Text(text) => {
                            if let Some(plan) = &mut composition {
                                // 与候选叶同次取得真实独占分配票；外部范围或长度本身不授予退休权。
                                plan.append(
                                    &mut document.edits,
                                    text,
                                    document.maximum_document_bytes,
                                )?
                            } else {
                                document
                                    .edits
                                    .append(text, document.maximum_document_bytes)?
                            }
                        }
                        TextInputReplacement::Frozen { backing, .. } => {
                            // 只接管同一个封存inode；4KiB叶计量失败仍在原checkpoint回滚范围内。
                            document
                                .edits
                                .append_frozen(backing, document.maximum_document_bytes)?
                        }
                    };
                    let inserted = build_balanced_piece_tree(inserted);
                    let (prefix, remainder) =
                        split_piece_tree(document, document.root.clone(), byte_start)?;
                    let (_, suffix) = split_piece_tree(document, remainder, byte_end - byte_start)?;
                    if let Some(plan) = &mut composition {
                        plan.prepare_rebased_root(&prefix, &suffix);
                    }
                    Ok(PreparedCommand {
                        root: join_piece_trees(prefix, join_piece_trees(inserted, suffix)),
                        after: selection_after,
                        metrics,
                    })
                },
            )?;
            if let Some(plan) = &mut composition {
                // 原候选所有真实读取均已结束；只替换同指标root，配套COW字节仍等publish才安装。
                plan.finish_prepared(&mut prepared);
            }
            debug_assert_eq!(
                piece_tree_metrics(&prepared.command.root).byte_len,
                result_bytes
            );
            (PendingCommand::Characters(prepared), output)
        };

        // 只在全部可失败阶段成功后占用唯一slot；nonce独立于root，两个空文档也不能互认。
        let nonce = Arc::new(InputNonce {
            cancelled: AtomicBool::new(false),
        });
        let token = MarkdownPreparedTextInput {
            nonce: Arc::downgrade(&nonce),
        };
        self.prepared_text_input = Some(PendingTextInput {
            nonce,
            original_root: self.root.clone(),
            original_generation: self.generation,
            original_selection: self.selection,
            command,
            composition,
        });
        Ok((token, output))
    }

    /// 消费原nonce和原canonical完整权限后一次发布；校验后不调用Native或观察者。
    /// 组合旧payload在发布后才退休；其真实清理错误留在原backing票，不能作为未提交Err返回。
    ///
    /// # Errors
    /// 外来、已消费、已取消票据或canonical权限变化均拒绝，保持当前候选供其正确owner处理。
    pub fn publish_text_input(
        &mut self,
        token: &MarkdownPreparedTextInput,
    ) -> Result<MarkdownHistoryTextEdit, MarkdownDocumentBufferError> {
        // prepare与publish之间外部冻结reader可能已发现真实回收失败；纯原子门保留原errno。
        // 失败不take唯一候选，不签发新root，也不剥夺原nonce的显式取消/资源回滚权限。
        self.original_backing_health()?;
        let valid = self.prepared_text_input.as_ref().is_some_and(|pending| {
            pending.matches(token)
                && !pending.nonce.cancelled.load(Ordering::Acquire)
                && pending.original_generation == self.generation
                && pending.original_selection == self.selection
                && piece_tree_links_share_root(&pending.original_root, &self.root)
        });
        if !valid {
            // 不take错误slot，尤其不能用过期票据回滚后继已准备的append。
            return Err(MarkdownDocumentBufferError::PreparedTextInputStale);
        }
        self.edits.pending_health()?; // 票据仍独占时核候选真实文件；拒绝不take，原owner仍可显式取消。
        let pending = self
            .prepared_text_input
            .take()
            .expect("the authority check observed the unique pending slot");
        let edit = match pending.command {
            PendingCommand::Characters(prepared) => {
                // history容量已预留；共享发布阶段只移动root、日志票据和固定预算历史。
                if let Some(plan) = pending.composition {
                    composition::publish(self, prepared, plan)
                } else {
                    command_transaction::publish_command(self, prepared)
                }
            }
            PendingCommand::SelectionOnly { after, metrics } => {
                // 保留原兼容合同：仅选区变化不增加g、不清redo，也不产生一条空history。
                self.selection = after;
                composition::publish_selection(self, pending.composition, after);
                metrics.redo_text_edit(after)
            }
        };
        Ok(edit)
    }
}

#[cfg(test)]
mod tests;
