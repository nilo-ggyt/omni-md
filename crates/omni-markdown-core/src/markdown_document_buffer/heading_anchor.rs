//! 标题链接只沿同一文档的真实 Global 事实推进；正文不整篇读取，重复 slug 不建内存目录。
//! 请求仅持原 root 弱身份；匿名文件归请求独占，成功、失败及取消均先显式关闭再交终点。

use super::*;
use crate::markdown_viewport_presentation::{
    parser::MarkdownParserSeed,
    table_window::{MarkdownTableWindowQuota, references::MarkdownTableReferenceQueries},
};
use pulldown_cmark::block_frontier::SourceFrame;
use pulldown_cmark::global_frontier::{
    BlockOriginLimits, GlobalFact, GlobalFrontier, GlobalFrontierError, GlobalFrontierLimits,
    GlobalSourceAuthority, GlobalStep, InlineBlockKind, InlineParagraphContext,
    InlineParagraphStep, InlinePlainLimits, LeafBlockDetails, LeafBlockKind, StructuralSpineLimits,
    VerifiedGlobalLeafBlock,
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::{fs::File, io, os::fd::IntoRawFd, os::unix::fs::FileExt, sync::Weak};
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};

mod projection;
use projection::{HeadingProjection, HeadingSource};
mod references;
use references::HeadingReferences;
mod footnotes;
use footnotes::HeadingFootnotes;

const SMALL: usize = 2048;
const PAGE: usize = 1024;
const FRAGMENT_LIMIT: usize = 16 * 1024;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// 所有错误仅含结构类别或系统错误号；fragment、标题及路径不进入错误文案。
pub enum MarkdownHeadingAnchorError {
    #[error("标题链接的正文来源不可用：{0}")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("标题链接的结构解析未完成：{0:?}")]
    Global(GlobalFrontierError),
    #[error("标题链接的行内来源未完成：{0:?}")]
    Semantic(super::table_window_request::MarkdownTableRequestError),
    #[error("标题链接临时存储不可用（{kind:?}，系统错误 {raw_os_error:?}）")]
    Storage {
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
    },
    #[error("标题链接已经取消")]
    Cancelled,
    #[error("标题链接的原文版本已经失效")]
    SourceSuperseded,
    #[error("标题链接片段不是有效的有限值")]
    InvalidFragment,
    #[error("标题链接的本次工作预算无效")]
    InvalidBudget,
    #[error("这个超长标题的行内语义尚不能完整确认")]
    UnsupportedInline,
    #[error("标题链接坐标或存储长度溢出")]
    ArithmeticOverflow,
    #[error("标题链接失败：{primary}；清理同时失败：{cleanup}")]
    Cleanup {
        primary: Arc<Self>,
        cleanup: Arc<Self>,
    },
}

type Error = MarkdownHeadingAnchorError;

