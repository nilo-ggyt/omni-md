//! 原footer只投影当前canonical聚合和已发生的保存结果；反馈显隐复用同一正文几何来源。

use super::*;
use omni_markdown_core::MarkdownWorkspaceError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) struct NativeMarkdownFooterDocument {
    dirty: bool,
    recovery: bool,
    characters: usize,
    lines: usize,
}
impl NativeMarkdownFooterDocument {
    /// 只读树根聚合；caller须在任何Native setter前结束这份Doc借用。
    pub(in crate::native_surface) fn capture(document: &MarkdownDocumentBuffer) -> Self {
        Self {
            dirty: document.is_dirty(),
            recovery: document.has_pending_legacy_recovery(),
            characters: document.character_count(),
            lines: document.line_count(),
        }
    }
    pub(super) fn is_dirty(self) -> bool {
        self.dirty
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum NativeMarkdownFooterSaveResult {
    #[default]
    Current,
    Conflict,
    Recovery,
    Unconfirmed,
}
impl NativeMarkdownFooterSaveResult {
    fn after_save(self, error: Option<&MarkdownWorkspaceError>) -> Self {
        match error {
            None => Self::Current, // 只有实际成功保存或新文稿安装才撤旧保存错误。
            Some(MarkdownWorkspaceError::SaveConflict) => Self::Conflict,
            Some(MarkdownWorkspaceError::PendingLegacyRecovery) => Self::Recovery,
            // 重试的读取、输入busy或目录同步失败不能证明旧冲突已收敛；当前错误
            // 仍由原临时反馈显示，持久footer保留必须另存/重载的既有业务事实。
            Some(_) if matches!(self, Self::Conflict | Self::Recovery) => self,
            Some(_) => Self::Unconfirmed, // rename后sync失败也到此；不谎报磁盘未写或已持久化。
        }
    }
    /// 冲突条只投影已观察到的保存事实；不把目录sync未确认说成磁盘尚未写入。
    fn resolution_message(
        self,
        document: Option<NativeMarkdownFooterDocument>,
    ) -> Option<&'static str> {
        let document = document?;
        if document.recovery || self == Self::Recovery {
            return Some("恢复草稿尚未解决。请另存为新笔记或重新加载磁盘版本。");
        }
        (self == Self::Conflict).then_some("检测到磁盘版本变化，本地草稿已保留。请选择处理方式。")
    }

    fn label(self, document: Option<NativeMarkdownFooterDocument>) -> &'static str {
        let Some(document) = document else {
            return "未打开笔记";
        };
        if document.recovery || self == Self::Recovery {
            return "恢复草稿待处理 · 请另存或重新载入";
        }
        match self {
            Self::Conflict => "保存已暂停 · 等待处理冲突", // clean也能遇外部冲突，不能由dirty推断。
            Self::Unconfirmed => "保存未确认 · 草稿已保留",
            Self::Current if document.dirty => "本地修改待保存",
            Self::Current => "已保存",
            Self::Recovery => "恢复草稿待处理 · 请另存或重新载入",
        }
    }
}

/// 只弱观察本笔记已经装配的scroll与Find；没有第二个布局/正文owner。
#[derive(Clone)]
pub(super) struct NativeMarkdownDocumentAreaBinding {
    scroll: Weak<NSScrollView>,
    find: std::rc::Weak<NativeMarkdownFindBar>,
}

impl NativeMarkdownController {
    /// 新文稿或空态清除上一个笔记的保存结果；只缓存四个显示标量，不授予文件操作权限。
    pub(in crate::native_surface) fn install_footer_document(
        &self,
        document: Option<NativeMarkdownFooterDocument>,
    ) {
        self.ivars()
            .footer_save_result
            .set(NativeMarkdownFooterSaveResult::Current);
        if document.is_none() {
            self.clear_conflict_bar();
        }
        self.refresh_footer_document(document);
    }

    /// 编辑/Undo/Redo由原完成通知刷新；冲突不会被undo到clean、复制或clear_status洗掉。
    pub(in crate::native_surface) fn refresh_footer_document(
        &self,
        document: Option<NativeMarkdownFooterDocument>,
    ) {
        self.ivars().footer_document.set(document);
        self.render_footer();
    }

    /// 在原Workspace错误转String之前记录结果，保留“未确认”和“未写入”的实际区别。
    pub(in crate::native_surface) fn finish_footer_save(
        &self,
        document: NativeMarkdownFooterDocument,
        error: Option<&MarkdownWorkspaceError>,
    ) {
        let previous = self.ivars().footer_save_result.get();
        self.ivars()
            .footer_save_result
            .set(previous.after_save(error));
        self.refresh_footer_document(Some(document));
    }

