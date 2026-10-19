//! Cancellable session-owned vault search with bounded ranking and streaming matching.

use super::*;
use crate::immutable_snapshot::SnapshotDirectory;

mod paging;
pub use paging::{MarkdownSearchAuthority, MarkdownSearchCursor, MarkdownSearchPage};

impl MarkdownSessionAccess {
    /// Search the active vault with generation cancellation and bounded retained results.
    pub fn search_notes(
        &self,
        query: &str,
        requested_limit: usize,
    ) -> Result<Vec<MarkdownSearchResult>, MarkdownWorkspaceError> {
        // Preserve the established synchronous API without allocating an external cancellation token.
        self.search_notes_with_cancellation(query, requested_limit, None)
    }

    /// Search with an additional caller-owned cancellation token checked before startup.
    ///
    /// A foreground owner sets the token before joining its worker. This closes the
    /// cancel-before-worker-start race without moving task ownership into the core.
    pub fn search_notes_cancellable(
        &self,
        query: &str,
        requested_limit: usize,
        cancelled: &AtomicBool,
    ) -> Result<Vec<MarkdownSearchResult>, MarkdownWorkspaceError> {
        // Borrow the token only for this synchronous call; the caller owns its worker lifetime.
        self.search_notes_with_cancellation(query, requested_limit, Some(cancelled))
    }

