//! 只承认当前表格字段的真实输入；同步unmark/validate只豁免具名、有限且逐UTF16相同的通知。

use super::*;
use objc2_foundation::NSCopying;

// 上限沿既有单格合同按UTF-16单元计数；不是完整正文或64KiB驻留内存的声明。
const MAX_FIELD_UTF16_UNITS: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 只允许同步具名调用保留当前Tab；其他输入事件继续撤销旧导航权限。
pub(in crate::native_surface) enum NativeMarkdownTableFieldOperation {
    /// 输入法可结束原组合，但不能更改字段的任何UTF-16单元。
    Unmark,
    /// 字段校验只接受调用前后均无组合输入的相同值。
    Validate,
}

fn expected_mark_transition(
    operation: NativeMarkdownTableFieldOperation,
    before: Option<NSRange>,
    after: Option<NSRange>,
    same_text: bool,
) -> bool {
    if !same_text {
        return false;
    } // 字面正文哪怕只改一unit，也不是旧Tab的同步确认权限。
    match operation {
        // unmark只可保留原marked事实或正常结束；新位置、新长度均属于新的物理输入。
        NativeMarkdownTableFieldOperation::Unmark => after == before || after.is_none(),
        // validate不能偷偷接管任何组合，即使marked范围真实存在而长度为零。
        NativeMarkdownTableFieldOperation::Validate => before.is_none() && after.is_none(),
    }
}

/// 此值仅在一个明确Native调用栈上保留有限字段不可变值，不持canonical文档或跨事件缓存。
pub(in crate::native_surface) struct NativeMarkdownTableFieldExpectation {
    input_epoch: u64,
    coordinate: NativeMarkdownTableCellCoordinate,
    field_editor: Weak<NSTextView>,
    text: Retained<NSString>,
    marked: Option<NSRange>,
    operation: NativeMarkdownTableFieldOperation,
}

struct ExpectationScope<'a> {
    view: &'a PieceBackedMarkdownTextView,
    expected: Rc<NativeMarkdownTableFieldExpectation>,
}
impl Drop for ExpectationScope<'_> {
    fn drop(&mut self) {
        // 只移除本scope发布的Rc；重入产生的后继期望由其自身调用栈持有和清理。
        let previous = {
            let mut slot = self.view.ivars().table_field_expectation.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|value| Rc::ptr_eq(value, &self.expected))
            {
                slot.take()
            } else {
                // 关闭或后继scope已更换槽，旧析构不能撤掉它们的新权限。
                None
            }
        };
        drop(previous); // 最后NSString/NativeWeak释放不跨RefMut，外层PayloadUseGuard仍有效。
    }
}

struct NotificationCheck<'a>(&'a Cell<bool>);
impl Drop for NotificationCheck<'_> {
    fn drop(&mut self) {
        // 仅最外层比较者持有此guard；正常返回和提前失败均归还有限同步检查权。
        self.0.set(false);
    }
}

impl PieceBackedMarkdownTextView {
    /// 只按仍活的Rc字段身份核当前格；不调用Native getter或推断其他窗口的共享编辑器。
    pub(in crate::native_surface) fn owns_table_field(&self, field: &NSTextField) -> bool {
        !self.ivars().textkit_graph_teardown.get()
            && self.ivars().active_table_cell.borrow().is_some()
            && self
                .ivars()
                .cell_editor
                .borrow()
                .as_ref()
                .is_some_and(|owner| std::ptr::eq(&***owner, field))
    }

    /// 所有Native步骤前后复用原epoch，不在getter返回后领取后来输入的代次。
    fn owned_field_notification_current(&self, field: &NSTextField, epoch: u64) -> bool {
        self.ivars().table_input_intent_epoch.get() == epoch && self.owns_table_field(field)
    }