    pub(super) fn render_footer(&self) {
        if self.ivars().close_finalizer_scheduled.get() {
            return;
        }
        let mode = self.ivars().published_presentation_mode.get();
        let document = self.ivars().footer_document.get();
        let save = self.ivars().footer_save_result.get();
        let current = || {
            !self.ivars().close_finalizer_scheduled.get()
                && self.ivars().published_presentation_mode.get() == mode
                && self.ivars().footer_document.get() == document
                && self.ivars().footer_save_result.get() == save
        };
        let mode = match mode {
            MarkdownPresentationMode::Source => "源码模式",
            MarkdownPresentationMode::InlinePreview => "行内预览",
        };
        let counts = document
            .map(|value| format!("{} 字符 · {} 行", value.characters, value.lines))
            .unwrap_or_default();
        let Some(labels) = self.ivars().footer_labels.get() else {
            return; // 仅构造尚未装配控件时无投影；真实文稿发布前会完整安装并刷新。
        };
        for (label, text) in labels
            .iter()
            .zip([mode, save.label(document), counts.as_str()])
        {
            if !current() {
                return;
            } // Native setter重入换文稿后不继续发布上一份显示标量。
            let text = NSString::from_str(text);
            label.setStringValue(&text);
            if !current() {
                return;
            }
            label.setToolTip(Some(&text));
        }
        if !current() {
            return;
        }
        if let Some(bar) = self.ivars().conflict_bar.get() {
            bar.publish(save.resolution_message(document), &current);
        }
        if current() {
            self.layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
        }
    }

    /// 拆图同时清除三列业务值，不让仍活着的控制器空壳保留旧笔记状态。
    pub(super) fn clear_footer(&self) {
        self.clear_conflict_bar();
        self.ivars().footer_document.set(None);
        self.ivars()
            .footer_save_result
            .set(NativeMarkdownFooterSaveResult::Current);
        if let Some(labels) = self.ivars().footer_labels.get() {
            for label in labels {
                label.setStringValue(ns_string!(""));
                label.setToolTip(None);
            }
        } // 构造失败尚无footer时仍必须撤销布局Weak，不能阻止其余真实资源清理。
        self.unbind_document_area();
    }

    /// 显隐只改变实际消息行占用；无消息时不永久留下空白，原footer不受影响。
    pub(super) fn set_feedback_visible(&self, visible: bool) {
        if visible && self.ivars().close_finalizer_scheduled.get() {
            return;
        }
        self.ivars().feedback_visible.set(visible);
        if let Some(label) = self.ivars().status_label.get() {
            label.setHidden(!visible);
        }
        // 构造失败未装反馈控件时只有纯标量清理；不创建替身或绕过原布局owner。
        self.layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
    }
    pub(in crate::native_surface) fn feedback_visible(&self) -> bool {
        self.ivars().feedback_visible.get() && self.ivars().status_label.get().is_some()
    }

    /// 绑定发生在真实scroll安装后；替换笔记同步撤旧Weak，不能由查找或反馈重建正文。
    pub(in crate::native_surface) fn bind_document_area(&self, scroll: &NSScrollView) {
        *self.ivars().document_area.borrow_mut() = Some(NativeMarkdownDocumentAreaBinding {
            scroll: Weak::from(scroll),
            find: std::rc::Weak::new(),
        });
        let _ = self.next_document_area_revision();
    }
    pub(in crate::native_surface) fn unbind_document_area(&self) {
        self.ivars().document_area.borrow_mut().take();
        let _ = self.next_document_area_revision();
    }
    pub(in crate::native_surface) fn bind_find_bar(
        &self,
        scroll: &NSScrollView,
        find: &Rc<NativeMarkdownFindBar>,
    ) -> Result<(), String> {
        let Some(binding) = self.ivars().document_area.borrow().clone() else {
            return Ok(()); // 独立Find embedding没有Window布局owner；其实际parent区域由show/hide直接消费。
        };
        let actual = binding.scroll.load().ok_or("笔记正文已经关闭")?;
        if !std::ptr::eq(&*actual, scroll) {
            return Err("查找栏不属于当前笔记正文".into());
        }
        // Weak升级与身份检查均已结束，修改绑定时不执行任何AppKit调用。
        drop(actual);
        *self.ivars().document_area.borrow_mut() = Some(NativeMarkdownDocumentAreaBinding {
            scroll: binding.scroll,
            find: Rc::downgrade(find),
        });
        let _ = self.next_document_area_revision();
        Ok(())
    }
    /// 正式Window已有原scroll绑定，扣真实chrome；独立Find消费者只有其调用方原parent。
    /// 两种装配都显式传同一绝对分配算法，不能因拿不到必需依赖而重建或猜一个窗口。
    pub(in crate::native_surface) fn document_area_base(
        &self,
        scroll: &NSScrollView,
        parent: NSRect,
    ) -> Result<NSRect, String> {
        let binding = self.ivars().document_area.borrow().clone();
        match binding {
            Some(binding) => {
                let actual = binding.scroll.load().ok_or("原笔记正文已经关闭")?;
                if !std::ptr::eq(&*actual, scroll) {
                    return Err("查找栏不属于当前笔记正文".into());
                }
                let base = super::super::window::native_markdown_editor_base_frame(parent.size);
                self.ivars()
                    .conflict_bar
                    .get()
                    .expect("Window正文布局已装配原冲突条")
                    .reserve_area(base)
            }
            None => Ok(parent), // 原独立embedding没有正式Window chrome；完整parent就是已给定正文区域。
        }
    }

