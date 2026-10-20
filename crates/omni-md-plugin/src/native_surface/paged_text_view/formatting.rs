//! 格式入口只消费完整输入票，显示后果复用既有编辑提交通知；纯源码事务不等待像素重排。

use super::*;
use omni_markdown_core::MarkdownFormatAction;

impl NativeMarkdownPagedTextView {
    /// 原插表按钮在源码客户中仍一次插入完整中文模板，首格caret属于同一历史收据。
    ///
    /// # Errors
    /// 输入权限、IME或原Core事务失败直接返回；提交后失权保留CommittedEffect。
    pub fn insert_default_table(&self) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_link_intent(); // 格式按钮属于后继输入，即使事务失败也不能稍后激活原click。
        let origin = self.origin()?; // 公开UI入口始终是外来动作，不能凭当前Native栈的depth借走event归属。
        self.insert_default_table_at(&origin)
    }

    /// UI与原队首共用这一笔Core事务；只有受控queued入口可给出interpreted的原origin。
    pub(super) fn insert_default_table_at(&self, origin: &Origin) -> Result<()> {
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if !origin.interpreted && origin.delegate.defer_table_insertion(&permit)? {
            return Ok(());
        }
        // 模板只依赖原Doc和完整有向选择，没有几何读取；刚提交单格后的旧Scene本就必须释放。
        // 已接纳的词导航仍由上方原队列排序，不能为等待像素而丢掉这次明确的插表动作。
        let commit = Rc::new(origin.input.insert_default_table(&permit, &current)?);
        // 原通知、解释链交接和文字历史只完成一次。首格意图继续借同一实际收据，
        // 不能在 Native 重入后重新读取当前 caret，再把后继用户选择误认成刚插入的首格。
        self.finish_owned_commit(origin, commit.clone(), true)?;
        origin
            .delegate
            .activate_inserted_table(&commit)
            .map_err(|cause| {
                // 模板已经发布；激活失败是后果，外层不得据此重新插入第二张表。
                NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
            })
    }

    /// 对当前Body/Cell选择执行一次Core格式事务，不重新解释菜单tag或更换输入目标。
    ///
    /// 原Input认证Source、代际与完整选择；Core发布后改用同一Commit通知显示。
    /// Body保留当前视口，Cell沿原编辑后活动端定位，不主动跳到文末或物化选中文字。
    ///
    /// # Errors
    /// IME组合、旧输入票和Core格式失败直接返回；提交后的显示失败保留
    /// CommittedEffect，调用者不能把它解释为没有编辑或据此重放格式动作。
    pub fn apply_format(&self, action: MarkdownFormatAction) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_link_intent();
        let origin = self.origin()?; // 即使toolbar重入顶层NSTextInputClient，公开动作也不能冒充该event。
        self.apply_format_at(&origin, action)
    }

    /// 不复制格式实现或选择；原origin直接决定排队/本event收据交接，最终仍只有同一Core提交。
    pub(super) fn apply_format_at(
        &self,
        origin: &Origin,
        action: MarkdownFormatAction,
    ) -> Result<()> {
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if !origin.interpreted && origin.delegate.defer_format(action, &permit)? {
            return Ok(());
        }
        // Core直接从原piece root准备格式事务，所选文字不需要先变成可见glyph或整段String。
        // 输入当前性与健康仍逐步核验，显示正在重建不是改用后继选择或吞掉菜单动作的理由。
        let commit = origin.input.apply_format(&permit, action, &current)?;
        // Core与observer借用已结束；唯一原提交路径撤销旧CPU候选、通知Body重排及Native坐标失效。
        self.finish_edit_commit(origin, commit)
    }
}
