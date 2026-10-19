//! 搜索续页只保存查询、会话身份与最后排序键；每页重扫而不缓存全库或全部匹配。

use super::*;

/// 不透明的活动查询权限。字段不能由调用者伪造；本地变更/刷新/关闭会撤销原代次。
///
/// 这是live查询，不是全库原子快照：外部文件重命名或改写可能改变后续排名，刷新重新开始。
/// 仅相同查询、相同会话及同一真实根目录可继续；凭据不持有正文或文件句柄。
#[derive(Clone, Debug)]
pub struct MarkdownSearchAuthority(Arc<SearchAuthority>);

/// 查询权限只有不可变的有限metadata；所有页和cursor共享同一Arc，避免重复拷贝查询/根。
#[derive(Debug)]
struct SearchAuthority {
    query: String,
    root: PathBuf,
    root_identity: (u64, u64),
    authority: Arc<AtomicU64>,
    generation: u64,
}

/// 不透明末项只与产生它的查询权限组合；调用方不能自行制造排序键。
#[derive(Debug)]
pub struct MarkdownSearchCursor {
    authority: MarkdownSearchAuthority,
    after: SearchPosition,
}

/// 一页有限搜索结果。next存在表示已观察到至少一条后续匹配，不能只按页已满猜测。
#[derive(Debug)]
pub struct MarkdownSearchPage {
    pub results: Vec<MarkdownSearchResult>,
    pub next: Option<MarkdownSearchCursor>,
    /// 末页也携带权限，排队交付不能依赖next存在才验证本地保存是否已撤权。
    pub authority: MarkdownSearchAuthority,
}

impl MarkdownSearchAuthority {
    /// 纯检查当前查询权限，不执行IO；外部文件并发改写仍遵循live查询/选择时复核合同。
    pub fn is_current(&self, session: &MarkdownSessionAccess, query: &str) -> bool {
        // 先有限拒绝原始过长查询，trim不能成为超大空白输入的线性入口。
        query.chars().take(MAX_QUERY_CHARACTERS + 1).count() <= MAX_QUERY_CHARACTERS
            && self.0.query == query.trim()
            && self.0.root == session.canonical_root
            && Arc::ptr_eq(&self.0.authority, &session.search_generation)
            && session.search_is_current(self.0.generation)
    }
}

impl MarkdownSessionAccess {
    /// 取得保持标题优先、路径排序的下一页；最多保留K+1条候选和一份有限查询。
    ///
    /// 每页重扫当前库，时间为一次库扫描；结果内存为O(K)，正文仍一次只映射一个候选。
    /// 不保证外部并发修改下的全库冻结一致性；用户刷新后可重新覆盖变化后的排名。
    ///
    /// # Errors
    /// 取消或本地变更返回SearchSuperseded；错查询、会话、根或失效凭据返回InvalidSearchCursor。
    /// 非空查询最多128个Unicode scalar；文件/编码/存储失败沿用原类型，不发布部分结果。
    pub fn search_notes_page_cancellable(
        &self,
        query: &str,
        requested_limit: usize,
        cursor: Option<&MarkdownSearchCursor>,
        cancelled: &AtomicBool,
    ) -> Result<MarkdownSearchPage, MarkdownWorkspaceError> {
        if cancelled.load(AtomicOrdering::Acquire) {
            // 启动前取消不推进代次，也不让旧请求撤销较新的查询。
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        if query.chars().take(MAX_QUERY_CHARACTERS + 1).count() > MAX_QUERY_CHARACTERS {
            // 原始scalar预算先于trim，超长padding不能绕过有限入口或推进查询代次。
            return Err(MarkdownWorkspaceError::InvalidSearchCursor);
        }
        let query = query.trim();
        if query.is_empty() {
            // 空查询由Native恢复目录页，不创建空查询凭据或推进新的搜索代次。
            return Err(MarkdownWorkspaceError::InvalidSearchCursor);
        }
        let root = resolve_existing_directory(&self.canonical_root, "")?;
        let metadata =
            fs::metadata(&root).map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        let root_identity = (metadata.dev(), metadata.ino());
        let authority = if let Some(cursor) = cursor {
            let previous = &cursor.authority.0;
            if previous.query != query
                || previous.root != root
                || previous.root_identity != root_identity
                || !Arc::ptr_eq(&previous.authority, &self.search_generation)
            {
                // 相同路径字符串不足以证明同一会话/目录inode；拒绝跨库、重建根和换词凭据。
                return Err(MarkdownWorkspaceError::InvalidSearchCursor);
            }
            if !self.search_is_current(previous.generation) {
                // 本地文件操作、刷新、新查询或关闭已取消此查询；不能复活旧末项。
                return Err(MarkdownWorkspaceError::SearchSuperseded);
            }
            cursor.authority.clone()
        } else {
            // 只有新查询建立代次；续页复用原代次，不取消自己的不透明凭据。
            let generation = self
                .search_generation
                .fetch_add(1, AtomicOrdering::AcqRel)
                .wrapping_add(1);
            MarkdownSearchAuthority(Arc::new(SearchAuthority {
                query: query.to_owned(),
                root,
                root_identity,
                authority: Arc::clone(&self.search_generation),
                generation,
            }))
        };
        let is_current = || {
            // 同步扫描既服从本会话代次，也服从此次worker的独立取消权。
            self.search_is_current(authority.0.generation)
                && !cancelled.load(AtomicOrdering::Acquire)
        };
        let limit = requested_limit.clamp(1, MAX_SEARCH_RESULTS);
        let scanned = self.scan_ranked_search(
            query,
            limit + 1,
            cursor.map(|value| &value.after),
            &is_current,
        );
        if !is_current() {
            // 扫描后再核验，不能将取消中的半页或旧next交回UI。
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        let mut ranked = scanned?;
        let has_more = ranked.len() > limit;
        ranked.truncate(limit);
        let next = if has_more {
            // 只有真实K+1匹配证明后续页；末项来自实际保留的第K条，不取被丢弃的sentinel。
            let last = ranked.last().expect("非空有限页才可能有后续匹配");
            Some(MarkdownSearchCursor {
                authority: authority.clone(),
                after: SearchPosition {
                    score: last.score,
                    relative_path: last.result.relative_path.clone(),
                },
            })
        } else {
            // 恰好K条也可能已穷尽；next为空，不能诱导用户进入虚假的空白下一页。
            None
        };
        Ok(MarkdownSearchPage {
            results: ranked
                .into_iter()
                .map(|item| {
                    // 排序键仅用于本次续页；UI接收有限metadata/snippet，不保留内部排名容器。
                    item.result
                })
                .collect(),
            next,
            authority,
        })
    }
}

#[cfg(test)]
mod tests;