    /// Coordinate bounded ranking while both session and caller cancellation remain authoritative.
    fn search_notes_with_cancellation(
        &self,
        query: &str,
        requested_limit: usize,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Vec<MarkdownSearchResult>, MarkdownWorkspaceError> {
        // Check caller cancellation before generation allocation: close may precede worker startup.
        if cancelled.is_some_and(|token| {
            // Observe caller cancellation before publishing or continuing a search generation.
            token.load(AtomicOrdering::Acquire)
        }) {
            // A cancelled worker owns no scan and must not supersede a later valid request.
            return Ok(Vec::new());
        }
        // Keep the query scalar-aligned and bounded before allocating its streaming matcher.
        let query = query.chars().take(MAX_QUERY_CHARACTERS).collect::<String>();
        let query = query.trim();
        if query.is_empty() {
            // Clearing a query cancels older scans and intentionally displays no search results.
            self.search_generation.fetch_add(1, AtomicOrdering::AcqRel);
            self.browse_generation.fetch_add(1, AtomicOrdering::AcqRel);
            return Ok(Vec::new());
        }

        // Only a live nonempty request supersedes the previous session search generation.
        let generation = self
            .search_generation
            .fetch_add(1, AtomicOrdering::AcqRel)
            .wrapping_add(1);
        // Both cancellation domains remain authoritative after the scan starts.
        let is_current = || {
            // A search remains current only while both its generation and the caller-owned cancellation token permit it.
            self.search_is_current(generation)
                && !cancelled.is_some_and(|token| {
                    // Observe caller cancellation before publishing or continuing a search generation.
                    token.load(AtomicOrdering::Acquire)
                })
        };
        let result_limit = requested_limit.clamp(1, MAX_SEARCH_RESULTS);
        let scanned = self.scan_ranked_search(query, result_limit, None, &is_current);
        if !is_current() {
            // 保留旧Vec接口既有取消语义；新分页接口另以显式失败拒绝失效凭据。
            return Ok(Vec::new());
        }
        Ok(scanned?
            .into_iter()
            .map(|item| {
                // 旧接口只转移最多K条元数据，不暴露内部排序键。
                item.result
            })
            .collect())
    }

    /// 共享单候选映射与有界排名；调用者决定旧Vec或新分页的取消返回语义。
    fn scan_ranked_search(
        &self,
        query: &str,
        result_limit: usize,
        after: Option<&SearchPosition>,
        is_current: &impl Fn() -> bool,
    ) -> Result<Vec<RankedSearchResult>, MarkdownWorkspaceError> {
        let pattern = SearchPattern::new(query.as_bytes());
        let mut ranked = Vec::<RankedSearchResult>::with_capacity(result_limit + 1);
        // 单次扫描可复用空的私有scratch目录；每个候选映射与目录owner都在本次调用结束前释放。
        let mut snapshot_directory = None;
        let walk_result = walk_markdown_workspace(&self.canonical_root, |entry| {
            // 目录条目同样检查取消，纯目录树也不能绕过结束权限继续遍历。
            if !is_current() {
                // 停止遍历同时释放已打开目录句柄，不保留后台游标。
                return Ok(WalkControl::Stop);
            }
            let MarkdownFilesystemEntry::Note(path) = entry else {
                // 目录只用于导航，不能当成正文候选或搜索结果。
                return Ok(WalkControl::Continue);
            };
            // 先匹配标题；标题命中优先且完全跳过该笔记正文映射。
            let title = markdown_title(path)?;
            let title_match = pattern.find(title.as_bytes(), is_current)?;
            // 在正文读取前按既有排序过滤已翻过的身份；不为旧页重复映射正文或生成摘要。
            let score = if title_match.is_some() {
                // 标题命中已经确定最高排名，不需要读取正文。
                2
            } else {
                // 只有正文可能命中；先核对排名前缀，排除旧页后才映射该候选。
                1
            };
            let relative_path = relative_markdown_path(&self.canonical_root, path)?;
            if after.is_some_and(|last| {
                // keyset 必须严格在末项之后；同分用原相对路径字节序，不受目录枚举顺序影响。
                compare_search_position(score, &relative_path, last) != Ordering::Greater
            }) {
                // 已展示的排名前缀不再进入当前页，正文仍保持未打开状态。
                return Ok(WalkControl::Continue);
            }
            let (score, snippet) = if title_match.is_some() {
                // 标题命中不生成正文摘要，也不获取文件映射。
                (2_u8, None)
            } else {
                // 仅临时映射当前正文候选；借用文本不能离开本回调。
                if snapshot_directory.is_none() {
                    // 第一次正文扫描才取得私有scratch；纯标题查询不执行快照IO。
                    snapshot_directory = Some(SnapshotDirectory::new().map_err(|_| {
                        // 无法建立私有快照边界则返回存储失败，不能回退为用户文件的可变映射。
                        MarkdownWorkspaceError::StorageUnavailable
                    })?);
                }
                let body_match = inspect_bounded_markdown_text_cancellable(
                    path,
                    MAX_SEARCH_FILE_BYTES,
                    is_current,
                    |text| {
                        // 首次命中后仍验证候选全部UTF8；合法EOF后才派生有限摘要。
                        let start = pattern.find_bounded(text, is_current)?;
                        start
                            .map(|start| {
                                bounded_file_search_snippet(
                                    text,
                                    start,
                                    pattern.byte_len(),
                                    is_current,
                                )
                            })
                            .transpose()
                    },
                    snapshot_directory.as_mut(),
                )?
                .value;
                let Some(snippet) = body_match else {
                    // 未命中候选不占结果名额，临时映射随检查返回释放。
                    return Ok(WalkControl::Continue);
                };
                (1_u8, Some(snippet))
            };
            // 只保留当前有界最佳候选；同分按路径排序，淘汰项立即释放。
            ranked.push(RankedSearchResult {
                score,
                result: MarkdownSearchResult {
                    relative_path,
                    title,
                    snippet,
                },
            });
            ranked.sort_unstable_by(compare_ranked_search_results);
            ranked.truncate(result_limit);
            Ok(WalkControl::Continue)
        });

        // 扫描失败保留原类别；取消也由外层在结果发布前再次核验，不发布半页。
        walk_result?;
        Ok(ranked)
    }

    /// Invalidate the current generation without retaining a worker or a callback.
    pub fn cancel_search(&self) {
        // The foreground owner separately cancels its external token and joins its owned worker.
        self.cancel_active_search();
    }

    /// Compare a request with the current live session search generation.
    fn search_is_current(&self, generation: u64) -> bool {
        // Acquire pairs with invalidation so an obsolete scan cannot publish results.
        self.search_generation.load(AtomicOrdering::Acquire) == generation
    }

    /// 同路径正文已原子发布，只撤销可能依赖旧正文的搜索，不撤销目录排序末项。
    /// 路径/kind/stem/root/collapse未变；note inode原子替换不属于目录cursor身份。
    pub(super) fn invalidate_content_search(&self) {
        self.search_generation.fetch_add(1, AtomicOrdering::AcqRel);
    }

    /// 新建、路径变动、显式取消或关闭同时撤销搜索和目录；不用于同路径正文保存。
    pub(super) fn cancel_active_search(&self) {
        self.invalidate_content_search();
        // 文件集合/排序身份已改变，旧目录after或排队页不能继续交付。
        self.browse_generation.fetch_add(1, AtomicOrdering::AcqRel);
    }
}

/// Query-sized KMP state, with ASCII folding that preserves original UTF-8 offsets.
struct SearchPattern {
    bytes: Vec<u8>,
    prefix_lengths: Vec<usize>,
    ascii_case_insensitive: bool,
}

impl SearchPattern {
    /// Build a reusable linear-time matcher without copying any candidate document.
    fn new(bytes: &[u8]) -> Self {
        let ascii_case_insensitive = bytes.is_ascii();
        let bytes = if ascii_case_insensitive {
            // ASCII normalization preserves byte width and is shared by every scanned candidate.
            bytes.iter().map(u8::to_ascii_lowercase).collect()
        } else {
            // Non-ASCII queries remain literal so byte offsets are valid scalar boundaries.
            bytes.to_vec()
        };
        let mut prefix_lengths = vec![0; bytes.len()];
        let mut prefix = 0;
        for index in 1..bytes.len() {
            // Compute borders once so repeated partial matches never restart a document scan.
            while prefix > 0 && bytes[index] != bytes[prefix] {
                // A mismatch falls back to a shorter valid border instead of revisiting source bytes.
                prefix = prefix_lengths[prefix - 1];
            }
            if bytes[index] == bytes[prefix] {
                // Extend the longest matching border used by later streaming comparisons.
                prefix += 1;
                prefix_lengths[index] = prefix;
            }
        }
        Self {
            bytes,
            prefix_lengths,
            ascii_case_insensitive,
        }
    }