    /// hasMarkedText和markedRange都来自同一实际editor，并在每次调用后重核原输入权限。
    fn actual_table_field_mark(
        editor: &NSTextView,
        is_current: &impl Fn() -> bool,
    ) -> Result<Option<NSRange>, NativeMarkdownStableTableFailure> {
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(editor);
        let has_marked = native_markdown_table_intent_step(is_current, || input.hasMarkedText())?;
        let range = native_markdown_table_intent_step(is_current, || input.markedRange())?;
        if has_marked {
            // 无法表示的组合范围不能被当成已结束；保持原字段和旧草稿而拒绝提交。
            if range.location == NSNotFound as NSUInteger
                || range.location.checked_add(range.length).is_none()
            {
                return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
            }
            Ok(Some(range)) // 包括真实零长度marked范围；不能只看length把组合误认为结束。
        } else {
            // 无marked事实时才使用None；不让遗留range数值制造新的组合权限。
            Ok(None)
        }
    }

    /// 先按实际currentEditor长度准入，再copy成不可变值；超量草稿留在原字段，不临时镜像它。
    fn bounded_table_field_text(
        editor: &NSTextView,
        is_current: &impl Fn() -> bool,
    ) -> Result<Retained<NSString>, NativeMarkdownStableTableFailure> {
        let source = native_markdown_table_intent_step(is_current, || editor.string())?;
        let length = native_markdown_table_intent_step(is_current, || source.length())?;
        if length > MAX_FIELD_UTF16_UNITS {
            // 先准入再copy；超量时仍留在原Native字段，不能为确认操作多造一份大草稿。
            return Err(NativeMarkdownStableTableFailure::CoverageOutsideBudget);
        }
        let value = native_markdown_table_intent_step(is_current, || source.copy())?;
        if native_markdown_table_intent_step(is_current, || value.length())? != length {
            // copy期间的重入或不稳定结果不是当前字段快照，拒绝用于通知豁免。
            return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
        }
        Ok(value)
    }