fn storage(error: io::Error) -> Error {
    Error::Storage {
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 实际输入、复制、文件 I/O 与控制转换分别计量；不把一次 Parser 调用冒充逐 token fuel。
pub struct MarkdownHeadingAnchorWork {
    pub source_bytes: usize,
    pub copied_bytes: usize,
    pub metadata_bytes: usize,
    pub storage_bytes: usize,
    pub parser_input_bytes: usize,
    pub structural_transitions: usize,
}

#[derive(Clone, Debug)]
struct SourceIdentity {
    root: Option<Weak<MarkdownPieceTreeNode>>,
    // 空源没有 root；借原文档已有派生读窗的 Weak 区分独立空文档，不新增资源或强保缓存。
    empty_owner: Option<Weak<Mutex<super::read_cache::Utf16ReadCache>>>,
    generation: u64,
    bytes: usize,
}

impl SourceIdentity {
    fn new(document: &MarkdownDocumentBuffer) -> Self {
        // 非空源的piece root已具备真实分配身份；只降为Weak，不复制索引，也不持源字节。
        // 空源没有树节点，沿文档原有读缓存的Weak品牌区分两个同长度、同代次的独立文档。
        // 读缓存本身仍由文档释放，本票据不能延长它、mmap或历史的寿命。
        Self {
            root: document.root.as_ref().map(Arc::downgrade),
            empty_owner: document
                .root
                .is_none()
                .then(|| Arc::downgrade(&document.utf16_read_cache)),
            generation: document.generation(),
            bytes: document.byte_len(),
        }
    }

    /// 空文档同样保版本/长度；非空文档必须是原 allocation，Undo 回同一 root 也不复活旧票。
    fn require(&self, document: &MarkdownDocumentBuffer) -> Result<(), Error> {
        document.original_backing_health()?;
        // 先核源的真实健康，再核品牌；相同文本、相同长度和相同generation都不能冒充原文档。
        // Undo即使恢复相同root，也已发生新代次；旧导航结果不会重新变成可消费权限。
        let root_matches = match (&self.root, &document.root) {
            (Some(expected), Some(actual)) => Weak::ptr_eq(expected, &Arc::downgrade(actual)),
            (None, None) => self.empty_owner.as_ref().is_some_and(|owner| {
                Weak::ptr_eq(owner, &Arc::downgrade(&document.utf16_read_cache))
            }),
            _ => false,
        };
        if !root_matches
            || self.generation != document.generation()
            || self.bytes != document.byte_len()
        {
            return Err(Error::SourceSuperseded);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
/// 仅同一真实标题 Fact 才可签发；调用者不能从 fragment 或任意偏移构造导航权限。
pub struct MarkdownHeadingAnchorTarget {
    source: SourceIdentity,
    position: usize,
}

impl MarkdownHeadingAnchorTarget {
    /// 位置是原标题物理行首的 canonical UTF-16；显示及折叠展开仍由真实 Native 选择请求决定。
    pub fn position_for(&self, document: &MarkdownDocumentBuffer) -> Result<usize, Error> {
        self.source.require(document)?;
        // 终点由原Fact的物理行首转换而来；这里再次沿当前树确认UTF16边界仍合法。
        // 不依据slug长度、可见文字或折叠高度反推Source位置，显示端仍消费自己的真实选择意图。
        document.byte_offset_for_utf16(self.position)?;
        Ok(self.position)
    }
}

#[derive(Debug)]
pub enum MarkdownHeadingAnchorStep {
    Progress {
        work: MarkdownHeadingAnchorWork,
    },
    Found {
        target: MarkdownHeadingAnchorTarget,
        work: MarkdownHeadingAnchorWork,
    },
    NotFound {
        work: MarkdownHeadingAnchorWork,
    },
}

/// 文件中每条已提交记录只有长度和最终 slug；未提交标题固定在末端，不保任何内存索引。
struct Slugs {
    // committed之前是已完成唯一化的顺序记录；之后只有当前未发布候选，没有第二个内存目录。
    // base_end保原base末端，candidate_end可以随数值后缀变化；不会在RAM重建整条长slug。
    // scan/compared只描述当前固定页比较，不是某个视口、正文行号或可见范围。
    file: Option<File>,
    committed: u64,
    base_end: u64,
    candidate_end: u64,
    scan: u64,
    record_len: u64,
    compared: u64,
    equal: bool,
    suffix: u64,
    maximum: u64,
    irrelevant: bool,
    // 可见标题跨所有行内域连续参与Unicode默认lowercase；只留一个待定Σ，不缓存后继marks。
    previous_cased: bool,
    pending_sigma: Option<u64>,
}

impl Slugs {
    fn new(maximum: usize) -> Self {
        // 上限来自已经校验的fragment字节长度，而不是文档长度；更长的最终ID不可能命中它。
        // 此处不打开文件。空fragment直接到文档首部，没有标题时也不会产生临时FD。
        Self {
            file: None,
            committed: 0,
            base_end: 0,
            candidate_end: 0,
            scan: 0,
            record_len: 0,
            compared: 0,
            equal: true,
            suffix: 0,
            maximum: maximum as u64,
            irrelevant: false,
            previous_cased: false,
            pending_sigma: None,
        }
    }

    fn file(&self) -> Result<&File, Error> {
        self.file.as_ref().ok_or(Error::ArithmeticOverflow)
    }

    /// tempfile 创建的是 0600 匿名文件；只在出现首个标题时创建，不接收外部路径。
    fn begin(&mut self) -> Result<(), Error> {
        if self.file.is_none() {
            self.file = Some(tempfile::tempfile().map_err(storage)?);
        }
        // 当前候选只在已有记录之后追加；若上一长标题已被剪枝，覆盖其未发布尾部即可。
        // 重置本标题的比较与大小写状态，不清空已经提交的唯一ID历史。
        self.base_end = self
            .committed
            .checked_add(8)
            .ok_or(Error::ArithmeticOverflow)?;
        self.candidate_end = self.base_end;
        self.scan = 0;
        self.record_len = 0;
        self.compared = 0;
        self.equal = true;
        self.suffix = 0;
        self.irrelevant = false;
        self.previous_cased = false;
        self.pending_sigma = None;
        Ok(())
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        // 只有已经完成语义证明的可见片段可到这里；markup后续闭合不能改变先前输出的意义。
        // irrelevant是本候选的终结事实，后继片段不分配更大缓冲、不写无用文件字节。
        if self.irrelevant {
            return Ok(());
        }
        if self
            .length()
            .checked_add(bytes.len() as u64)
            .ok_or(Error::ArithmeticOverflow)?
            > self.maximum
        {
            // 唯一化只追加后缀；已证 base 比目标更长时永不影响任何可等于该目标的更短 ID。
            // 语义投影仍须先完整证明，不能拿尚待闭合的源字面前缀直接做这个剪枝。
            self.irrelevant = true;
            return Ok(());
        }
        self.file()?
            .write_all_at(bytes, self.candidate_end)
            .map_err(storage)?;
        self.candidate_end = self
            .candidate_end
            .checked_add(bytes.len() as u64)
            .ok_or(Error::ArithmeticOverflow)?;
        Ok(())
    }

    /// 默认Unicode lowercase只有最终Σ需要语言无关上下文；所有可见片段共用这个原文件状态。
    /// 先写同宽σ，真正后继/标题末端才决定ς；任意长Case_Ignorable后缀只占一个文件偏移。
    fn append_visible(&mut self, text: &str) -> Result<(usize, usize), Error> {
        if self.irrelevant {
            return Ok((0, 0));
        }
        // 调用者每次只交至多260字节完整UTF8；lowercase扩展也留在这一固定输出页。
        // 过滤发生在默认大小写上下文之后：被删掉的标点仍可能终结或延续Greek词形判断。
        let mut output = [0u8; 1024];
        let mut length = 0;
        let mut writes = 0;
        let start = self.candidate_end;
        for scalar in text.chars() {
            let ignored = case_ignorable(scalar);
            let cased = scalar.is_lowercase()
                || scalar.is_uppercase()
                || scalar.general_category() == GeneralCategory::TitlecaseLetter;
            // Consume the pending sigma on every non-ignorable successor, including a cased one.
            // Only a non-cased successor converts it to final sigma; neither path retains a word copy.
            if !ignored
                && let Some(pending) = self.pending_sigma.take()
                && !cased
            {
                if pending >= start {
                    // A sigma from this quantum still occupies its fixed output page. Both UTF-8
                    // forms use two bytes, so replacing the final byte cannot shift later content.
                    output[(pending - start) as usize + 1] = 0x82;
                } else {
                    // Earlier output is in this query's exclusive candidate file; patch the same
                    // certified offset without allocating another output namespace or loading its text.
                    self.file()?
                        .write_all_at("ς".as_bytes(), pending)
                        .map_err(storage)?;
                    writes += 2;
                }
            }
            // 当前Σ既是前一个待定Σ的真实后继，也可能成为新的待定项；始终只有一个offset。
            // 原小写σ/ς不做上下文改写，避免把lowercase误实现成Unicode casefold。
            if scalar == 'Σ' && self.previous_cased {
                self.pending_sigma = Some(start + length as u64);
            }
            if !ignored {
                self.previous_cased = cased;
            }
            for lower in scalar.to_lowercase() {
                let Some(lower) = slug_scalar(lower) else {
                    continue;
                };
                let mut encoded = [0; 4];
                let bytes = lower.encode_utf8(&mut encoded).as_bytes();
                let end = length + bytes.len();
                output
                    .get_mut(length..end)
                    .ok_or(Error::ArithmeticOverflow)?
                    .copy_from_slice(bytes);
                length = end;
            }
        }
        self.append(&output[..length])?;
        // 首次越过目标上限只撤销候选，不写当前输出页；此前Σ补写确已发生，仍按真实2字节报告。
        let appended = if self.irrelevant { 0 } else { length };
        Ok((writes + appended, length))
    }

    /// 真标题结束才授权最终Σ；语义域、视口页或文件片尾均没有这个权限。
    fn finish_visible(&mut self) -> Result<usize, Error> {
        let Some(pending) = self.pending_sigma.take() else {
            return Ok(0);
        };
        if self.irrelevant {
            return Ok(0);
        }
        self.file()?
            .write_all_at("ς".as_bytes(), pending)
            .map_err(storage)?;
        Ok(2)
    }

    fn length(&self) -> u64 {
        // 记录头的8字节永远不参与ID比较；长度直接取原文件水位，不扫描或复制候选正文。
        self.candidate_end - self.committed - 8
    }

    /// 同一最终 slug 不能重复；每次仅比较一个固定页，必要重扫以真实 I/O 计费而不建 HashMap。
    fn collision_step(&mut self) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        if self.scan == self.committed {
            return Ok((work, true));
        }
        if self.compared == 0 {
            // 先读已提交记录的真实长度。不同长度的ID不能相等，直接跳到下一条原记录。
            // 等长时才读取两个固定页；未提交尾部没有记录头，不能被当作历史条目扫描。
            let mut bytes = [0; 8];
            self.file()?
                .read_exact_at(&mut bytes, self.scan)
                .map_err(storage)?;
            self.record_len = u64::from_le_bytes(bytes);
            work.storage_bytes += 8;
            if self.record_len != self.length() {
                self.scan = self
                    .scan
                    .checked_add(8)
                    .and_then(|v| v.checked_add(self.record_len))
                    .ok_or(Error::ArithmeticOverflow)?;
                return Ok((work, false));
            }
            self.equal = true;
        }
        // 每个量子只比较至多1KiB，不因一次长ID碰撞把整个候选搬回内存。
        // 即使早期页已失配，仍按原长度前进到记录边界，后续扫描不会进入记录中间。
        let count = (self.record_len - self.compared).min(PAGE as u64) as usize;
        let mut previous = [0; PAGE];
        let mut candidate = [0; PAGE];
        self.file()?
            .read_exact_at(&mut previous[..count], self.scan + 8 + self.compared)
            .map_err(storage)?;
        self.file()?
            .read_exact_at(&mut candidate[..count], self.committed + 8 + self.compared)
            .map_err(storage)?;
        self.equal &= previous[..count] == candidate[..count];
        self.compared += count as u64;
        work.storage_bytes += count * 2;
        work.structural_transitions += count;
        if self.compared != self.record_len {
            return Ok((work, false));
        }
        if self.equal {
            // 原 base 只写一次；新后缀替换此前候选后缀，不把整条长 slug 搬回 RAM。
            if self.suffix == 0 {
                self.base_end = self.candidate_end;
            }
            // GitHub式碰撞依原出现顺序追加最小可用数字；显式写出的a-1也占用同一ID空间。
            // 数字后缀仍与所有既有最终ID重新比较，不能只为同base保一个不准确的计数器。
            self.suffix = self
                .suffix
                .checked_add(1)
                .ok_or(Error::ArithmeticOverflow)?;
            self.candidate_end = self.base_end;
            self.file()?.set_len(self.base_end).map_err(storage)?;
            let suffix = format!("-{}", self.suffix);
            self.append(suffix.as_bytes())?;
            work.storage_bytes += suffix.len();
            self.scan = 0;
        } else {
            self.scan = self
                .scan
                .checked_add(8)
                .and_then(|v| v.checked_add(self.record_len))
                .ok_or(Error::ArithmeticOverflow)?;
        }
        self.compared = 0;
        Ok((work, false))
    }

    fn commit(&mut self) -> Result<(), Error> {
        // 只有真实唯一化及fragment比较均结束才写记录头；失败候选不成为后继标题的占用项。
        // pwrite失败直接结束原请求，不重放一个结局未知的提交，也不提前移动committed。
        self.file()?
            .write_all_at(&self.length().to_le_bytes(), self.committed)
            .map_err(storage)?;
        self.committed = self.candidate_end;
        Ok(())
    }

    /// 所有终态显式 close；close 的未知后果不重试，Drop 仅负责遗弃请求的最后兜底。
    fn close(&mut self) -> Result<(), Error> {
        if let Some(file) = self.file.take() {
            let fd = file.into_raw_fd();
            // SAFETY: fd 是本匿名文件刚转移的唯一所有权；只调用一次，之后不再读写或重试它。
            if unsafe { libc::close(fd) } != 0 {
                return Err(storage(io::Error::last_os_error()));
            }
        }
        Ok(())
    }
}

// 每个阶段归同一个请求；一次advance只推进当前有限步骤，不在这里驱动UI事件循环。
// 短标题有明确2KiB Parser边界，长标题则保真实流式投影；两者最终共用同一个唯一化文件。
// References/Footnotes只回答当前标题的真实依赖，Unknown不会被折叠成Missing。
enum Phase {
    Scan,
    Copy { range: Range<usize>, next: usize },
    Parse,
    CollectReferences,
    References,
    Footnotes,
    Project,
    Slug { text: String, next: usize },
    Collision,
    Compare { next: usize, equal: bool },
    Done,
}

/// 请求不持正文/历史/映射强引用；巨标题只有原 Fact、固定页和匿名文件，不形成全文 String。
pub struct MarkdownHeadingAnchorRequest {
    source: SourceIdentity,
    // fragment是唯一外部连续值，已在入口限制到16KiB；请求完成时立即清空其容量。
    fragment: String,
    global: Option<GlobalFrontier>,
    phase: Phase,
    // small仅属于≤2KiB的完整标题；物理前缀、语义seed和Parser必须来自同一Global事实。
    // 它不是整段落备用缓存，长标题从构造之初就走磁盘投影，不能回退到这里扩容。
    small: Vec<u8>,
    small_leaf: Option<VerifiedGlobalLeafBlock>,
    paragraph: Option<InlineParagraphContext>,
    references: Option<Box<HeadingReferences>>,
    footnotes: Option<Box<HeadingFootnotes>>,
    projection: Option<Box<HeadingProjection>>,
    position: usize,
    slugs: Slugs,
    failure: Option<Error>,
    found: Option<MarkdownHeadingAnchorTarget>,
}

impl std::fmt::Debug for MarkdownHeadingAnchorRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MarkdownHeadingAnchorRequest")
            .field("source", &self.source)
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}

impl MarkdownDocumentBuffer {
    /// fragment 已由 URL 边界做一次 percent decode；这里不重解码、不把标题文本当 fragment 规范化。
    pub fn heading_anchor_request(
        &self,
        fragment: &str,
    ) -> Result<MarkdownHeadingAnchorRequest, Error> {
        self.original_backing_health()?;
        if fragment.len() > FRAGMENT_LIMIT || fragment.contains(['\0', '\r', '\n']) {
            return Err(Error::InvalidFragment);
        }
        // 工作燃料由同一真实Source长度确定；各续体不能因多次advance或新视口重置累计额度。
        // 结构页保独立有限库存，既不把全文AST放在请求里，也不向消费者交裸坐标签发Fact。
        let bytes = self.byte_len();
        let fuel = bytes
            .checked_mul(4096)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(Error::ArithmeticOverflow)?;
        let global = GlobalFrontier::new(
            self.generation(),
            bytes,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
            GlobalFrontierLimits {
                spine: StructuralSpineLimits {
                    max_depth: 16_384,
                    max_live_page_bytes: 4 * 1024 * 1024,
                    max_page_allocations: 1_000_000,
                    max_role_work: fuel,
                },
                max_byte_work: fuel,
                max_transitions: fuel,
            },
        )
        .map_err(Error::Global)?;
        Ok(MarkdownHeadingAnchorRequest {
            source: SourceIdentity::new(self),
            fragment: fragment.to_owned(),
            global: Some(global),
            phase: Phase::Scan,
            small: Vec::new(),
            small_leaf: None,
            paragraph: None,
            references: None,
            footnotes: None,
            projection: None,
            position: 0,
            slugs: Slugs::new(fragment.len()),
            failure: None,
            found: None,
        })
    }
}

impl MarkdownHeadingAnchorRequest {
    /// 先撤销权限，再真正归还匿名 FD；取消后任何 advance 都保持取消，绝不恢复旧标题扫描。
    pub fn cancel(&mut self) -> Result<(), Error> {
        // 先固定取消结局，随后释放全部自有资源；清理失败不能把取消重新解释为可恢复扫描。
        // 已存在的业务首错仍保留，新增的真实close次因由同一边界一起返回。
        self.failure.get_or_insert(Error::Cancelled);
        if let Err(cleanup) = self.release() {
            let primary = self.failure.take().unwrap_or(Error::Cancelled);
            let error = Error::Cleanup {
                primary: Arc::new(primary),
                cleanup: Arc::new(cleanup),
            };
            self.failure = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }

    fn release(&mut self) -> Result<(), Error> {
        // 先丢Source语义续体及有限文本，撤销后继解析权限，再分别尝试所有fallible close。
        // projection与slug文件互不代为关闭；任一失败也不能提前跳过另一个真实FD。
        // Done不代表成功：只有found或已固定failure能解释终态，不能凭清理完成制造Found。
        self.global = None;
        self.small = Vec::new();
        self.small_leaf = None;
        self.paragraph = None;
        self.references = None;
        self.footnotes = None;
        self.phase = Phase::Done;
        self.fragment = String::new();
        let projection = self
            .projection
            .take()
            .map_or(Ok(()), |mut projection| projection.close());
        let slugs = self.slugs.close();
        match (projection, slugs) {
            (Err(primary), Err(cleanup)) => Err(Error::Cleanup {
                primary: Arc::new(primary),
                cleanup: Arc::new(cleanup),
            }),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }

    /// 真实首因与真实关闭错误一起保留；后续查询只能重复这个结局，不把清理成功当业务成功。
    fn fail(&mut self, error: Error) -> Error {
        let primary = self.failure.take().unwrap_or(error);
        let error = match self.release() {
            Ok(()) => primary,
            Err(cleanup) => Error::Cleanup {
                primary: Arc::new(primary),
                cleanup: Arc::new(cleanup),
            },
        };
        self.failure = Some(error.clone());
        error
    }

    /// 每次入口先核原映射健康与 root；工作仅推进一个真实阶段，不驱动事件循环或代替 UI 显示。
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownHeadingAnchorStep, Error> {
        if let Err(error) = self.source.require(document) {
            return Err(self.fail(error));
        }
        if let Some(error) = self.failure.as_ref() {
            return Err(error.clone());
        }
        // 无效预算也是这个公开请求的真实失败边界；已有scratch必须立即归还，不能只等Drop。
        // Native使用固定合法量子，公开API调用者同样不能借换budget复活已失败的操作。
        if !(4096..=64 * 1024).contains(&budget) {
            return Err(self.fail(Error::InvalidBudget));
        }
        match self.advance_inner(document, budget) {
            Ok(step) => Ok(step),
            Err(error) => Err(self.fail(error)),
        }
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownHeadingAnchorStep, Error> {
        use MarkdownHeadingAnchorStep::*;
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        // 已完成请求可重复领取同一个弱身份结果；每次仍先经过上层Source健康/代次检查。
        // 这里不再访问关闭后的文件，也不会为同一导航重新分配或重复扫描标题。
        if matches!(self.phase, Phase::Done) {
            return Ok(match &self.found {
                Some(target) => Found {
                    target: target.clone(),
                    work,
                },
                None => NotFound { work },
            });
        }
        if self.fragment.is_empty() {
            return self.finish(document, 0, work); // 空 fragment 的 URL 语义是文档首部，仍先核同源。
        }
        match &mut self.phase {
            Phase::Scan => {
                // 只从原全局结构前沿领取标题Fact；代码围栏、HTML和普通段落不会因裸'#'文本误入。
                // 借页先finish再展开结构错误，真实Source回收失败保留在唯一失败链上。
                let global = self.global.as_mut().ok_or(Error::ArithmeticOverflow)?;
                let frame = document.table_source_frame(global.next_byte(), PAGE)?;
                let borrowed = frame.bytes.len();
                let step = global
                    .advance(
                        SourceFrame {
                            source_revision: frame.generation,
                            byte_start: frame.byte_start,
                            bytes: &frame.bytes,
                            real_eof: frame.real_eof,
                        },
                        GlobalSourceAuthority {
                            current_revision: document.generation(),
                            cancelled: false,
                        },
                        budget - 128,
                    )
                    .map_err(Error::Global);
                frame.finish()?;
                let step = step?;
                let (measured, fact) = match step {
                    GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => {
                        (work, None)
                    }
                    GlobalStep::Fact { fact, work } => (work, Some(fact)),
                    GlobalStep::Complete { work: measured } => {
                        work.source_bytes = borrowed;
                        work.structural_transitions += measured.transitions;
                        self.release()?;
                        return Ok(NotFound { work });
                    }
                };
                work.source_bytes = borrowed;
                work.structural_transitions += measured.transitions;
                if let Some(fact) = fact {
                    self.accept_heading(document, fact)?;
                }
            }
            Phase::Copy { range, next } => {
                // range已由完整短标题事实及2KiB上限认证；这里只复制当前小页，不重判行/容器语法。
                // piece边界可切UTF8，全部源字节齐备后才交原Parser校验，片尾绝不是语法EOF。

                if *next == range.end {
                    self.phase = Phase::Parse;
                    return Ok(Progress { work });
                }
                let frame = document.table_source_frame(*next, (range.end - *next).min(PAGE))?;
                self.small.extend_from_slice(&frame.bytes);
                let count = frame.bytes.len();
                *next += count;
                frame.finish()?;
                work.source_bytes = count;
                work.copied_bytes = count;
            }
            Phase::Parse => {
                let source =
                    std::str::from_utf8(&self.small).map_err(|_| Error::UnsupportedInline)?;
                if let Some(context) = self
                    .paragraph
                    .as_mut()
                    .filter(|context| !context.is_complete())
                {
                    // 深容器的短 Setext 必须先闭合原 capture 的真实逐行前缀，不能把片段重判为 Root。
                    let step = context
                        .advance(
                            source,
                            GlobalSourceAuthority {
                                current_revision: document.generation(),
                                cancelled: false,
                            },
                            budget - 128,
                        )
                        .map_err(Error::Global)?;
                    let measured = match step {
                        InlineParagraphStep::Progress { work }
                        | InlineParagraphStep::Complete { work } => work,
                        InlineParagraphStep::Unsupported { .. } => {
                            return Err(Error::UnsupportedInline);
                        }
                    };
                    work.source_bytes = measured.byte_reads;
                    work.structural_transitions += measured.transitions;
                    return Ok(Progress { work });
                }
                // Parser只解释本标题的真实seed。Start/End界定标题事件，URL/title和原始HTML不混入可见文本。
                // Code已经由原Parser完成trim/换行规则，Text已经完成实体/转义；这里不再写第二遍语法。
                let mut heading = false;
                let mut completed = false;
                let mut text = String::new();
                // 两种引用有不同权威：普通链接需要目标值，脚注只需要真实定义存在性。
                // 两个callback均只登记有限依赖，不在Parser迭代内做全源I/O或阻塞主线程到EOF。
                let unresolved = std::cell::Cell::new(false);
                let references = self.references.as_ref();
                let fuel = self
                    .source
                    .bytes
                    .checked_mul(4096)
                    .and_then(|n| n.checked_add(1024 * 1024))
                    .ok_or(Error::ArithmeticOverflow)?;
                let footnotes = self.footnotes.get_or_insert_with(|| {
                    Box::new(HeadingFootnotes::new(MarkdownTableWindowQuota::new(
                        1024, fuel,
                    )))
                });
                let footnotes = std::cell::RefCell::new(footnotes.as_mut());
                let footnote_lookup = |label: &str| footnotes.borrow_mut().lookup(label);
                let mut callback = |link: pulldown_cmark::BrokenLink<'_>| {
                    if let Some(references) = references {
                        // 只借同一标题真实全源扫描已证的值；Missing 与尚未完成的查询不混为一类。
                        return references.resolve(&link.reference);
                    }
                    unresolved.set(true);
                    None
                };
                let parser = if let Some(leaf) = self.small_leaf.as_ref() {
                    Parser::new_with_atx_leaf_seed(source, leaf, Some(&mut callback))
                        .map_err(Error::Global)?
                } else if let Some(context) = self.paragraph.as_ref() {
                    Parser::new_with_inline_block_seed(source, context, Some(&mut callback))
                        .map_err(Error::Global)?
                } else {
                    Parser::new_with_broken_link_callback(source,
                        crate::markdown_viewport_presentation::parser::markdown_presentation_options(false),
                        Some(&mut callback))
                };
                // 只对完整且≤2KiB的真实闭合标题使用原 Parser；绝不分片建 Parser 或放宽输入上限。
                // 同源脚注答案来自真实全局Fact；Unknown必须先暂停，片段没有定义不代表Missing。
                for event in parser.with_footnote_lookup(&footnote_lookup) {
                    match event {
                        Event::Start(Tag::Heading { .. }) if !heading && !completed => {
                            heading = true
                        }
                        Event::End(TagEnd::Heading(_)) if heading => {
                            heading = false;
                            completed = true;
                        }
                        Event::Text(value) | Event::Code(value) if heading => text.push_str(&value),
                        Event::SoftBreak | Event::HardBreak if heading => text.push(' '),
                        _ => {}
                    }
                }
                if let Some(error) = footnotes.borrow_mut().take_failure() {
                    return Err(error);
                }
                let footnotes_pending = !footnotes.borrow().is_ready();
                // Parser callbacks and their temporary RefCell borrows have ended. Only this scalar
                // pending result crosses into the next phase; no footnote body is retained here.
                if !completed {
                    return Err(Error::UnsupportedInline);
                }
                work.parser_input_bytes = self.small.len();
                work.copied_bytes = text.len();
                // 当前事件文本尚不可信时先丢弃它；真实查询完成后重跑同一小标题，不发布半正确slug。
                // 脚注会改变原LinkStack的关闭方式，因此其Found/Missing先于最终普通链接语义发布。
                if footnotes_pending {
                    self.phase = Phase::Footnotes;
                    return Ok(Progress { work });
                }
                if unresolved.get() {
                    self.phase = Phase::CollectReferences;
                    return Ok(Progress { work }); // 本量子已运行一次 Parser；查询收集留到下一真实阶段。
                }
                self.small = Vec::new();
                self.small_leaf = None;
                self.paragraph = None;
                self.references = None;
                self.footnotes = None;
                self.phase = Phase::Slug { text, next: 0 };
            }
            Phase::CollectReferences => {
                // 上一量子已经运行过Parser；依赖收集独立推进，不能把两次整块解释藏在同一收据。
                // 查询库存只覆盖当前短标题，后置定义仍从原Source按真实先后顺序取得。

                let source =
                    std::str::from_utf8(&self.small).map_err(|_| Error::UnsupportedInline)?;
                let seed = self
                    .small_leaf
                    .as_ref()
                    .map(MarkdownParserSeed::Atx)
                    .or_else(|| self.paragraph.as_ref().map(MarkdownParserSeed::Scoped));
                let options = self.small_leaf.as_ref().map_or_else(
                    || crate::markdown_viewport_presentation::parser::markdown_presentation_options(false),
                    VerifiedGlobalLeafBlock::options);
                let queries = MarkdownTableReferenceQueries::from_bounded_inline(
                    source,
                    options,
                    MarkdownTableWindowQuota::new(1024, 512 * 1024),
                    seed,
                )
                .map_err(|error| {
                    Error::Semantic(
                        super::table_window_request::MarkdownTableRequestError::Window(error),
                    )
                })?;
                self.references = Some(Box::new(HeadingReferences::new(document, queries)?));
                work.parser_input_bytes = self.small.len();
                self.phase = Phase::References;
            }
            Phase::References => {
                let references = self.references.as_mut().ok_or(Error::ArithmeticOverflow)?;
                let (measured, complete) = references.advance(document, budget)?;
                work = measured;
                if complete {
                    self.phase = Phase::Parse;
                }
            }
            Phase::Footnotes => {
                let (measured, complete) = self
                    .footnotes
                    .as_mut()
                    .ok_or(Error::UnsupportedInline)?
                    .advance(document, budget)?;
                work = measured;
                if complete {
                    self.phase = Phase::Parse;
                }
            }
            Phase::Project => {
                // 长标题先完全闭合共享行内事实，再按源序渲染到slug文件；后文delimiter不会回改已发语义。
                // 真实标题末端才完成最终Σ判断，文件页尾、视口尾和暂时NeedInput都没有该权限。

                let projection = self.projection.as_mut().ok_or(Error::UnsupportedInline)?;
                let (measured, complete) = projection.advance(document, &mut self.slugs, budget)?;
                work = measured;
                if complete {
                    work.storage_bytes += self.slugs.finish_visible()?;
                    self.projection = None;
                    self.phase = Phase::Collision;
                }
            }
            Phase::Slug { text, next } => {
                // 这里的text严格来自有界Parser输出。按完整scalar切片，大小写上下文仍由Slugs跨片保存。
                // 最终ID与长投影共用同一过滤/碰撞路径，不按两种渲染实现各自生成一份锚点规则。

                let mut end = (*next + 256).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let (writes, copied) = self.slugs.append_visible(&text[*next..end])?;
                work.copied_bytes = copied;
                work.storage_bytes = writes;
                work.structural_transitions += end - *next;
                *next = end;
                if end == text.len() {
                    work.storage_bytes += self.slugs.finish_visible()?;
                    self.phase = Phase::Collision;
                }
            }
            Phase::Collision => {
                if self.slugs.irrelevant {
                    self.phase = Phase::Scan;
                    return Ok(Progress { work });
                }
                let (measured, done) = self.slugs.collision_step()?;
                work = measured;
                if done {
                    self.phase = Phase::Compare {
                        next: 0,
                        equal: self.slugs.length() == self.fragment.len() as u64,
                    };
                }
            }
            Phase::Compare { next, equal } => {
                // 当前唯一候选只与外部已限制的fragment比较；相等才能发布原Fact对应的UTF16终点。
                // 不命中则提交最终ID供后继去重，继续原Global，不从当前位置新建一个假文档Parser。

                if *equal && *next < self.fragment.len() {
                    let count = (self.fragment.len() - *next).min(PAGE);
                    let mut bytes = [0; PAGE];
                    self.slugs
                        .file()?
                        .read_exact_at(&mut bytes[..count], self.slugs.committed + 8 + *next as u64)
                        .map_err(storage)?;
                    *equal &= bytes[..count] == self.fragment.as_bytes()[*next..*next + count];
                    *next += count;
                    work.storage_bytes += count;
                    return Ok(Progress { work });
                }
                if *equal {
                    return self.finish(document, self.position, work);
                }
                self.slugs.commit()?;
                work.storage_bytes += 8;
                self.phase = Phase::Scan;
            }
            Phase::Done => unreachable!("已完成状态在入口处理"),
        }
        self.source.require(document)?;
        Ok(Progress { work })
    }

    fn accept_heading(
        &mut self,
        document: &MarkdownDocumentBuffer,
        fact: GlobalFact,
    ) -> Result<(), Error> {
        // Fact本身决定标题种类及物理首行；长度只选择有界Parser或真实流式实现，绝不改变成员资格。
        // ATX首行可包含祖先前缀；Setext要保同一Paragraph捕获及underline确定的真实正文末端。
        let (range, physical, long) = match fact {
            GlobalFact::LeafBlock(leaf) if matches!(leaf.kind(), LeafBlockKind::AtxHeading(_)) => {
                let range = leaf.physical_start()..leaf.source_range().end;
                let physical = leaf.physical_start();
                if range.len() > SMALL {
                    (range, physical, Some(HeadingSource::Atx(leaf)))
                } else {
                    self.small_leaf = Some(leaf);
                    (range, physical, None)
                }
            }
            GlobalFact::InlineBlock(block)
                if matches!(block.kind(), InlineBlockKind::SetextHeading(_)) =>
            {
                let range = block.physical_range();
                let physical = range.start;
                if range.len() > SMALL {
                    (range, physical, Some(HeadingSource::Setext(block)))
                } else {
                    if block.scope_depth() > 1 {
                        let fuel = self
                            .source
                            .bytes
                            .checked_mul(4096)
                            .and_then(|n| n.checked_add(1024 * 1024))
                            .ok_or(Error::ArithmeticOverflow)?;
                        let (origins, seed) = block
                            .into_origins_and_semantic_seed(BlockOriginLimits {
                                max_byte_work: fuel,
                                max_transitions: fuel,
                            })
                            .map_err(Error::Global)?;
                        drop(origins); // 同一Fact仅移交上下文；不保另一份行目录或Source强图。
                        self.paragraph = seed
                            .bounded_paragraph_context(InlinePlainLimits {
                                max_byte_work: fuel,
                                max_transitions: fuel,
                            })
                            .map_err(Error::Global)?;
                        if self.paragraph.is_none() {
                            return Err(Error::UnsupportedInline);
                        }
                    }
                    (range, physical, None)
                }
            }
            _ => return Ok(()), // 真代码、HTML、引用定义与普通段落均不成为标题候选。
        };
        // 坐标转换依原树索引而非扫描标题可见文本；UTF8 emoji/组合字符不会被当作一个UTF16单元。
        // 这里只记录来源终点，最终Source健康、选择和CPU可见位置仍在各自真正拥有层验证。
        self.position = document.utf16_offset_for_byte(physical)?;
        self.slugs.begin()?;
        self.phase = if let Some(source) = long {
            self.projection = Some(Box::new(HeadingProjection::new(source)?));
            Phase::Project
        } else {
            self.small = Vec::with_capacity(range.len());
            Phase::Copy {
                next: range.start,
                range,
            }
        };
        Ok(())
    }

    fn finish(
        &mut self,
        document: &MarkdownDocumentBuffer,
        position: usize,
        work: MarkdownHeadingAnchorWork,
    ) -> Result<MarkdownHeadingAnchorStep, Error> {
        // Found发布紧前再次核原源；全部自有临时文件必须先成功关闭，才把终点交给Native。
        // 目标仅留Weak品牌和canonical坐标，文档、历史、mmap与字体资源都不会被导航结果强保。
        self.source.require(document)?;
        let target = MarkdownHeadingAnchorTarget {
            source: self.source.clone(),
            position,
        };
        self.release()?;
        self.found = Some(target.clone());
        Ok(MarkdownHeadingAnchorStep::Found { target, work })
    }
}

/// GitHub section-link的空格/标点过滤只消费一个真实lowercase scalar；不做语言地区化或NFKC。
fn slug_scalar(scalar: char) -> Option<char> {
    if scalar == ' ' {
        return Some('-');
    }
    (matches!(scalar, '-' | '_')
        || matches!(
            scalar.general_category_group(),
            GeneralCategoryGroup::Letter
                | GeneralCategoryGroup::Mark
                | GeneralCategoryGroup::Number
        ))
    .then_some(scalar)
}

/// 与现有unicode-properties 0.1.4同为Unicode17：D136规定的GC集合加WordBreak三个真实集合。
/// WordBreak的17个标点来自该版MidLetter/MidNumLet/Single_Quote；不能把所有标点或所有Mark视为可忽略。
/// 权威：https://www.unicode.org/Public/17.0.0/ucd/DerivedCoreProperties.txt 与 auxiliary/WordBreakProperty.txt。
fn case_ignorable(scalar: char) -> bool {
    matches!(
        scalar.general_category(),
        GeneralCategory::NonspacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::Format
            | GeneralCategory::ModifierLetter
            | GeneralCategory::ModifierSymbol
    ) || matches!(
        scalar,
        '\u{0027}'
            | '\u{002E}'
            | '\u{003A}'
            | '\u{00B7}'
            | '\u{0387}'
            | '\u{055F}'
            | '\u{05F4}'
            | '\u{2018}'
            | '\u{2019}'
            | '\u{2024}'
            | '\u{2027}'
            | '\u{FE13}'
            | '\u{FE52}'
            | '\u{FE55}'
            | '\u{FF07}'
            | '\u{FF0E}'
            | '\u{FF1A}'
    )
}

impl Drop for MarkdownHeadingAnchorRequest {
    fn drop(&mut self) {
        // 遗弃请求没有可返回错误的边界；File RAII 仍回收唯一 FD，普通终态已在 release 中显式关闭。
        drop(self.slugs.file.take());
    }
}

#[cfg(test)]
mod tests;
