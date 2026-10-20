// Copyright 2015 Google Inc. All rights reserved.
// 原链接目标/标题 scanner 的分帧消费；完整 MIT 许可见 LICENSE。

//! 同一闭合 Root 的真实 LinkStack / reference 暂停，不创建正文、第二 Parser 或括号 Vec。
//!
//! 顺序游标实际遇到 opener 才建立原文件记录；code/HTML/目标/标题继续调用原 scanner。
//! 成功 Link 对祖先的禁用与整段 Parser 共用一个决策，Image 保留自身和内层 Link。
//! 引用标签复用原规范化器，先交原 Global/Queries 回答，再发布正面样式；局部没有定义
//! 不能冒充 Missing。标题脚注沿真实Global定义暂停；普通视口仍保其独立全语义路径，
//! 不把标题专属省略事实或容器权限误交给原来的样式消费者。
//!
//! 文件记录按源顺序重放成 Begin/label/End，Core 同文件保存各层强调检查点。
//! 目标与标题从不进入 label 样式栈。所有范围仍为原 UTF8 byte 坐标，Source、UTF16、
//! 选择与实际空间准入由同一 Core 请求拥有；外存错误不被包装成 General 或字面成功。
//! 暂停不会持有借入源帧，取消立即释放游标固定状态，文件由其唯一拥有者关闭。

use super::*;