    fn table_field_values_equal(
        left: &NSString,
        right: &NSString,
        is_current: &impl Fn() -> bool,
    ) -> Result<bool, NativeMarkdownStableTableFailure> {
        let length = native_markdown_table_intent_step(is_current, || left.length())?;
        let right_length = native_markdown_table_intent_step(is_current, || right.length())?;
        if length != right_length || length > MAX_FIELD_UTF16_UNITS {
            // 长度变化或超预算都足以证明不能继承旧Tab，无需复制后面的单元。
            return Ok(false);
        }
        let mut left_units = [0_u16; 256];
        let mut right_units = [0_u16; 256];
        for start in (0..length).step_by(left_units.len()) {
            // 两边均为本次有限不可变值；逐unit比较，不使用可能做Unicode等价比较的字符串API。
            let count = (length - start).min(left_units.len());
            let range = NSRange::new(start, count);
            native_markdown_table_intent_step(is_current, || {
                // SAFETY: 两个固定缓冲均容纳count≤256个unit，range已在已确认长度内。
                unsafe {
                    left.getCharacters_range(NonNull::new(left_units.as_mut_ptr()).unwrap(), range);
                }
            })?;
            native_markdown_table_intent_step(is_current, || {
                // SAFETY: right也已确认同长且不可变，缓冲足够写入相同有限range。
                unsafe {
                    right.getCharacters_range(
                        NonNull::new(right_units.as_mut_ptr()).unwrap(),
                        range,
                    );
                }
            })?;
            if left_units[..count] != right_units[..count] {
                // 首个不同unit就撤权，不能把规范等价的Unicode文本误当作未发生编辑。
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// 用一次具名操作的实际输入快照包围unmark/validate；scope销毁并重核之后才能继续提交。
    pub(in crate::native_surface) fn expected_table_field_operation(
        &self,
        field: &NSTextField,
        editor: &Retained<NSTextView>,
        operation: NativeMarkdownTableFieldOperation,
        is_current: &impl Fn() -> bool,
        invoke: impl FnOnce(),
    ) -> Result<(), String> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let epoch = self.ivars().table_input_intent_epoch.get();
        let current = || is_current() && self.owned_field_notification_current(field, epoch);
        let error = || "表格字段输入已经改变或超过单格预算，草稿将保持打开".to_owned();
        if !current() || self.ivars().table_field_expectation.borrow().is_some() {
            // 旧字段、关闭状态或嵌套具名操作均无新期望权限，也不清除现有输入。
            return Err(error());
        }
        let actual = native_markdown_table_intent_step(&current, || field.currentEditor())
            .map_err(|_| error())?
            .and_then(|value| value.downcast::<NSTextView>().ok())
            .ok_or_else(error)?;
        if !std::ptr::eq(&*actual, &**editor) {
            // 同一个字段也可能已换field editor；调用者的旧实例不能确认新输入。
            return Err(error());
        }
        drop(actual); // 临时Native claim先结束，后续快照调用仍逐步复核原权限。
        let text = Self::bounded_table_field_text(editor, &current).map_err(|_| error())?;
        let marked = Self::actual_table_field_mark(editor, &current).map_err(|_| error())?;
        let text_length =
            native_markdown_table_intent_step(&current, || text.length()).map_err(|_| error())?;
        if marked.is_some_and(|range| range.location.saturating_add(range.length) > text_length) {
            return Err(error()); // 不接受超出实际不可变字段值的marked坐标。
        }
        if operation == NativeMarkdownTableFieldOperation::Validate && marked.is_some() {
            return Err("表格单元格的输入尚未完成，草稿将保持打开".into());
        }
        let native_editor =
            native_markdown_table_intent_step(&current, || Weak::from_retained(editor))
                .map_err(|_| error())?;
        // 纯Rust坐标在NativeWeak构造返回之后读取；Ref只覆盖这次Copy投影。
        let coordinate = self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .map(|active| active.coordinate)
            .ok_or_else(error)?;
        let expected = Rc::new(NativeMarkdownTableFieldExpectation {
            input_epoch: epoch,
            coordinate,
            field_editor: native_editor,
            text,
            marked,
            operation,
        });
        if !current() || self.ivars().table_field_expectation.borrow().is_some() {
            // 快照及weak构造可能重入；发布前仍必须拥有原字段和空期望槽。
            return Err(error());
        }
        *self.ivars().table_field_expectation.borrow_mut() = Some(Rc::clone(&expected));
        let scope = ExpectationScope {
            view: self,
            expected,
        };
        let result = native_markdown_table_intent_step(&current, invoke).map_err(|_| error());
        drop(scope); // 临时Native值/weak释放可能重入，结束全部局部期望后再核权限。
        if !current() {
            // 具名操作或局部资源析构已触发新输入，旧Tab即使Native调用返回也不得继续。
            return Err(error());
        }
        result
    }

    /// 外来字段/迟到通知没有撤权资格；本字段新文本或marked变化必须同步撤销旧Tab。
    pub(in crate::native_surface) fn table_field_did_change_input(
        &self,
        notification: &NSNotification,
    ) -> bool {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let epoch = self.ivars().table_input_intent_epoch.get();
        let Some(owner) = self.ivars().cell_editor.borrow().as_ref().map(Rc::clone) else {
            return false; // 没有项目字段时不把普通正文/Find通知归为表格输入。
        };
        let field = notification
            .object()
            .and_then(|value| value.downcast::<NSTextField>().ok());
        let Some(field) = field else {
            return false; // 外来notification对象没有撤销此表格导航的权限。
        };
        if !std::ptr::eq(&**owner, &*field) || !self.owned_field_notification_current(&field, epoch)
        {
            return false; // 后继格或旧field的通知不能污染当前格的物理输入代次。
        }
        let current = || self.owned_field_notification_current(&field, epoch);
        let expected = self
            .ivars()
            .table_field_expectation
            .borrow()
            .as_ref()
            .map(Rc::clone);
        let allowed = if self.ivars().table_field_notification_checking.replace(true) {
            false // 比较getter内再有真实输入时保守撤权，不递归比较或吞掉新事件。
        } else {
            // 只有本层首次取得检查权才创建guard；嵌套拒绝不能提前复位外层Busy。
            let _checking = NotificationCheck(&self.ivars().table_field_notification_checking);
            (|| {
                let expected = expected.as_ref()?;
                if expected.input_epoch != epoch
                    || self
                        .ivars()
                        .active_table_cell
                        .borrow()
                        .as_ref()
                        .is_none_or(|active| active.coordinate != expected.coordinate)
                {
                    return None; // 坐标/epoch变化属于新意图，不以相同字符串值恢复旧导航。
                }
                let actual = native_markdown_table_intent_step(&current, || field.currentEditor())
                    .ok()??
                    .downcast::<NSTextView>()
                    .ok()?;
                let original =
                    native_markdown_table_intent_step(&current, || expected.field_editor.load())
                        .ok()??;
                if !std::ptr::eq(&*actual, &*original) {
                    return None; // 实际editor已替换，weak仍活也不是同一个输入会话。
                }
                let text = Self::bounded_table_field_text(&actual, &current).ok()?;
                let same = Self::table_field_values_equal(&expected.text, &text, &current).ok()?;
                let marked = Self::actual_table_field_mark(&actual, &current).ok()?;
                Some(expected_mark_transition(
                    expected.operation,
                    expected.marked,
                    marked,
                    same,
                ))
            })()
            .unwrap_or(false)
        };
        drop(expected); // NativeWeak/NSString的临时Rc先离借用、先释放，再决定后续撤权。
        if current() && !allowed {
            // 只有本字段且原epoch仍current时撤权；失效旧回调不能取消后来已发布的请求。
            self.cancel_table_continuity_for_new_input();
        }
        true
    }

    /// 命令只接受当前项目字段与实际currentEditor；局部Native claim析构后再纯核epoch。
    pub(in crate::native_surface) fn table_field_command_is_current(
        &self,
        control: &NSControl,
        editor: &NSTextView,
    ) -> bool {
        let epoch = self.ivars().table_input_intent_epoch.get();
        let Some(owner) = self.ivars().cell_editor.borrow().as_ref().map(Rc::clone) else {
            return false; // 没有项目字段时不把普通正文/Find通知归为表格输入。
        };
        if std::ptr::from_ref(control).cast::<()>() != std::ptr::from_ref(&**owner).cast::<()>()
            || !self.owned_field_notification_current(&owner, epoch)
        {
            return false;
        }
        // Rc字段owner已经离开Ref借用，Native currentEditor可安全触发同步回调。
        let actual = owner
            .currentEditor()
            .and_then(|value| value.downcast::<NSTextView>().ok());
        let same_editor = actual
            .as_ref()
            .is_some_and(|actual| std::ptr::eq(&**actual, editor));
        drop(actual); // 最后一次临时Native释放也可能重入，不能在此之后返回预先计算的权限。
        // 若此门禁为true，活动槽仍拥有同一个Rc，随后局部owner析构不会释放其Native字段。
        same_editor && self.owned_field_notification_current(&owner, epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 同一次unmark仅允许不改正文地结束原组合；新文字、扩展标记或新组合必须撤权。
    #[test]
    fn unmark_allows_only_same_utf16_and_old_or_absent_mark() {
        let before = Some(NSRange::new(2, 3));
        assert!(expected_mark_transition(
            NativeMarkdownTableFieldOperation::Unmark,
            before,
            None,
            true
        ));
        assert!(expected_mark_transition(
            NativeMarkdownTableFieldOperation::Unmark,
            before,
            before,
            true
        ));
        assert!(!expected_mark_transition(
            NativeMarkdownTableFieldOperation::Unmark,
            before,
            None,
            false
        ));
        assert!(!expected_mark_transition(
            NativeMarkdownTableFieldOperation::Unmark,
            before,
            Some(NSRange::new(2, 4)),
            true
        ));
        assert!(!expected_mark_transition(
            NativeMarkdownTableFieldOperation::Unmark,
            None,
            before,
            true
        ));
    }
    /// formatter校验不允许偷渡真实零长度组合，也不能把改写后的值当成未输入。
    #[test]
    fn validate_rejects_new_text_or_any_marked_state() {
        assert!(expected_mark_transition(
            NativeMarkdownTableFieldOperation::Validate,
            None,
            None,
            true
        ));
        assert!(!expected_mark_transition(
            NativeMarkdownTableFieldOperation::Validate,
            None,
            None,
            false
        ));
        assert!(!expected_mark_transition(
            NativeMarkdownTableFieldOperation::Validate,
            None,
            Some(NSRange::new(0, 0)),
            true
        ));
        assert!(!expected_mark_transition(
            NativeMarkdownTableFieldOperation::Validate,
            Some(NSRange::new(0, 1)),
            None,
            true
        ));
    }
}
