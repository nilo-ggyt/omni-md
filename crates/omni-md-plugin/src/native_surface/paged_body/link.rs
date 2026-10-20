//! 原链接click只保弱权限和源坐标；目标沿唯一RootLeaf有界恢复，Ready之后才允许一次系统副作用。

use super::*;

/// 不保Scene、Reader、Doc或Native对象；View放弃该手势后Weak立即失效。
pub(super) struct PendingLink {
    key: NativeMarkdownPagedSceneKey,
    index: usize,
    intent: Weak<()>,
}

impl NativeMarkdownPagedBody {
    /// 只接纳mouseup已经确认的原Scene命中；不在指针栈读取URL、推进Parser或截获拖动。
    pub(super) fn follow_link(
        &self,
        key: &NativeMarkdownPagedSceneKey,
        index: usize,
        intent: Weak<()>,
    ) -> std::result::Result<(), ViewError> {
        if !self.scene_current(key) || intent.strong_count() == 0 {
            return Err(ViewError::Revoked);
        }
        *self.pending_link.borrow_mut() = Some(PendingLink {
            key: key.clone(),
            index,
            intent,
        });
        self.schedule().map_err(|_| ViewError::Revoked)
    }

    /// 一次主队列回合最多推进原4096量子，原FontTrial未完成则只等它已有的完成通知。
    pub(super) fn advance_link(&self, request: &NativeMarkdownPagedSceneRequest) -> Result<bool> {
        let Some(pending) = self.pending_link.borrow_mut().take() else {
            return Ok(false);
        };
        let view = self.view()?;
        let current = || view.link_intent_current(&pending.key, &pending.intent);
        let live = match current() {
            Ok(live) => live,
            Err(ViewError::Revoked | ViewError::Closed | ViewError::Pending) => false,
            Err(cause) => return Err(cause.into()),
        };
        if !pending.key.request.same(request) || !live {
            view.retire_link_intent(&pending.intent);
            return Ok(false); // 后继输入/焦点/宽度撤旧，不消费较新Scene或借同一坐标打开旧target。
        }
        let mut producer = self
            .producer
            .borrow_mut()
            .take()
            .expect("发布Scene的活动Body持唯一Producer");
        let result = producer.advance_link(self, &pending.key, pending.index);
        let waiting_font = producer.has_in_flight();
        *self.producer.borrow_mut() = Some(producer); // 所有Native调用与后续重入之前先归还原唯一owner槽。
        let live = match current() {
            Ok(live) => live,
            Err(ViewError::Revoked | ViewError::Closed | ViewError::Pending) => false,
            Err(cause) => return Err(cause.into()),
        };
        match result {
            Err(cause) if cause.is_foreground_revocation() && !live => {
                view.retire_link_intent(&pending.intent);
                self.schedule()?; // 原Source健康已由Producer核过；后继正常回合负责退休失效排版，不在此另造图。
            }
            Err(cause) => return Err(cause),
            Ok(_) if !live => {
                view.retire_link_intent(&pending.intent);
                self.schedule()?;
            }
            Ok(None) => {
                // 同步Native重入可以接纳较新click；旧量子只能放回仍未被替换的槽，不覆写新意图。
                if self.pending_link.borrow().is_none() {
                    *self.pending_link.borrow_mut() = Some(pending);
                }
                if !waiting_font {
                    self.schedule()?;
                }
            }
            Ok(Some(link)) => {
                if let Some(link) = link {
                    view.finish_link_activation(&pending.key, &pending.intent, &link.destination);
                } else {
                    view.retire_link_intent(&pending.intent);
                } // 真正覆盖而无link是一次完成；未知空洞不得被伪装成None或消耗第一次click。
            }
        }
        Ok(true)
    }
}