/// 同一个真实 StackFile 中的记录身份；零是文件编码的 None，不能成为一条记录。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InlineLinkRecordId(std::num::NonZeroUsize);
impl InlineLinkRecordId {
    pub fn new(value: usize) -> Option<Self> {
        std::num::NonZeroUsize::new(value).map(Self)
    }
    pub fn get(self) -> usize {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Link 会禁用其仍在栈上的 Link 祖先；Image 不禁用祖先，也不赋予外层点击动作。
pub enum InlineLinkKind {
    Link,
    Image,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Full 使用第二标签；Collapsed 与 Shortcut 使用原外标签，三者的 owner 结束范围不同。
pub enum InlineReferenceKind {
    Full,
    Collapsed,
    Shortcut,
    /// 原GFM脚注引用，key不含'^'；只有全源定义事实才能决定省略或保留字面。
    Footnote,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Open 只是栈候选；Literal 不创建样式 scope，Direct/Reference 才允许在 replay 中签 Begin。
pub enum InlineLinkRecordStatus {
    Open,
    Literal,
    Direct,
    Reference,
    /// 原脚注查到真实定义；不建立普通Link样式scope，而在源序replay交付省略事实。
    Footnote,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 原括号栈的外存记录，只含源坐标和真实栈边；它本身不能签发样式或链接权限。
/// Core 用原文件两个 64B slot 编码，不能把 Rust 内存布局当持久格式。
/// previous 是语法父 opener，next 是源顺序后继，两者不可互换成文件物理相邻关系。
pub struct InlineLinkRecord {
    pub previous: Option<InlineLinkRecordId>,
    pub next: Option<InlineLinkRecordId>,
    pub serial: usize,
    pub source_start: usize,
    pub label_start: usize,
    pub label_end: usize,
    pub source_end: usize,
    pub destination: Range<usize>,
    pub title: Option<Range<usize>>,
    pub reference: Option<(InlineReferenceKind, Range<usize>)>,
    pub kind: InlineLinkKind,
    pub status: InlineLinkRecordStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 同一文件的固定记录读写，不接受源正文、目标 String 或另一个文件的句柄。
pub enum InlineLinkStorageOperation {
    Append(InlineLinkRecord),
    Read(InlineLinkRecordId),
    Update {
        id: InlineLinkRecordId,
        record: InlineLinkRecord,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
/// 只允许恢复原请求。读写错误由真实文件拥有者直接保留为其 errno，不能反喂成 General。
pub struct InlineLinkStorageRequest {
    pub request_id: usize,
    pub operation: InlineLinkStorageOperation,
}
#[derive(Clone, Debug)]
/// 回复必须与原操作对应；Appended 身份只在整条 128B 记录真正可读取后交付。
pub enum InlineLinkStorageReply {
    Appended { id: InlineLinkRecordId },
    Read { record: InlineLinkRecord },
    Updated,
}
#[derive(Debug)]
/// Progress 只表示原文件已经做了真实工作，例如分片 flush；原操作仍处于暂停状态。
/// 分配准入只由 Core 原 QuotaState 负责，不进入 cursor 的 scope workspace 计费。
pub enum InlineLinkStorageStep {
    Ready {
        reply: InlineLinkStorageReply,
        work: GlobalWork,
    },
    Progress {
        work: GlobalWork,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Missing 只能来自原 Global 的真实 EOF，不是当前 viewport 没有找到定义。
pub enum InlineReferenceResolution {
    Resolved,
    Missing,
}

#[derive(Clone, Debug)]
/// 只有同一游标实际读取、闭合且校验过的 label 能创建引用请求。
/// reference 包含原 closing bracket；decoder 依它判断 label 真终点，不拿 frame 尾作 EOF。
pub struct VerifiedInlineReferenceRequest {
    pub(super) revision: u64,
    pub(super) source_end: usize,
    pub(super) block: Range<usize>,
    pub(super) record: InlineLinkRecordId,
    pub(super) source: Range<usize>,
    pub(super) label: Range<usize>,
    pub(super) reference: Range<usize>,
    pub(super) kind: InlineReferenceKind,
    // 仅 scoped 原请求携同一结构 seed；浅 Arc 身份防止裸坐标伪造另一 scope 的续行权限。
    pub(super) scope: Option<std::sync::Arc<super::super::VerifiedInlineSemanticSeed>>,
}
impl PartialEq for VerifiedInlineReferenceRequest {
    fn eq(&self, other: &Self) -> bool {
        // seed保真实祖先捕获，不能按内容值比较伪造scope等价；同一请求浅clone才保这份Arc身份。
        // 其它字段仍核完整原暂停身份，不能仅凭同一文件record或同一个key恢复另一操作。
        let same_scope = match (&self.scope, &other.scope) {
            (None, None) => true,
            (Some(left), Some(right)) => std::sync::Arc::ptr_eq(left, right),
            _ => false,
        };
        same_scope
            && self.revision == other.revision
            && self.source_end == other.source_end
            && self.block == other.block
            && self.record == other.record
            && self.source == other.source
            && self.label == other.label
            && self.reference == other.reference
            && self.kind == other.kind
    }
}
impl Eq for VerifiedInlineReferenceRequest {}
impl VerifiedInlineReferenceRequest {
    /// 原 Source 修订；还需调用方的真实健康/取消守卫，数字不能复活已撤销源。
    pub fn source_revision(&self) -> u64 {
        self.revision
    }
    /// Doc 的真实 byte 末端，不是本标签末端或借入 frame 末端。
    pub fn source_end(&self) -> usize {
        self.source_end
    }
    /// 原 seed 的完整闭合 inline 域，不能用 requested viewport 重建一个假块。
    pub fn block_range(&self) -> Range<usize> {
        self.block.clone()
    }
    /// 同一文件中已实际创建的 opener 身份，恢复时必须与原暂停请求完全一致。
    pub fn record_id(&self) -> InlineLinkRecordId {
        self.record
    }
    /// 原完整 owner；选择显隐不能把它缩为可见 label 的交集。
    pub fn source_range(&self) -> Range<usize> {
        self.source.clone()
    }
    /// 原 label 不含外括号；其中的 code/强调仍需按原匹配域单独产生。
    pub fn label_range(&self) -> Range<usize> {
        self.label.clone()
    }
    /// key 内容起点至 closing bracket 之后；包含真闭合，以供分帧 scanner 判定完成。
    pub fn reference_range(&self) -> Range<usize> {
        self.reference.clone()
    }
    /// 原 Parser 实际选择的引用形式，不以 key 是否相同推测 shortcut/collapsed。
    pub fn kind(&self) -> InlineReferenceKind {
        self.kind
    }
    /// 只恢复 label 的原规范化规则；Unicode folding 和目标库存仍归唯一 Queries。
    pub fn reference_decoder(&self) -> InlineReferenceDecoder {
        InlineReferenceDecoder::new(self)
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
/// 直接目标携真实原文范围；引用只携同请求身份，让唯一 Queries 共享已规范化目标。
pub enum InlineLinkTarget {
    Direct {
        destination: Range<usize>,
        title: Option<Range<usize>>,
    },
    Reference {
        request: VerifiedInlineReferenceRequest,
    },
}

#[derive(Clone, Debug)]
/// 只有原顺序游标及完整闭合目标能签发；范围不是一个重新解析的局部 Paragraph。
pub struct VerifiedInlineLink {
    pub(super) revision: u64,
    pub(super) source_end: usize,
    pub(super) source: Range<usize>,
    pub(super) label: Range<usize>,
    pub(super) kind: InlineLinkKind,
    pub(super) target: InlineLinkTarget,
}

impl VerifiedInlineLink {
    /// 与原Source同一修订；消费者还须核其真实健康，数字相同不创造新Source权限。
    pub fn source_revision(&self) -> u64 {
        self.revision
    }
    /// 原Doc真实byte末端，不是owner末端或这次借入帧末端。
    pub fn source_end(&self) -> usize {
        self.source_end
    }
    /// 完整 owner 用于选择显隐；不能把可见 label 交集当成整个链接。
    pub fn source_range(&self) -> Range<usize> {
        self.source.clone()
    }
    /// 完整显示label不含外括号；长label不会被requested viewport交集截成新owner。
    pub fn label_range(&self) -> Range<usize> {
        self.label.clone()
    }
    /// Image 保持原 alt/marker 样式，但不因此获得外层点击目标。
    pub fn kind(&self) -> InlineLinkKind {
        self.kind
    }
    /// 引用只携原请求身份；真实目标由 Core 同一 Queries 的 Resolved 值浅共享。
    pub fn target(&self) -> InlineLinkTarget {
        self.target.clone()
    }
    /// 原直接目标范围；引用不伪造一个空目标范围让消费者错误执行解码。
    pub fn destination_range(&self) -> Option<Range<usize>> {
        match &self.target {
            InlineLinkTarget::Direct { destination, .. } => Some(destination.clone()),
            _ => None,
        }
    }
    pub fn title_range(&self) -> Option<Range<usize>> {
        match &self.target {
            InlineLinkTarget::Direct { title, .. } => title.clone(),
            _ => None,
        }
    }
    /// 目标沿真实范围分帧规范化；不复制 label，也不给 URL 新增长度拒绝门。
    pub fn destination_decoder(&self) -> Option<InlineLinkDestinationDecoder> {
        let destination = self.destination_range()?;
        Some(InlineLinkDestinationDecoder {
            revision: self.revision,
            source_end: self.source_end,
            end: destination.end,
            next: destination.start,
            stream: crate::scanners::UnescapeStream::new(false),
            complete: false,
            failure: None,
        })
    }
}

#[derive(Debug)]
/// 输出只是本量子已规范化的完整UTF8；String总容量和既有target上限仍由实际Core拥有。
// work包含本次扫描、实体比较、UTF8检查与本地copy；Core另收真实借帧和目标库存费用。
// written只授权output的这段完整UTF8；complete以前的片段不能单独作为可激活目标。
pub struct InlineLinkDestinationStep {
    pub work: GlobalWork,
    pub written: usize,
    pub complete: bool,
}

#[derive(Debug)]
// 不持Doc、mmap、Reader或源帧，取消不会留下第二正文owner；错误首因只在本实例固定。
// utf8只跨片保留一个未完成scalar，实体carry由原共享内核拥有，二者均有固定上界。
pub struct InlineLinkDestinationDecoder {
    revision: u64,
    source_end: usize,
    end: usize,
    next: usize,
    stream: crate::scanners::UnescapeStream,
    complete: bool,
    failure: Option<Error>,
}

impl InlineLinkDestinationDecoder {
    /// 返回真实已消费端点；有待排出输出时可已到end，调用方此时无需再做源I/O。
    pub fn next_byte(&self) -> usize {
        self.next
    }

    /// actual target末端已闭合，故可以空帧归还carry；real_eof仍只承认原Doc真实EOF。
    /// 源撤权、取消、UTF8和原首错都不能以部分规范化字符串冒充完成。
    ///
    /// 原authority先于任何写入；包括已complete的重复查询，也不能借撤销票继续取值。
    /// output来自消费者固定栈，最小8B足以容纳两个scalar；不在这里分配目标String。
    /// 输入扫描和输出检查共享budget，保留CONTROL及最多一个scalar的固定复制费用。
    /// Core仍按本步实际借帧长度付I/O，不应把consumed等同于整帧实际读取长度。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        authority: GlobalSourceAuthority,
        output: &mut [u8],
        budget: usize,
    ) -> Result<InlineLinkDestinationStep, Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = (|| {
            if authority.cancelled {
                return Err(Error::Cancelled);
            }
            if authority.current_revision != self.revision {
                return Err(Error::SourceSuperseded);
            }
            if !(2048..=MAX_FRONTIER_STEP_BYTES).contains(&budget) || output.len() < 8 {
                return Err(Error::InvalidBudget);
            }
            crate::source_frontier::admitted_bytes(
                &frame,
                self.revision,
                self.source_end,
                self.next,
                budget,
            )?;
            if self.complete {
                return Ok(InlineLinkDestinationStep {
                    work: GlobalWork::default(),
                    written: 0,
                    complete: true,
                });
            }
            // source_end是Doc权限，end是已闭合目标；借帧可能覆盖其后字节但决不消费。
            // 终止理由来自原proof，real_end不是根据一个暂时短帧猜出的“当前输入完毕”。
            let count = frame.bytes.len().min(self.end - self.next);
            let (consumed, written, transitions, complete) = self
                .stream
                .feed_utf8(
                    &frame.bytes[..count],
                    self.next + count == self.end,
                    output,
                    (budget - CONTROL - 4) / 2,
                )
                .map_err(|_| Error::SourceUtf8)?;
            // 原共享入口逐片保证 UTF8，也实际计入本片验证。只有实际消费端点推进，
            // 实体尚待排出时可以借空 frame，不把 output 满误当完整目标。
            self.next += consumed;
            self.complete = complete;
            Ok(InlineLinkDestinationStep {
                work: GlobalWork {
                    byte_reads: consumed,
                    transitions: CONTROL + transitions,
                },
                written,
                complete,
            })
        })();
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }
}

#[derive(Debug)]
/// 引用 key 仍归 Core 唯一 Queries；这里仅复用原 Parser 的 label scanner，不折叠、不建第二表。
/// 固定 UTF8 carry 只保跨片一个 scalar，原请求身份和首错在整个恢复过程中保持不变。
pub struct InlineReferenceDecoder {
    revision: u64,
    source_end: usize,
    start: usize,
    end: usize,
    scanner: crate::linklabel::LabelScanner,
    scope: Option<std::sync::Arc<super::super::VerifiedInlineSemanticSeed>>,
    prefix: Option<crate::global_frontier::ReferencePrefixScanner>,
    allocated: usize,
    utf8: [u8; 4],
    utf8_len: usize,
    complete: bool,
    failure: Option<Error>,
}
impl InlineReferenceDecoder {
    fn new(request: &VerifiedInlineReferenceRequest) -> Self {
        Self {
            revision: request.revision,
            source_end: request.source_end,
            start: request.reference.start,
            end: request.reference.end,
            scanner: crate::linklabel::LabelScanner::new(),
            scope: request.scope.clone(),
            prefix: None,
            allocated: 0,
            utf8: [0; 4],
            utf8_len: 0,
            complete: false,
            failure: None,
        }
    }
    /// 真Global定义已由原标签/colon词法认证；decoder仍逐帧重读原key，不接受外造label。
    pub(in crate::global_frontier) fn from_footnote(
        definition: &crate::global_frontier::VerifiedGlobalFootnoteDefinition,
    ) -> Self {
        let reference = definition.reference_range();
        Self {
            revision: definition.source_revision(),
            source_end: definition.source_end(),
            start: reference.start,
            end: reference.end,
            scanner: crate::linklabel::LabelScanner::new(),
            scope: None,
            prefix: None,
            allocated: 0,
            utf8: [0; 4],
            utf8_len: 0,
            complete: false,
            failure: None,
        }
    }
    pub fn next_byte(&self) -> usize {
        self.prefix
            .as_ref()
            .map_or(self.start + self.scanner.offset, |prefix| {
                prefix.next_byte()
            })
    }
    /// 实际前缀游标取得的累计结构页，Core 按每步增量纳入原元数据配额。
    pub fn allocated_workspace_bytes(&self) -> usize {
        self.allocated
    }

    /// 完整 label 已由原扫描认证，decoder 不以更小短帧推断闭合或缺失定义。
    /// 每步读/输出均有界；Core 仍另按实际借帧长度收 Source I/O，并正常收单 key 的有限库存。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        authority: GlobalSourceAuthority,
        output: &mut [u8],
        budget: usize,
    ) -> Result<InlineLinkDestinationStep, Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = (|| {
            if authority.cancelled {
                return Err(Error::Cancelled);
            }
            if authority.current_revision != self.revision {
                return Err(Error::SourceSuperseded);
            }
            if !(2048..=MAX_FRONTIER_STEP_BYTES).contains(&budget) || output.len() < 8 {
                return Err(Error::InvalidBudget);
            }
            crate::source_frontier::admitted_bytes(
                &frame,
                self.revision,
                self.source_end,
                self.next_byte(),
                budget,
            )?;
            if self.complete {
                return Ok(InlineLinkDestinationStep {
                    work: GlobalWork::default(),
                    written: 0,
                    complete: true,
                });
            }
            // 容器label与Root label共用原语法，只有真实NeedLine才能让原seed启动续行前缀。
            // 前缀步骤只回实际工作，不输出identifier；complete后才恢复同一个LabelScanner。
            if let Some(prefix) = self.prefix.as_mut() {
                use crate::global_frontier::ReferencePrefixStep;
                let before = prefix.allocated_workspace_bytes();
                let step = prefix.advance(
                    SourceFrame {
                        source_revision: frame.source_revision,
                        byte_start: frame.byte_start,
                        bytes: frame.bytes,
                        real_eof: frame.real_eof,
                    },
                    authority,
                    budget,
                )?;
                // 新取得的真实结构页累计交回Core配额；归还旧prefix不把燃料计数倒退或重复领取。
                self.allocated += prefix.allocated_workspace_bytes() - before;
                let (work, complete) = match step {
                    ReferencePrefixStep::Progress { work, .. }
                    | ReferencePrefixStep::NeedInput { work, .. } => (work, None),
                    ReferencePrefixStep::Complete { work, prefix_bytes } => {
                        (work, Some(prefix_bytes))
                    }
                };
                if let Some(bytes) = complete {
                    self.scanner
                        .continue_line(bytes)
                        .ok_or(Error::InvalidFrame)?;
                    self.prefix = None;
                }
                return Ok(InlineLinkDestinationStep {
                    work,
                    written: 0,
                    complete: false,
                });
            }
            let available = frame.bytes.len().min(self.end - self.next_byte());
            // 一个输入 byte 最多触发 escape/空白转移和两个输出 byte，先给真实循环及 UTF8 验证留足费用。
            let count = available.min((budget - CONTROL - 16) / 8);
            output[..self.utf8_len].copy_from_slice(&self.utf8[..self.utf8_len]);
            let before = self.scanner.transitions;
            let real_end = self.next_byte() + count == self.end;
            let (consumed, written, step) = self.scanner.feed(
                &frame.bytes[..count],
                real_end,
                &mut output[self.utf8_len..],
            );
            let transitions = self.scanner.transitions - before;
            let complete = match step {
                crate::linklabel::LabelStep::Complete => true,
                crate::linklabel::LabelStep::Rejected => return Err(Error::InvalidFrame),
                crate::linklabel::LabelStep::NeedLine => {
                    if let Some(scope) = self.scope.as_ref() {
                        // 引用key只去原容器标记，空白仍由LabelScanner规范化，不能套正文AllSpace裁掉key。
                        // 原scope保在请求浅Arc里，不由Core根据range或文本猜quote/list层级。

                        let after_eol = self.start + self.scanner.offset;
                        self.prefix = Some(scope.continuation_prefix_scanner(
                            after_eol,
                            crate::global_frontier::ReferencePrefixPurpose::ContainerOnly,
                        )?);
                    } else {
                        self.scanner.continue_line(0).ok_or(Error::InvalidFrame)?;
                    }
                    false
                }
                _ => false,
            };
            let produced = self.utf8_len + written;
            let valid = match std::str::from_utf8(&output[..produced]) {
                Ok(_) => produced,
                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                Err(_) => return Err(Error::SourceUtf8),
            };
            self.utf8_len = produced - valid;
            if self.utf8_len > 3 || complete && self.utf8_len != 0 {
                return Err(Error::SourceUtf8);
            }
            self.utf8[..self.utf8_len].copy_from_slice(&output[valid..produced]);
            self.complete = complete;
            Ok(InlineLinkDestinationStep {
                work: GlobalWork {
                    byte_reads: consumed,
                    transitions: CONTROL + transitions + produced,
                },
                written: valid,
                complete,
            })
        })();
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }
}
