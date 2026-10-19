//! 目录续页完整扫描后只发布K+1有界候选，绝不发布文件系统随机前缀。

use super::*;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

const MAX_SCAN_ENTRIES: usize = 512;
const MAX_PAGE_ENTRIES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 每层目录排在笔记前；路径中间段必然是目录。
pub enum MarkdownWorkspaceEntryKind {
    Directory,
    Note,
}

/// 树顺序：父在子前，每层目录优先，同类名称按Unicode scalar序，完整路径打破同名平局。
/// 中文按码点而非拼音，不归一化或修改文件名，也不等同旧JS的默认localeCompare。
pub fn compare_markdown_workspace_entries(
    left: &str,
    left_kind: MarkdownWorkspaceEntryKind,
    right: &str,
    right_kind: MarkdownWorkspaceEntryKind,
) -> Ordering {
    let mut left_parts = left.split('/').peekable();
    let mut right_parts = right.split('/').peekable();
    loop {
        // 比较只借用当前分段；不按路径深度分配排序键或名称副本。
        match (left_parts.next(), right_parts.next()) {
            (Some(left_name), Some(right_name)) => {
                let left_directory = left_parts.peek().is_some()
                    || left_kind == MarkdownWorkspaceEntryKind::Directory;
                let right_directory = right_parts.peek().is_some()
                    || right_kind == MarkdownWorkspaceEntryKind::Directory;
                let order = right_directory.cmp(&left_directory).then_with(|| {
                    // 原目录名中的点保留，末尾.md笔记按展示stem比较；UTF8序与scalar序一致。
                    workspace_display_name(left_name, left_directory)
                        .cmp(workspace_display_name(right_name, right_directory))
                });
                if order != Ordering::Equal {
                    // 首个层级差异决定整棵子树的先后。
                    return order;
                }
            }
            (None, Some(_)) => {
                // 相同前缀的父目录先于后代。
                return Ordering::Less;
            }
            (Some(_), None) => {
                // 后代不能排在其父目录前。
                return Ordering::Greater;
            }
            (None, None) => {
                // 相同展示标题也保留严格总序，不能让续页键遗漏不同真实文件。
                return left.cmp(right).then_with(|| {
                    (left_kind == MarkdownWorkspaceEntryKind::Note)
                        .cmp(&(right_kind == MarkdownWorkspaceEntryKind::Note))
                });
            }
        }
    }
}

/// 借用名称，不因排序建立另一份字符串。
fn workspace_display_name(name: &str, directory: bool) -> &str {
    if directory {
        // 目录点号是名称的一部分。
        name
    } else {
        // 已验证的.md条目以文件stem显示；隐藏名没有非空stem前缀时保留原名。
        name.rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map_or(name, |(stem, _)| stem)
    }
}

/// 只持深度优先路径栈中的目录句柄，不缓存整层目录。
struct DirectoryCursor {
    path: PathBuf,
    entries: fs::ReadDir,
}

#[derive(Clone)]
/// 页权限绑定真实根、会话代次和用户折叠状态；没有正文或Native所有权。
pub struct MarkdownWorkspacePageAuthority(Arc<WorkspacePageAuthority>);

struct WorkspacePageAuthority {
    root: PathBuf,
    root_identity: (u64, u64),
    generation: Arc<AtomicU64>,
    expected_generation: u64,
    collapsed: HashSet<String>,
}

impl MarkdownWorkspacePageAuthority {
    /// 安装前的纯权限检查；真实根inode另在每个扫描步骤开始和发布前核验。
    pub fn is_current(&self, session: &MarkdownSessionAccess, collapsed: &HashSet<String>) -> bool {
        self.0.root == session.canonical_root
            && Arc::ptr_eq(&self.0.generation, &session.browse_generation)
            && self.0.expected_generation == self.0.generation.load(AtomicOrdering::Acquire)
            && self.0.collapsed == *collapsed
    }
}

/// 私有候选最多K+1条，EOF之后才拆成公开快照。
enum WorkspaceCandidate {
    Directory(String),
    Note(MarkdownNoteSummary),
}

impl WorkspaceCandidate {
    /// 比较借用既有路径，不读正文或复制标题。
    fn position(&self) -> (&str, MarkdownWorkspaceEntryKind) {
        match self {
            // 目录只有相对身份。
            Self::Directory(path) => (path, MarkdownWorkspaceEntryKind::Directory),
            // 笔记沿用同一个摘要里的相对身份。
            Self::Note(note) => (&note.relative_path, MarkdownWorkspaceEntryKind::Note),
        }
    }
}