    pub(in crate::native_surface) fn document_area_layout_revision(&self) -> u64 {
        self.ivars().document_area_revision.get()
    }

    fn next_document_area_revision(&self) -> Option<u64> {
        let revision = self.ivars().document_area_revision.get().checked_add(1)?;
        self.ivars().document_area_revision.set(revision);
        Some(revision)
    }

    /// resize/消息显隐共用同一绝对几何，不在scroll现有高度上重复加减Find或反馈。
    pub(in crate::native_surface) fn layout_document_area(
        &self,
        authority: NativeMarkdownHeaderAuthority,
    ) -> bool {
        let foreground = matches!(authority, NativeMarkdownHeaderAuthority::Foreground);
        if self.ivars().close_finalizer_scheduled.get()
            || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
            || (foreground && !native_markdown_controller_has_foreground_authority(self))
        {
            return false;
        }
        let Some(revision) = self.next_document_area_revision() else {
            return false;
        };
        let Some(binding) = self.ivars().document_area.borrow().clone() else {
            return true;
        }; // 空文稿没有scroll需要重排。
        let Some(scroll) = binding.scroll.load() else {
            return false;
        };
        let current = || {
            self.ivars().document_area_revision.get() == revision
                && !self.ivars().close_finalizer_scheduled.get()
                && VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_none()
                && (!foreground || native_markdown_controller_has_foreground_authority(self))
        };
        if !current() {
            return false;
        }
        // SAFETY: 只读取原scroll的父视图；局部强引用不发布到controller或后继笔记。
        let Some(parent) = (unsafe { scroll.superview() }) else {
            return false;
        };
        if !current() {
            return false;
        }
        let bounds = parent.bounds();
        let size = bounds.size;
        if !current() {
            return false;
        }
        let find = binding.find.upgrade();
        let height = find.as_ref().map_or(0.0, |find| find.occupied_height());
        let full = super::super::window::native_markdown_editor_base_frame(size);
        let Ok(base) = self.document_area_base(&scroll, bounds) else {
            return false;
        };
        if !current() {
            return false;
        }
        let Some(frames) =
            super::super::header_layout::editor_area_frames(base, self.feedback_visible(), height)
        else {
            return false;
        };
        scroll.setFrame(NSRect::new(
            NSPoint::new(frames.scroll.x, frames.scroll.y),
            NSSize::new(frames.scroll.width, frames.scroll.height),
        ));
        if !current() {
            return false;
        }
        if let (Some(find), Some(frame)) = (find, frames.find)
            && !find.position_in_document_area(
                NSRect::new(
                    NSPoint::new(frame.x, frame.y),
                    NSSize::new(frame.width, frame.height),
                ),
                self,
            )
        {
            return false;
        }
        if !current() {
            return false;
        }
        if let Some(bar) = self.ivars().conflict_bar.get() {
            let content_top = frames
                .find
                .as_ref()
                .map_or(frames.scroll.y + frames.scroll.height, |frame| {
                    frame.y + frame.height
                });
            bar.position(&parent, full, base, content_top, &current);
        }
        if !current() {
            return false;
        }
        // 信息卡覆盖当前正文区域，沿同一resize/Find/反馈几何定位，不压缩正文或另建窗口。
        self.position_information_panel(&parent, scroll.frame(), &current);
        current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    /// 实际canonical的字符/CRLF行数和Undo保存基线由原树聚合提供，不读第二份字符串。
    fn canonical_edit_and_undo_supply_the_footer_counts_and_dirty_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("footer.md");
        std::fs::write(&path, "A😀\r\n中").unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 4096).unwrap();
        let clean = NativeMarkdownFooterDocument::capture(&document);
        assert_eq!((clean.characters, clean.lines, clean.dirty), (4, 2, false));
        document
            .replace_utf16_range(0..1, "ab", MarkdownTextSelection::caret(2))
            .unwrap();
        let edited = NativeMarkdownFooterDocument::capture(&document);
        assert_eq!(
            (edited.characters, edited.lines, edited.dirty),
            (5, 2, true)
        );
        document.undo().unwrap();
        assert_eq!(NativeMarkdownFooterDocument::capture(&document), clean);
    }

    #[test]
    /// 冲突是独立保存结果；undo到clean和一般失败均不能伪报已保存或磁盘未写。
    fn save_labels_preserve_clean_conflict_recovery_and_uncertain_publication() {
        let clean = NativeMarkdownFooterDocument {
            dirty: false,
            recovery: false,
            characters: 3,
            lines: 1,
        };
        let dirty = NativeMarkdownFooterDocument {
            dirty: true,
            ..clean
        };
        assert_eq!(
            NativeMarkdownFooterSaveResult::Current.resolution_message(None),
            None
        );
        assert_eq!(
            NativeMarkdownFooterSaveResult::Current.resolution_message(Some(clean)),
            None
        );
        assert_eq!(
            NativeMarkdownFooterSaveResult::Unconfirmed.resolution_message(Some(dirty)),
            None
        );
        assert_eq!(
            NativeMarkdownFooterSaveResult::Current.resolution_message(Some(
                NativeMarkdownFooterDocument {
                    recovery: true,
                    ..clean
                }
            )),
            Some("恢复草稿尚未解决。请另存为新笔记或重新加载磁盘版本。")
        );
        assert_eq!(
            NativeMarkdownFooterSaveResult::Current.label(None),
            "未打开笔记"
        );
        assert_eq!(
            NativeMarkdownFooterSaveResult::Current.label(Some(clean)),
            "已保存"
        );
        assert_eq!(
            NativeMarkdownFooterSaveResult::Current.label(Some(dirty)),
            "本地修改待保存"
        );
        let conflict = NativeMarkdownFooterSaveResult::Current
            .after_save(Some(&MarkdownWorkspaceError::SaveConflict));
        assert_eq!(conflict.label(Some(clean)), "保存已暂停 · 等待处理冲突");
        assert_eq!(conflict.label(Some(dirty)), "保存已暂停 · 等待处理冲突");
        // 外部版本与继续编辑后的正文相同时，真实baseline采用仍可能在目录sync失败；
        // StorageUnavailable没有完成采用权限，不能把此前的SaveConflict清掉。
        let retry = conflict.after_save(Some(&MarkdownWorkspaceError::StorageUnavailable));
        assert_eq!(retry, NativeMarkdownFooterSaveResult::Conflict);
        // 旧冲突随后发生目录sync未确认时，直达操作仍在，但不能再保证“没有覆盖磁盘”。
        assert_eq!(
            retry.resolution_message(Some(clean)),
            Some("检测到磁盘版本变化，本地草稿已保留。请选择处理方式。")
        );
        assert_eq!(retry.label(Some(clean)), "保存已暂停 · 等待处理冲突");
        let recovery = retry.after_save(Some(&MarkdownWorkspaceError::PendingLegacyRecovery));
        assert_eq!(
            recovery.after_save(Some(&MarkdownWorkspaceError::StorageUnavailable)),
            NativeMarkdownFooterSaveResult::Recovery
        );
        let uncertain = NativeMarkdownFooterSaveResult::Current
            .after_save(Some(&MarkdownWorkspaceError::StorageUnavailable));
        assert_eq!(uncertain.label(Some(dirty)), "保存未确认 · 草稿已保留");
        assert_eq!(retry.after_save(None).label(Some(clean)), "已保存");
        assert_eq!(
            recovery.after_save(None),
            NativeMarkdownFooterSaveResult::Current
        );
        assert_eq!(
            NativeMarkdownFooterSaveResult::Current.label(Some(NativeMarkdownFooterDocument {
                recovery: true,
                ..clean
            })),
            "恢复草稿待处理 · 请另存或重新载入"
        );
    }
}
