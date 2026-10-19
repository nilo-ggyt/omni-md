// Copyright 2015 Google Inc. All rights reserved.
// 以下状态转移机械提取自固定0.13.4 firstpass.rs；完整MIT许可见LICENSE。

//! AST与纯结构spine共享的空list item、松紧和同marker继续规则。

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParentKind {
    Root,
    List { marker: u8 },
    DefinitionList,
    Item,
    Other,
}

/// 只提供原规则实际访问的纯结构状态；不要求frontier保存AST或已完成child链。
pub(crate) trait ListState {
    fn parent_kind(&self) -> ParentKind;
    fn begin_list_item(&self) -> Option<usize>;
    fn set_begin_list_item(&mut self, start: Option<usize>);
    fn last_line_blank(&self) -> bool;
    fn set_last_line_blank(&mut self, blank: bool);
    fn pop_container(&mut self, end: usize);
    fn loosen_parent_list(&mut self);
    fn loosen_grandparent_list(&mut self);
}

/// 空item最多以一个blank开头；第二个blank后的新块不能再继承该item的indent。
pub(crate) fn finish_empty_list_item(state: &mut impl ListState) {
    if let Some(start) = state.begin_list_item() {
        if state.last_line_blank() && state.parent_kind() == ParentKind::Item {
            // 与旧pop相同地先关闭item，再清begin标记；list本身仍可接同marker的新item。
            state.pop_container(start);
        }
    }
    state.set_begin_list_item(None);
}

/// 关闭直接悬空的list；blank引起的上层松列表状态在pop之后按真实祖父层写入。
pub(crate) fn finish_list(state: &mut impl ListState, end: usize) {
    finish_empty_list_item(state);
    if matches!(
        state.parent_kind(),
        ParentKind::List { .. } | ParentKind::DefinitionList
    ) {
        state.pop_container(end);
    }
    if state.last_line_blank() {
        state.loosen_grandparent_list();
        state.set_last_line_blank(false);
    }
}

/// 返回是否需要新建list容器；节点/有限spine的分配仍由各sink按自己的准入责任执行。
pub(crate) fn prepare_list(state: &mut impl ListState, start: usize, marker: u8) -> bool {
    finish_empty_list_item(state);
    if state.parent_kind() != ParentKind::Root {
        if state.parent_kind() == (ParentKind::List { marker }) {
            if state.last_line_blank() {
                // 同marker沿用原list，blank只把它置loose，不创建第二个同层容器。
                state.loosen_parent_list();
                state.set_last_line_blank(false);
            }
            return false;
        }
        // 其他父层保留旧finish_list顺序；不得直接pop一层猜测它必然是list。
        finish_list(state, start);
    }
    true
}