/// 旧页只留下一个排序末项，不保留旧页或全部目录。
struct WorkspacePosition {
    path: String,
    kind: MarkdownWorkspaceEntryKind,
}

/// 活动pager只有K+1候选、当前深度栈和用户折叠集合；没有全库缓存。
pub struct MarkdownWorkspacePager {
    authority: MarkdownWorkspacePageAuthority,
    collapse_bound: bool,
    stack: Vec<DirectoryCursor>,
    candidates: Vec<WorkspaceCandidate>,
    after: Option<WorkspacePosition>,
    page_limit: Option<usize>,
    exhausted: bool,
}

/// 已完成整轮扫描的有限页；虚拟根不在结果内，适配层须为它预留一行。
pub struct MarkdownWorkspacePage {
    pub snapshot: MarkdownWorkspaceSnapshot,
    pub has_more: bool,
    pub authority: MarkdownWorkspacePageAuthority,
}

/// Pending没有可显示的半页；Ready也可表示空库的最终空页。
pub enum MarkdownWorkspacePageProgress {
    Pending,
    Ready(MarkdownWorkspacePage),
}

impl MarkdownSessionAccess {
    /// 新目录遍历只撤销旧目录，不取消正在显示的搜索。
    ///
    /// # Errors
    /// 固定根缺失、链接替换或metadata不可用时返回原存储错误，不发布游标。
    pub fn open_workspace_pager(&self) -> Result<MarkdownWorkspacePager, MarkdownWorkspaceError> {
        let root = resolve_existing_directory(&self.canonical_root, "")?;
        let metadata =
            fs::metadata(&root).map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        let expected_generation = self
            .browse_generation
            .fetch_add(1, AtomicOrdering::AcqRel)
            .wrapping_add(1);
        Ok(MarkdownWorkspacePager {
            authority: MarkdownWorkspacePageAuthority(Arc::new(WorkspacePageAuthority {
                root,
                root_identity: (metadata.dev(), metadata.ino()),
                generation: Arc::clone(&self.browse_generation),
                expected_generation,
                collapsed: HashSet::new(),
            })),
            collapse_bound: false,
            stack: Vec::new(),
            candidates: Vec::new(),
            after: None,
            page_limit: None,
            exhausted: false,
        })
    }
}

impl MarkdownWorkspacePager {
    /// 推进最多512次枚举；整轮EOF之前只返回Pending，不发布随机前缀。
    ///
    /// 页宽首步绑定1..=64，折叠集合不可中途改；每页重扫只保留O(K+深度+用户折叠状态)。
    /// 正文从不打开。外部变更遵循live续页，新增排序前缀在刷新后可见，不承诺全库快照。
    ///
    /// # Errors
    /// 取消/结构发布/新目录遍历/关闭返回WorkspaceSuperseded；同路径正文保存保留权限。页宽/折叠变化返回
    /// InvalidWorkspaceCursor。失败释放半页与目录句柄，不可复活该游标。
    pub fn next_page_cancellable(
        &mut self,
        requested_limit: usize,
        scanned_entry_limit: usize,
        collapsed: &HashSet<String>,
        cancelled: &AtomicBool,
    ) -> Result<MarkdownWorkspacePageProgress, MarkdownWorkspaceError> {
        let result = self.scan_step(requested_limit, scanned_entry_limit, collapsed, cancelled);
        if result.is_err() {
            // 错误的游标不能在flag复位后继续发布旧半页，容量也随之释放。
            self.stack = Vec::new();
            self.candidates = Vec::new();
            self.exhausted = true;
            self.close();
        }
        result
    }

    /// 每步开始及发布前重新核验权限和根inode。
    fn validate(&self, cancelled: &AtomicBool) -> Result<(), MarkdownWorkspaceError> {
        if cancelled.load(AtomicOrdering::Acquire)
            || self.authority.0.generation.load(AtomicOrdering::Acquire)
                != self.authority.0.expected_generation
        {
            // 撤权先于IO，也不能替较新的会话发布旧页。
            return Err(MarkdownWorkspaceError::WorkspaceSuperseded);
        }
        let root = resolve_existing_directory(&self.authority.0.root, "")?;
        let metadata =
            fs::metadata(root).map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        if (metadata.dev(), metadata.ino()) != self.authority.0.root_identity {
            // 同一路径重建的目录不是原来的根。
            return Err(MarkdownWorkspaceError::InvalidWorkspaceCursor);
        }
        Ok(())
    }