    /// Return the original-width match length for bounded snippet construction.
    fn byte_len(&self) -> usize {
        // ASCII folding does not change this length, and Unicode queries were not folded.
        self.bytes.len()
    }

    /// 单个KMP前缀跨真实4KiB窗口延续；命中位置只记录一次，尾部UTF8仍必须有效。
    fn find_bounded(
        &self,
        text: &BoundedMarkdownText,
        is_current: &impl Fn() -> bool,
    ) -> Result<Option<usize>, MarkdownWorkspaceError> {
        let mut matched = 0;
        let mut found = self.bytes.is_empty().then_some(0);
        text.visit_validated_bytes(is_current, |start, bytes| {
            if found.is_some() {
                // 无需继续KMP，但visit仍读取并校验真实尾部，非法文件不能发布局部命中。
                return Ok(());
            }
            for (index, source_byte) in bytes.iter().copied().enumerate() {
                let byte = if self.ascii_case_insensitive {
                    source_byte.to_ascii_lowercase()
                } else {
                    source_byte
                };
                while matched > 0 && byte != self.bytes[matched] {
                    matched = self.prefix_lengths[matched - 1];
                }
                if byte == self.bytes[matched] {
                    matched += 1;
                    if matched == self.bytes.len() {
                        found = Some(start + index + 1 - matched);
                        break; // 只停止此窗口KMP，不终止外层完整UTF8扫描。
                    }
                }
            }
            Ok(())
        })?;
        Ok(found)
    }

