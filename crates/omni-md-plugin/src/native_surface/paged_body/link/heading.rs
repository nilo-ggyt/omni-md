//! 标题跟随沿原链接槽逐量子解析，最终把一次真实选择提交交给已有SourceTarget与CPU终点。

use super::*;
use crate::native_surface::paged_body::NativeMarkdownPagedBodyError as Error;
use omni_markdown_core::{MarkdownHeadingAnchorRequest, MarkdownHeadingAnchorStep};

/// 仅保原完整请求、可选原click弱票与一个Core查询。跨笔记安装不持旧Scene或旧Doc。
/// 查询拥有的匿名scratch随取消/失败归还；它不是窗口级缓存或下一次点击的历史索引。
pub(super) struct PendingHeading {
    request: NativeMarkdownPagedSceneRequest,
    click: Option<(NativeMarkdownPagedSceneKey, Weak<()>)>,
    pub(super) query: MarkdownHeadingAnchorRequest,
}

impl NativeMarkdownPagedBody {
    /// 本地fragment已经由原URI边界解码一次；点击票只允许接纳这一张原Scene的标题查询。
    /// Source借用在排队前归还，不在鼠标栈执行整篇解析、文件扫描或字形重排。
    pub(in crate::native_surface) fn follow_heading(
        &self,
        key: &NativeMarkdownPagedSceneKey,
        intent: Weak<()>,
        fragment: &str,
    ) -> std::result::Result<(), ViewError> {
        if !self.scene_current(key) || intent.strong_count() == 0 {
            return Err(ViewError::Revoked);
        }
        self.begin_heading(key.request.clone(), Some((key.clone(), intent)), fragment)
            .map_err(cell::cell_view_error)
    }

    /// 已通过原保存/旧图barrier安装的新Doc，在自己的完整Input和几何下接管fragment。
    /// 不要求先发布文首CPU，也不拿旧文稿的Source或click票给新文稿补签。
    pub(in crate::native_surface) fn navigate_heading(&self, fragment: &str) -> Result<()> {
        let request = self.view()?.capture_scene_request()?;
        self.begin_heading(request, None, fragment)
    }

    fn begin_heading(
        &self,
        request: NativeMarkdownPagedSceneRequest,
        click: Option<(NativeMarkdownPagedSceneKey, Weak<()>)>,
        fragment: &str,
    ) -> Result<()> {
        if self.input.is_cell()
            || !self.current_body()
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
            || self.input.state().map_err(ViewError::Input)? != request.input
        {
            return Err(ViewError::Revoked.into());
        }
        let document = self.document.borrow().clone().ok_or(Error::Closed)?;
        let query = document
            .try_borrow()
            .map_err(|_| ViewError::Busy)?
            .heading_anchor_request(fragment)
            .map_err(ViewError::HeadingAnchor)?;
        let previous = self.pending_link.replace(Some(PendingLink {
            task: LinkTask::Heading(Box::new(PendingHeading {
                request,
                click,
                query,
            })),
        }));
        if let Some(previous) = previous {
            previous.retire()?; // 旧任务先退槽再显式关闭scratch，不跨槽借用析构或丢close错误。
        }
        self.schedule()
    }

    /// 一回合只调用一次Core的4096来源步；不与Composer、Font或第二次查询合领额度。
    /// 只有标题解析成功才消费原click并提交选择；原Source错误高于后继输入撤权。
    pub(super) fn advance_heading(
        &self,
        mut pending: PendingHeading,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> Result<bool> {
        let preparation = (|| -> Result<_> {
            let document = self.document.borrow().clone().ok_or(Error::Closed)?;
            {
                let document = document.try_borrow().map_err(|_| ViewError::Busy)?;
                document
                    .byte_offset_for_utf16(0)
                    .map_err(ViewError::Document)?;
            }
            let view = self.view()?;
            let live = self.current_body()
                && pending.request.same(actual)
                && self.input.state().map_err(ViewError::Input)? == pending.request.input;
            let click_live = if let Some((key, intent)) = &pending.click {
                match view.link_intent_current(key, intent) {
                    Ok(live) => live,
                    Err(ViewError::Revoked | ViewError::Closed | ViewError::Pending) => false,
                    Err(cause) => return Err(cause.into()),
                }
            } else {
                live // 新挂载导航仅绑定自己的完整请求，不凭空创建原mouse意图。
            };
            Ok((document, view, live && click_live))
        })();
        let (document, view, live) = match preparation {
            Ok(prepared) => prepared,
            Err(cause) => {
                // 任务退槽后，入口Source/客户失败同样显式取消scratch；不能仅靠Drop
                // 隐去close次因。原Source首错与清理错沿已有准备清理链一起归还Body。
                let cleanup = pending
                    .query
                    .cancel()
                    .map_err(|error| ViewError::HeadingAnchor(error).into());
                return producer::merge_preparation_cleanup(Err(cause), cleanup).map(|()| false);
            }
        };
        if !live {
            pending.query.cancel().map_err(ViewError::HeadingAnchor)?;
            if let Some((_, intent)) = &pending.click {
                view.retire_link_intent(intent);
            }
            return Ok(false);
        }
        let result = (|| -> Result<_> {
            let document = document.try_borrow().map_err(|_| ViewError::Busy)?;
            let step = pending
                .query
                .advance(&document, 4096)
                .map_err(ViewError::HeadingAnchor)?;
            Ok(match step {
                MarkdownHeadingAnchorStep::Progress { .. } => None,
                MarkdownHeadingAnchorStep::Found { target, .. } => Some(Some(
                    target
                        .position_for(&document)
                        .map_err(ViewError::HeadingAnchor)?,
                )),
                MarkdownHeadingAnchorStep::NotFound { .. } => Some(None),
            })
        })(); // 全部Source窗口和Doc借用在任何Native/选择提交之前明确归还。
        let result = match result {
            Ok(result) => result,
            Err(cause) => {
                let cleanup = pending
                    .query
                    .cancel()
                    .map_err(|error| ViewError::HeadingAnchor(error).into());
                return producer::merge_preparation_cleanup(Err(cause), cleanup).map(|()| false);
            }
        };
        let Some(position) = result else {
            if self.pending_link.borrow().is_none() {
                *self.pending_link.borrow_mut() = Some(PendingLink {
                    task: LinkTask::Heading(Box::new(pending)),
                });
            } else {
                // 原Native意图核验可以接纳后继click；旧查询不覆盖它，自己的FD仍须结算。
                pending.query.cancel().map_err(ViewError::HeadingAnchor)?;
            }
            self.schedule()?;
            return Ok(true);
        };
        if let Some((key, intent)) = &pending.click {
            if !view.link_intent_current(key, intent)? {
                view.retire_link_intent(intent);
                return Ok(false);
            }
            view.retire_link_intent(intent); // 成功/不存在均只消费一次；后继选择会正常退休旧Scene。
        }
        let Some(position) = position else {
            if let Some((key, _)) = &pending.click {
                view.report_link_failure(key, ViewError::LinkAnchorNotFound);
            } else {
                view.report_heading_failure(&pending.request, ViewError::LinkAnchorNotFound);
            }
            self.schedule()?; // 不存在只交原链接反馈；新挂载正文仍须继续自己的首屏，不能摧毁可编辑Doc。
            return Ok(true);
        };
        // Core已签同源真实标题物理起点；选择命令保留generation/Undo并按原规则展开必要折叠。
        // 原finish_edit_outcome会接既有reveal_committed_selection→SourceTarget，绝不Find假跳转。
        view.navigate_to_heading(pending.request.input, position)?;
        Ok(true)
    }
}