    /// 内部步骤只取得metadata；所有未完成候选都留在pager私有状态。
    fn scan_step(
        &mut self,
        requested_limit: usize,
        scanned_entry_limit: usize,
        collapsed: &HashSet<String>,
        cancelled: &AtomicBool,
    ) -> Result<MarkdownWorkspacePageProgress, MarkdownWorkspaceError> {
        self.validate(cancelled)?;
        let limit = requested_limit.clamp(1, MAX_PAGE_ENTRIES);
        if requested_limit == 0
            || scanned_entry_limit == 0
            || self.page_limit.is_some_and(|previous| previous != limit)
            || (self.collapse_bound && self.authority.0.collapsed != *collapsed)
        {
            // 旧排序末项不能用于新页宽或折叠意图；调用者须显式刷新。
            return Err(MarkdownWorkspaceError::InvalidWorkspaceCursor);
        }
        if !self.collapse_bound {
            // 首步尚未发布authority，独占写入用户折叠集合，后续严格比较。
            Arc::get_mut(&mut self.authority.0)
                .expect("首步authority由pager独占")
                .collapsed = collapsed.clone();
            self.collapse_bound = true;
            self.page_limit = Some(limit);
        }
        if self.exhausted {
            // 已经确认末页后不再打开目录。
            return Ok(MarkdownWorkspacePageProgress::Ready(self.empty_page()));
        }
        if self.stack.is_empty() {
            // 新逻辑页完整重扫，末项过滤发生在候选保留之前。
            self.candidates = Vec::with_capacity(limit + 1);
            let root = self.authority.0.root.clone();
            let entries =
                fs::read_dir(&root).map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
            self.stack.push(DirectoryCursor {
                path: root,
                entries,
            });
        }
        for _ in 0..scanned_entry_limit.min(MAX_SCAN_ENTRIES) {
            // EOF同样收费，连续空目录也不能越过512次循环和取消点。
            if cancelled.load(AtomicOrdering::Acquire)
                || self.authority.0.generation.load(AtomicOrdering::Acquire)
                    != self.authority.0.expected_generation
            {
                return Err(MarkdownWorkspaceError::WorkspaceSuperseded);
            }
            let Some(cursor) = self.stack.last_mut() else {
                // 整轮EOF之后才允许确认K+1和末页。
                break;
            };
            let relative = relative_workspace_path(&self.authority.0.root, &cursor.path)?;
            resolve_existing_directory(&self.authority.0.root, &relative)?;
            let Some(entry) = cursor.entries.next() else {
                // 当前目录耗尽立即释放句柄，再返回父级。
                self.stack.pop();
                continue;
            };
            let entry = entry.map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
            let kind = entry
                .file_type()
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
            if kind.is_symlink() {
                // 链接既不暴露为笔记，也不授权外部目录遍历。
                continue;
            }
            if cursor.path == self.authority.0.root
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(ATTACHMENTS_DIRECTORY_NAME))
            {
                // 根附件树是资源，不参与笔记目录排序。
                continue;
            }
            let path = entry.path();
            let (relative, entry_kind) = if kind.is_dir() {
                let relative = relative_workspace_path(&self.authority.0.root, &path)?;
                if !collapsed.contains(&relative) {
                    // 折叠目录本身仍可见，只有未折叠时才取得后代目录句柄。
                    let checked = resolve_existing_directory(&self.authority.0.root, &relative)?;
                    let entries = fs::read_dir(&checked)
                        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
                    self.stack.push(DirectoryCursor {
                        path: checked,
                        entries,
                    });
                }
                (relative, MarkdownWorkspaceEntryKind::Directory)
            } else if kind.is_file() && has_markdown_extension(&path) {
                // 先取得当前条目的路径scratch；未入选时连stat和标题分配也不执行。
                (
                    relative_markdown_path(&self.authority.0.root, &path)?,
                    MarkdownWorkspaceEntryKind::Note,
                )
            } else {
                // 不支持的条目收费但不占可显示行。
                continue;
            };
            let Some(index) = self.admit_candidate(&relative, entry_kind, limit) else {
                // 排序前缀、重复或更差条目只释放当前路径，不建立额外摘要记录。
                continue;
            };
            let candidate = match entry_kind {
                MarkdownWorkspaceEntryKind::Directory => {
                    // 原路径直接移入唯一目录记录；需要时的旧worst已经释放。
                    WorkspaceCandidate::Directory(relative)
                }
                MarkdownWorkspaceEntryKind::Note => {
                    // 只有入选候选才stat普通.md，正文从不打开或映射。
                    let metadata = entry
                        .metadata()
                        .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
                    WorkspaceCandidate::Note(MarkdownNoteSummary {
                        relative_path: relative,
                        title: markdown_title(&path)?,
                        byte_count: metadata.len(),
                        modified_at_ms: modified_at_ms(&metadata),
                    })
                }
            };
            self.candidates.insert(index, candidate);
        }
        self.validate(cancelled)?;
        if !self.stack.is_empty() {
            // 完整排序还未确定，只有Pending可以离开本步骤。
            return Ok(MarkdownWorkspacePageProgress::Pending);
        }
        let has_more = self.candidates.len() > limit;
        self.candidates.truncate(limit);
        self.after = self.candidates.last().map(|candidate| {
            // 末项来自实际返回的第K条，不能取被丢弃的sentinel。
            let (path, kind) = candidate.position();
            WorkspacePosition {
                path: path.to_owned(),
                kind,
            }
        });
        self.exhausted = !has_more;
        let mut page = self.empty_page();
        page.has_more = has_more;
        for candidate in std::mem::take(&mut self.candidates) {
            // 只移动有限metadata，Native按公开比较器恢复混合树顺序。
            match candidate {
                WorkspaceCandidate::Directory(path) => page.snapshot.directories.push(path),
                WorkspaceCandidate::Note(note) => page.snapshot.notes.push(note),
            }
        }
        Ok(MarkdownWorkspacePageProgress::Ready(page))
    }

    /// 先按纯路径预留候选槽，淘汰旧worst后才构造新摘要，记录总数保持K+1。
    /// 枚举另有当前路径scratch，续页另有一个最小末项键，不冒称它们零占用。
    fn admit_candidate(
        &mut self,
        path: &str,
        kind: MarkdownWorkspaceEntryKind,
        limit: usize,
    ) -> Option<usize> {
        if self.after.as_ref().is_some_and(|after| {
            compare_markdown_workspace_entries(path, kind, &after.path, after.kind)
                != Ordering::Greater
        }) {
            // live重扫不重复返回排序前缀；外部新建早项须刷新可见。
            return None;
        }
        let index = match self.candidates.binary_search_by(|existing| {
            let (existing_path, existing_kind) = existing.position();
            compare_markdown_workspace_entries(existing_path, existing_kind, path, kind)
        }) {
            Ok(_) => {
                // 外部并发变更下read_dir也可能重复身份，同一页不重复保留相同路径。
                return None;
            }
            Err(index) => index,
        };
        if index > limit {
            // 更差条目不进入容器，避免瞬时扩到K+2。
            return None;
        }
        if self.candidates.len() > limit {
            // 插入前先释放旧sentinel，保持K+1对象硬边界。
            self.candidates.pop();
        }
        Some(index)
    }

    /// EOF返回的空页仍携带独立权限，迟到末页同样必须被拒绝。
    fn empty_page(&self) -> MarkdownWorkspacePage {
        MarkdownWorkspacePage {
            snapshot: MarkdownWorkspaceSnapshot {
                directories: Vec::new(),
                notes: Vec::new(),
            },
            has_more: false,
            authority: self.authority.clone(),
        }
    }

    /// 显式关闭只撤销本代并释放扫描容量/末项，不影响较新遍历。
    /// authority与用户折叠集合随最后page/pager持有者drop，不声称本方法清掉外持共享值。
    pub fn close(&mut self) {
        let _ = self.authority.0.generation.compare_exchange(
            self.authority.0.expected_generation,
            self.authority.0.expected_generation.wrapping_add(1),
            AtomicOrdering::AcqRel,
            AtomicOrdering::Acquire,
        );
        self.stack = Vec::new();
        self.candidates = Vec::new();
        self.after = None;
        self.exhausted = true;
    }
}

#[cfg(test)]
mod tests;