    /// Scan one borrowed candidate, periodically yielding to both cancellation domains.
    fn find(
        &self,
        haystack: &[u8],
        is_current: impl Fn() -> bool,
    ) -> Result<Option<usize>, MarkdownWorkspaceError> {
        if self.bytes.is_empty() {
            // Empty literal matching is well-defined internally; public query coordination filters it.
            return Ok(Some(0));
        }
        let mut matched = 0;
        for (index, source_byte) in haystack.iter().copied().enumerate() {
            // Poll only at a bounded byte interval, including before the first candidate byte.
            if index % SEARCH_CANCELLATION_BYTE_INTERVAL == 0 && !is_current() {
                // Early cancellation unwinds the temporary mapping and bounded matcher state.
                return Err(MarkdownWorkspaceError::SearchSuperseded);
            }
            let byte = if self.ascii_case_insensitive {
                // Fold candidate ASCII in the same width-preserving space as the query.
                source_byte.to_ascii_lowercase()
            } else {
                // Literal Unicode bytes retain their original offset interpretation.
                source_byte
            };
            while matched > 0 && byte != self.bytes[matched] {
                // Preserve amortized linear progress by reusing an already validated border.
                matched = self.prefix_lengths[matched - 1];
            }
            if byte == self.bytes[matched] {
                // A matching byte advances the query prefix without allocating candidate text.
                matched += 1;
                if matched == self.bytes.len() {
                    // Stop at the first complete match; callers need one snippet per candidate.
                    return Ok(Some(index + 1 - matched));
                }
            }
        }
        // Exhaustion without a complete query is an ordinary nonmatching candidate.
        Ok(None)
    }
}

/// Bounded user-facing metadata plus its temporary title/body ranking score.
struct RankedSearchResult {
    score: u8,
    result: MarkdownSearchResult,
}

/// 已展示末项的纯排序键；不持正文、摘要、映射或目录迭代器。
#[derive(Debug)]
struct SearchPosition {
    score: u8,
    relative_path: String,
}

/// 比较当前候选与旧页末项；Greater才有资格进入后续页。
fn compare_search_position(score: u8, path: &str, last: &SearchPosition) -> Ordering {
    last.score.cmp(&score).then_with(|| {
        // 与首次查询同一字节序，避免并列项跨页重复或遗漏。
        path.cmp(&last.relative_path)
    })
}

/// Prefer title hits, then a stable relative-path ordering independent from filesystem iteration.
fn compare_ranked_search_results(
    left: &RankedSearchResult,
    right: &RankedSearchResult,
) -> Ordering {
    // Reversing score order keeps the strongest bounded results before truncation.
    right.score.cmp(&left.score).then_with(|| {
        // Break otherwise equal search ranks by stable vault-relative path ordering.
        left.result.relative_path.cmp(&right.result.relative_path)
    })
}

/// 已完成全文件UTF8验证后，只借用命中周围足以覆盖原scalar规则的小窗口。
fn bounded_file_search_snippet(
    text: &BoundedMarkdownText,
    match_start: usize,
    match_bytes: usize,
    is_current: &impl Fn() -> bool,
) -> Result<String, MarkdownWorkspaceError> {
    let match_end = match_start
        .checked_add(match_bytes)
        .filter(|end| *end <= text.len())
        .ok_or(MarkdownWorkspaceError::StorageUnavailable)?;
    // 原前缀rev().nth(N)实际需要N+1个前驱，后缀nth(N)最多需要N+1个边界。
    // 每scalar至多4byte，查询上限128scalar，整个借用始终小于4KiB。
    let context = (SNIPPET_CONTEXT_CHARACTERS + 1) * 4;
    let begin = match_start.saturating_sub(context);
    let end = match_end.saturating_add(context).min(text.len());
    text.with_bytes(begin..end, is_current, |bytes| {
        let mut skip = 0;
        while skip < bytes.len() && bytes[skip] & 0xc0 == 0x80 {
            // 任意字节起点最多切入一个已证明有效的scalar，跳过其continuation尾。
            skip += 1;
        }
        let bytes = &bytes[skip..];
        let valid = match std::str::from_utf8(bytes) {
            Ok(value) => value,
            Err(error) if error.error_len().is_none() => {
                // 有限窗口尾可以切入下一scalar，只去掉该残余；已验证EOF本身不能残缺。
                std::str::from_utf8(&bytes[..error.valid_up_to()])
                    .map_err(|_| MarkdownWorkspaceError::InvalidUtf8Document)?
            }
            Err(_) => return Err(MarkdownWorkspaceError::InvalidUtf8Document),
        };
        let local = match_start
            .checked_sub(begin + skip)
            .filter(|local| {
                valid.is_char_boundary(*local)
                    && local
                        .checked_add(match_bytes)
                        .is_some_and(|end| valid.is_char_boundary(end))
            })
            .ok_or(MarkdownWorkspaceError::StorageUnavailable)?;
        Ok(bounded_search_snippet(valid, local, match_bytes))
    })?
}

/// Materialize only scalar-aligned context and remove control/duplicate whitespace for display.
fn bounded_search_snippet(text: &str, match_start: usize, match_bytes: usize) -> String {
    // The matcher returns scalar-aligned UTF-8 bounds; extend by a fixed number of surrounding scalars.
    let match_end = match_start.saturating_add(match_bytes).min(text.len());
    let context_start = text[..match_start]
        .char_indices()
        .rev()
        .nth(SNIPPET_CONTEXT_CHARACTERS)
        .map_or(0, |(index, _)| {
            // Retain only a scalar-aligned source byte position for later bounded slicing.
            index
        });
    let context_end = text[match_end..]
        .char_indices()
        .nth(SNIPPET_CONTEXT_CHARACTERS)
        .map_or(text.len(), |(index, _)| {
            // Translate a bounded suffix scalar position back into the note's source byte coordinate.
            match_end + index
        });
    let mut snippet = String::with_capacity(context_end.saturating_sub(context_start));
    let mut previous_was_space = false;
    for character in text[context_start..context_end].chars() {
        // The fixed context bounds cap both iteration work and retained preview content.
        if character.is_whitespace() {
            // Normalize any Unicode whitespace to a single display separator.
            if !previous_was_space && !snippet.is_empty() {
                // Avoid leading/duplicate whitespace while preserving separation between visible words.
                snippet.push(' ');
            }
            previous_was_space = true;
        } else if !character.is_control() {
            // Keep printable content literally; other control characters contribute nothing to the UI.
            snippet.push(character);
            previous_was_space = false;
        }
    }
    snippet.trim().to_owned()
}

#[cfg(test)]
mod tests {
    //! External cancellation must remain effective when it happens before worker startup.

