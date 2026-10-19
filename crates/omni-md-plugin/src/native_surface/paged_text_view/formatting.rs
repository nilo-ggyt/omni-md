//! 格式入口只捕获当前真实Scene与原输入票，显示后果复用既有编辑提交通知。

use super::*;
use omni_markdown_core::MarkdownFormatAction;

impl NativeMarkdownPagedTextView {
    /// 对当前Body/Cell选择执行一次Core格式事务，不重新解释菜单tag或更换输入目标。
    ///
    /// 原Scene只认证格式之前的Source、选择与几何；Core发布后改用同一Commit通知显示。
    /// Body保留当前视口，Cell沿原编辑后活动端定位，不主动跳到文末或物化选中文字。
    ///
    /// # Errors
    /// 尚无当前Scene、IME组合、旧输入票和Core格式失败直接返回；提交后的显示失败保留
    /// CommittedEffect，调用者不能把它解释为没有编辑或据此重放格式动作。
    pub fn apply_format(&self, action: MarkdownFormatAction) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        let commit = scene.origin.input.apply_format(&permit, action, &current)?;
        // Core与observer借用已结束；唯一原提交路径撤销旧CPU候选、通知Body重排及Native坐标失效。
        self.finish_edit_commit(&scene.origin, commit)
    }
}