    use super::*;

    /// A pre-cancelled request must not start a scan or invalidate a later request generation.
    #[test]
    fn caller_cancellation_before_start_prevents_generation_and_can_be_reused() {
        // A synthetic vault exposes query results without accessing any personal file content.
        let directory = tempfile::tempdir().expect("temporary vault");
        let root = directory.path().join("vault");
        let workspace = MarkdownWorkspace::new(root.clone(), directory.path().join("data"));
        let access = workspace.open_session().expect("open synthetic session");
        fs::write(root.join("Note.md"), "needle").expect("seed searchable note");
        let generation = access.search_generation.load(AtomicOrdering::Acquire);
        let cancelled = AtomicBool::new(true);

        // Closing before the worker calls into core returns immediately without a generation claim.
        assert!(
            access
                .search_notes_cancellable("needle", 10, &cancelled)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            access.search_generation.load(AtomicOrdering::Acquire),
            generation
        );

        // A fresh active request still uses the same ordinary session and bounded result semantics.
        cancelled.store(false, AtomicOrdering::Release);
        let results = access
            .search_notes_cancellable("needle", 10, &cancelled)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].relative_path, "Note.md");
        assert!(access.search_generation.load(AtomicOrdering::Acquire) > generation);
    }
}

#[cfg(test)]
mod bounded_tests;
