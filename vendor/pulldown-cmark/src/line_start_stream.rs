// Copyright 2015 Google Inc. All rights reserved.
// 本地分段状态机从固定 pulldown-cmark 0.13.4 的 LineStart 逐分支迁移；
// 原始 MIT 许可与署名保留在本目录 LICENSE。新增注释说明分段权限，不另造语法。

//! 不持正文的 LineStart 操作续体；lookahead 锚点与最终提交的 ix 明确分离。
//!
//! 源owner必须提供同一已验证snapshot的revision/origin/end，state沿原LineStart或
//! 上一个Complete传递。本层只识别ASCII结构byte，不重新扫描全文验证UTF8，也不把
//! frame末尾当成源行/EOF；普通CR行与metadata的raw-LF行边界仍由外层语法决定。

/// 每个输入 frame 和 step 的读取/转换都受同一固定上限约束。
pub(crate) const MAX_LINE_START_STEP: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 与原 LineStart 相同的四个标量；字节偏移相对同一个 source_origin。
pub(crate) struct LineStartState {
    pub(crate) ix: usize,
    pub(crate) tab_start: usize,
    pub(crate) spaces_remaining: usize,
    pub(crate) min_hrule_offset: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 一次续体只完成一项原操作；外层仍决定容器优先级和源物理行边界。
pub(crate) enum LineStartOperation {
    Space(usize),
    AllSpace,
    AtEol,
    BlankLine,
    Quote,
    Definition(usize),
    List(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 返回值与原方法逐项对应；Space 返回未消费的逻辑空格数。
pub(crate) enum LineStartValue {
    SpaceRemaining(usize),
    Unit,
    Bool(bool),
    BlankLine(Option<usize>),
    Definition(Option<usize>),
    List(Option<(u8, u64, usize)>),
}

/// 只借本次有界输入；可在 UTF-8 scalar 或 CRLF 中间切分，不保存 frame。
pub(crate) struct LineStartFrame<'a> {
    pub(crate) source_revision: u64,
    pub(crate) byte_start: usize,
    pub(crate) bytes: &'a [u8],
    pub(crate) real_eof: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 读取计数包含 lookahead 和必要重读，不能把提交字节数冒充真实工作量。
pub(crate) struct LineStartWork {
    pub(crate) byte_reads: usize,
    pub(crate) transitions: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// NeedInput 没有 EOF 权限；Progress 只表示本 step 预算用完。
pub(crate) enum LineStartProgress {
    Progress {
        next_byte: usize,
    },
    NeedInput {
        next_byte: usize,
    },
    Complete {
        state: LineStartState,
        value: LineStartValue,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 每次返回同时携带本 step 工作量；累计值只增不减。
pub(crate) struct LineStartStep {
    pub(crate) progress: LineStartProgress,
    pub(crate) work: LineStartWork,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 输入身份/预算先验失败不推进状态；数值溢出拒绝而不伪造缩小的 indent。
pub(crate) enum LineStartError {
    InvalidState,
    InvalidFrame,
    InvalidBudget,
    ArithmeticOverflow,
}

#[derive(Clone, Copy, Debug)]
/// Space 的返回点是有限枚举，不用递归栈或带正文的闭包保存 continuation。
enum AfterSpace {
    Return,
    Quote,
    DefinitionProbe,
    DefinitionFinish,
    ListFirst,
    ListExtra,
}

#[derive(Clone, Copy, Debug)]
/// 每个状态转换至多读取一个源字节，长 hrule/blank lookahead 也不能在内部跑整行。
enum Phase {
    Space {
        requested: usize,
        remaining: usize,
        after: AfterSpace,
    },
    AllSpace,
    AtEol,
    BlankLine,
    BlankLineCr,
    Quote,
    Definition,
    List,
    Digits,
    Hrule,
    HruleCr,
    ListEol,
    Blank,
    Complete(LineStartValue),
}

#[derive(Debug)]
/// 只保存有限标量和 continuation；无 Vec、String、slice、映射或 source 强 owner。
pub(crate) struct LineStartStream {
    revision: u64,
    source_origin: usize,
    source_end: usize,
    initial: LineStartState,
    state: LineStartState,
    marker_save: LineStartState,
    phase: Phase,
    operation: LineStartOperation,
    probe: usize,
    probe_start: usize,
    marker: u8,
    marker_value: u64,
    marker_indent: usize,
    hrule_count: u8,
    total: LineStartWork,
}

impl LineStartStream {
    /// 同一源 revision/origin/end 是全部操作的权限；state 沿原 LineStart clone 语义传递。
    pub(crate) fn new(
        revision: u64,
        source_origin: usize,
        source_end: usize,
        state: LineStartState,
        operation: LineStartOperation,
    ) -> Result<Self, LineStartError> {
        let Some(source_len) = source_end.checked_sub(source_origin) else {
            // 源区间倒置没有任何合法锚点，先于状态写入拒绝。
            return Err(LineStartError::InvalidState);
        };
        if state.ix > source_len || state.tab_start > state.ix || state.spaces_remaining > 3 {
            // tab 余宽最多三个；不能借任意标量伪造超范围 source cursor。
            return Err(LineStartError::InvalidState);
        }
        let phase = match operation {
            // 原 scan_space 失败也会提交已经消费的空格，不能恢复 initial。
            LineStartOperation::Space(requested) => Phase::Space {
                requested,
                remaining: requested,
                after: AfterSpace::Return,
            },
            // scan_all_space 刻意不维护 tab_start，与原 interior 扫描完全相同。
            LineStartOperation::AllSpace => Phase::AllSpace,
            // 只读EOL仍需真实源byte或EOF权限，不能从frame长度推断。
            LineStartOperation::AtEol => Phase::AtEol,
            // 原 scan_blank_line 的 no_nl 空白与 CRLF 也经过同一可续扫原语。
            LineStartOperation::BlankLine => Phase::BlankLine,
            // quote只有一个marker和至多一个逻辑空格，不自行扫描下一物理行。
            LineStartOperation::Quote => Phase::Quote,
            // definition保留原五格probe和save/restore顺序。
            LineStartOperation::Definition(_) => Phase::Definition,
            // list先排除thematic break，再处理bullet/ordered和空白lookahead。
            LineStartOperation::List(_) => Phase::List,
        };
        let mut current = state;
        if operation == LineStartOperation::AllSpace {
            // 原方法先丢弃尚未消耗的 tab 逻辑余宽，再按原始 ASCII byte 前进。
            current.spaces_remaining = 0;
        }
        Ok(Self {
            revision,
            source_origin,
            source_end,
            initial: state,
            state: current,
            marker_save: state,
            phase,
            operation,
            probe: state.ix,
            probe_start: state.ix,
            marker: 0,
            marker_value: 0,
            marker_indent: 0,
            hrule_count: 0,
            total: LineStartWork::default(),
        })
    }

    /// 测试核累计实际读取/转换；生产消费者直接累计每次step.work，不增加第二套计量入口。
    #[cfg(test)]
    pub(crate) fn total_work(&self) -> LineStartWork {
        self.total
    }

    /// 只表示下一次所需源锚点；Complete.state.ix 才是上游提交的 cursor。
    pub(crate) fn next_byte(&self) -> usize {
        self.source_origin + self.read_offset().unwrap_or(self.state.ix)
    }

    /// 按独立 bytes/transitions 预算执行；frame 缺字节时保留同一 continuation，不解释为 EOF。
    ///
    /// 单个List操作最多一次hrule和一次blank线性probe，加有限数字/缩进阶段，因此
    /// byte读取≤2N+64、转换≤读取+32（N是剩余源长）；这不是整个document producer
    /// 的总工作证明。每次转换至多读一个byte，每step两个计量分别受调用方上限约束。
    pub(crate) fn advance(
        &mut self,
        frame: LineStartFrame<'_>,
        byte_budget: usize,
        transition_budget: usize,
    ) -> Result<LineStartStep, LineStartError> {
        if byte_budget == 0
            || transition_budget == 0
            || byte_budget > MAX_LINE_START_STEP
            || transition_budget > MAX_LINE_START_STEP
        {
            // 零预算没有推进保证；超预算不得把外层有界接口偷换成整行操作。
            return Err(LineStartError::InvalidBudget);
        }
        let frame_end = frame.byte_start.checked_add(frame.bytes.len());
        if frame.source_revision != self.revision
            || frame.bytes.len() > MAX_LINE_START_STEP
            || frame.byte_start < self.source_origin
            || !frame_end.is_some_and(|end| end <= self.source_end)
            || (frame.real_eof && frame_end != Some(self.source_end))
        {
            // 先核 revision、范围与真实 EOF 声明；错误 frame 不消费一个字节。
            return Err(LineStartError::InvalidFrame);
        }
        let mut work = LineStartWork::default();
        loop {
            if let Phase::Complete(value) = self.phase {
                // 完成值可重复读取；不重跑 lookahead，也不借最后一个 frame 的寿命。
                return Ok(LineStartStep {
                    progress: LineStartProgress::Complete {
                        state: self.state,
                        value,
                    },
                    work,
                });
            }
            let next_byte = self.next_byte();
            if work.transitions == transition_budget
                || (self.read_offset().is_some() && work.byte_reads == byte_budget)
            {
                // 预算耗尽区别于缺输入；下一 step 保留相同阶段和单调累计工作量。
                return Ok(LineStartStep {
                    progress: LineStartProgress::Progress { next_byte },
                    work,
                });
            }
            let input = if let Some(offset) = self.read_offset() {
                let absolute = self.source_origin + offset;
                if absolute == self.source_end && frame.real_eof {
                    // 只有本 frame 真实到源末尾，None 才代表 EOF。
                    None
                } else if absolute >= frame.byte_start && absolute < frame_end.unwrap() {
                    // 读取只在本次借出的 frame 内；每个状态转换至多收费一个 byte。
                    work.byte_reads += 1;
                    self.total.byte_reads = self
                        .total
                        .byte_reads
                        .checked_add(1)
                        .ok_or(LineStartError::ArithmeticOverflow)?;
                    Some(frame.bytes[absolute - frame.byte_start])
                } else {
                    // frame 末尾或 lookahead 回到旧 anchor：请求精确位置，不能借缓存猜测源内容。
                    return Ok(LineStartStep {
                        progress: LineStartProgress::NeedInput {
                            next_byte: absolute,
                        },
                        work,
                    });
                }
            } else {
                // 逻辑 tab 余宽和有限返回点不读取源，但仍收费一次 transition。
                None
            };
            work.transitions += 1;
            self.total.transitions = self
                .total
                .transitions
                .checked_add(1)
                .ok_or(LineStartError::ArithmeticOverflow)?;
            self.transition(input)?;
        }
    }

    /// 给外层明确的读取 anchor；纯逻辑步骤不为取得字节而强迫重复 frame。
    fn read_offset(&self) -> Option<usize> {
        match self.phase {
            Phase::Space { remaining, .. }
                if remaining == 0 || self.state.spaces_remaining != 0 =>
            {
                None
            }
            Phase::Space { .. }
            | Phase::AllSpace
            | Phase::AtEol
            | Phase::Quote
            | Phase::Definition
            | Phase::List
            | Phase::ListEol => Some(self.state.ix),
            Phase::Digits if self.probe - self.probe_start >= 10 => None, // 已达原数字上限，无需多读新frame才能拒绝。
            Phase::Digits
            | Phase::Hrule
            | Phase::HruleCr
            | Phase::Blank
            | Phase::BlankLine
            | Phase::BlankLineCr => Some(self.probe),
            Phase::Complete(_) => None,
        }
    }

    /// 与原方法同序推进一次；所有跨 frame 中间态只保常数标量。
    fn transition(&mut self, input: Option<u8>) -> Result<(), LineStartError> {
        match self.phase {
            Phase::Space {
                requested,
                mut remaining,
                after,
            } => {
                if self.state.spaces_remaining != 0 {
                    // 已读过的 tab 余宽不用再读源，部分消费仍是原始 tab 的同一列。
                    let taken = remaining.min(self.state.spaces_remaining);
                    remaining -= taken;
                    self.state.spaces_remaining -= taken;
                } else if remaining != 0 {
                    match input {
                        Some(b' ') => {
                            // 真实ASCII空格恰好占一列、一byte，不引入tab余宽。
                            self.state.ix += 1;
                            remaining -= 1;
                        }
                        Some(b'\t') => {
                            // tab 停靠点依赖原 byte 距离，不按 Unicode 字符或固定四空格猜算。
                            let width = 4 - (self.state.ix - self.state.tab_start) % 4;
                            self.state.ix += 1;
                            self.state.tab_start = self.state.ix;
                            let taken = remaining.min(width);
                            remaining -= taken;
                            self.state.spaces_remaining = width - taken;
                        }
                        _ => {
                            // 非空格和真实 EOF 都停止，但已消费的前缀不回滚。
                            return self.after_space(requested, remaining, after);
                        }
                    }
                }
                if remaining == 0 {
                    // 请求满足就返回；不多读下一个字节来“确认”已知事实。
                    self.after_space(requested, remaining, after)?;
                } else {
                    // 余量仍存在，下一次转换才继续读；超长缩进不会隐藏内部循环。
                    self.phase = Phase::Space {
                        requested,
                        remaining,
                        after,
                    };
                }
            }
            Phase::AllSpace => match input {
                Some(b' ' | b'\t') => self.state.ix += 1,
                _ => self.phase = Phase::Complete(LineStartValue::Unit), // 原方法只接受空格/tab，其他 ASCII whitespace 不消费。
            },
            Phase::AtEol => {
                // 只读测试不消费CR/LF，None只可能来自已获权限的真实EOF。
                self.phase = Phase::Complete(LineStartValue::Bool(matches!(
                    input,
                    None | Some(b'\r' | b'\n')
                )))
            }
            Phase::BlankLine => match input {
                Some(byte) if super::scanners::is_ascii_whitespace_no_nl(byte) => self.probe += 1,
                Some(b'\r') => {
                    self.probe += 1;
                    self.phase = Phase::BlankLineCr;
                }
                Some(b'\n') => {
                    self.probe += 1;
                    self.complete_blank_line();
                }
                None => self.complete_blank_line(),
                _ => {
                    // 非空白候选只读失败；保留原 LineStart 状态，让原分类从同一位置继续。
                    self.state = self.initial;
                    self.phase = Phase::Complete(LineStartValue::BlankLine(None));
                }
            },
            Phase::BlankLineCr => {
                if input == Some(b'\n') {
                    self.probe += 1;
                }
                self.complete_blank_line();
            }
            Phase::Quote => {
                if input == Some(b'>') {
                    // marker 本身先消费；其后至多一个逻辑空格即使不足也接受 quote。
                    self.state.ix += 1;
                    self.start_space(1, AfterSpace::Quote);
                } else {
                    // 原 scan_ch 失败无变化。
                    self.phase = Phase::Complete(LineStartValue::Bool(false));
                }
            }
            Phase::Definition => {
                if input == Some(b':') {
                    // 冒号后的 probe 可以跨 frame，但真正提交前必须恢复这个精确 save。
                    self.state.ix += 1;
                    self.marker_save = self.state;
                    self.start_space(5, AfterSpace::DefinitionProbe);
                } else {
                    // 非 definition marker 恢复全部 scalar，不能保留 speculative 空格。
                    self.state = self.initial;
                    self.phase = Phase::Complete(LineStartValue::Definition(None));
                }
            }
            Phase::List => self.start_list(input)?,
            Phase::Digits => {
                if self.probe - self.probe_start >= 10 {
                    // 原 ordered marker 最多九个数字加一个分隔符；达到上限完整回滚。
                    self.reject_list();
                } else if let Some(byte) = input {
                    self.probe += 1;
                    if byte.is_ascii_digit() {
                        // 最多十个十进制数字，数值必然落在 u64 范围。
                        self.marker_value = self.marker_value * 10 + u64::from(byte - b'0');
                    } else if matches!(byte, b')' | b'.') {
                        // 原序列包含 delimiter 后才更新 ix；数字 probe 本身未发布。
                        self.marker = byte;
                        self.state.ix = self.probe;
                        let LineStartOperation::List(indent) = self.operation else {
                            unreachable!()
                        };
                        self.marker_indent = indent
                            .checked_add(1 + self.probe - self.probe_start)
                            .ok_or(LineStartError::ArithmeticOverflow)?;
                        self.start_space(1, AfterSpace::ListFirst);
                    } else {
                        // 非数字/分隔符使整个 marker 失败，不能丢掉原始缩进或 tab 余宽。
                        self.reject_list();
                    }
                } else {
                    // 真实EOF前没有分隔符也不是列表，保留原四个scalar。
                    self.reject_list();
                }
            }
            Phase::Hrule => match input {
                Some(b'\r') => {
                    self.probe += 1;
                    self.phase = Phase::HruleCr;
                } // 少于三标记时 Err offset 要区分 CRLF，等待下一 byte。
                Some(b'\n') => {
                    // LF结束hrule probe，其byte宽度计入原相对Err offset。
                    self.probe += 1;
                    self.finish_hrule()?;
                }
                Some(byte) if byte == self.marker => {
                    // 只需区分不足三次/至少三次，饱和计数不随超长行增长。
                    self.hrule_count = (self.hrule_count + 1).min(3);
                    self.probe += 1;
                }
                // hrule内部允许空格/tab，probe仍只按真实byte前进。
                Some(b' ' | b'\t') => self.probe += 1,
                None => self.finish_hrule()?, // 真实 EOF 等同原 scan_hrule 的 slice 耗尽。
                _ => self.fail_hrule(self.probe - self.probe_start)?, // 首个非成员的位置原样写 min_hrule_offset，保留上游相对值语义。
            },
            Phase::HruleCr => {
                if input == Some(b'\n') {
                    // CRLF 只在 probe 上作为两个 byte；非 LF 被窥视但仍属于后续源。
                    self.probe += 1;
                }
                self.finish_hrule()?;
            }
            Phase::ListEol => {
                if matches!(input, None | Some(b'\r' | b'\n')) {
                    // marker 后无逻辑空格仍可接受空项，EOF 必须有真实 frame 证明。
                    self.start_finish_list();
                } else {
                    // 正文紧跟 marker 且没有分隔空格，不是列表。
                    self.reject_list();
                }
            }
            Phase::Blank => match input {
                Some(byte) if super::scanners::is_ascii_whitespace_no_nl(byte) => self.probe += 1,
                None | Some(b'\r' | b'\n') => self.complete_list()?, // 只需 blank 是真；probe 不提交到 state.ix。
                _ => {
                    self.state = self.marker_save;
                    self.start_space(4, AfterSpace::ListExtra);
                } // 非空行按原规则最多追加三个空格，四格则回到 save。
            },
            Phase::Complete(_) => (), // advance 在到达此状态后直接返回，不重跑任何源读取。
        }
        Ok(())
    }

    /// 固定的空格子操作返回点；没有递归、动态闭包或源字符串快照。
    fn start_space(&mut self, requested: usize, after: AfterSpace) {
        self.phase = Phase::Space {
            requested,
            remaining: requested,
            after,
        };
    }

    fn complete_blank_line(&mut self) {
        // 真 EOF 允许空尾行；任意 frame 尾没有该权限。count 包含原真实 CR/LF/CRLF。
        self.state.ix = self.probe;
        self.phase = Phase::Complete(LineStartValue::BlankLine(Some(
            self.probe - self.initial.ix,
        )));
    }

    /// 子操作结束后保留原方法中每个 save/restore 的精确时序。
    fn after_space(
        &mut self,
        requested: usize,
        remaining: usize,
        after: AfterSpace,
    ) -> Result<(), LineStartError> {
        match after {
            AfterSpace::Return => {
                // 原scan_space_inner返回未满足余量，并提交此前已消费的空格。
                self.phase = Phase::Complete(LineStartValue::SpaceRemaining(remaining))
            }
            AfterSpace::Quote => self.phase = Phase::Complete(LineStartValue::Bool(true)), // quote 不要求 marker 后一定有空格。
            AfterSpace::DefinitionProbe => {
                // 原算法先 probe 五格，再从冒号后 save 重读一格或五格；不提交 lookahead cursor。
                self.state = self.marker_save;
                self.start_space(
                    if remaining == 0 { 1 } else { 5 },
                    AfterSpace::DefinitionFinish,
                );
            }
            AfterSpace::DefinitionFinish => {
                let LineStartOperation::Definition(indent) = self.operation else {
                    unreachable!()
                };
                let result = indent
                    .checked_add(1)
                    .and_then(|value| value.checked_add(requested - remaining))
                    .ok_or(LineStartError::ArithmeticOverflow)?;
                self.phase = Phase::Complete(LineStartValue::Definition(Some(result)));
            }
            AfterSpace::ListFirst => {
                if remaining == 0 {
                    // 成功消费第一个分隔空格；接下来的 blank probe 不能改变此提交位置。
                    self.start_finish_list();
                } else {
                    // 未消费分隔空格仍可能是空项，需要单独核真实 EOL/EOF。
                    self.phase = Phase::ListEol;
                }
            }
            AfterSpace::ListExtra => {
                if requested - remaining < 4 {
                    // 附加一至三格属于列表缩进；四格及以上按上游恢复 save。
                    self.marker_indent = self
                        .marker_indent
                        .checked_add(requested - remaining)
                        .ok_or(LineStartError::ArithmeticOverflow)?;
                } else {
                    // 四格及以上是原marker后save，不把code缩进吞进列表prefix。
                    self.state = self.marker_save;
                }
                self.complete_list()?;
            }
        }
        Ok(())
    }

    /// 列表开启优先于 marker 消费先判 thematic break，保持固定 0.13.4 规则。
    fn start_list(&mut self, input: Option<u8>) -> Result<(), LineStartError> {
        match input {
            Some(marker @ (b'-' | b'+' | b'*')) => {
                self.marker = marker;
                self.marker_value = 0;
                if self.state.ix >= self.state.min_hrule_offset {
                    if marker == b'+' || self.source_end - self.source_origin - self.state.ix < 3 {
                        // 原 scan_hrule 不接受加号，或 slice 总长不足三；Err offset 必须为零。
                        self.fail_hrule(0)?;
                    } else {
                        // lookahead 扫描任意长 line，但每 step 预算收费且只保标记数量。
                        self.probe_start = self.state.ix;
                        self.probe = self.state.ix;
                        self.hrule_count = 0;
                        self.phase = Phase::Hrule;
                    }
                } else {
                    // 原 min_hrule_offset 抑制重复检查，不能擅自重置成当前 absolute anchor。
                    self.consume_bullet()?;
                }
            }
            Some(byte) if byte.is_ascii_digit() => {
                // 数字 probe 尚不消费 state.ix；失败恢复整个原 LineStart。
                self.probe_start = self.state.ix;
                self.probe = self.state.ix + 1;
                self.marker_value = u64::from(byte - b'0');
                self.phase = Phase::Digits;
            }
            _ => self.reject_list(), // 其他字符和真实 EOF 均不是列表。
        }
        Ok(())
    }

    /// hrule 判定只需要是否成功；失败时仍须保留原返回的相对 offset。
    fn finish_hrule(&mut self) -> Result<(), LineStartError> {
        if self.hrule_count >= 3 {
            // thematic break 抢先，原 scan_list_marker 在此恢复所有 scalar 并返回 None。
            self.reject_list();
            Ok(())
        } else {
            self.fail_hrule(self.probe - self.probe_start)
        }
    }

    /// 原 min_hrule_offset 的赋值发生在成功列表 probe 内；最终失败仍整体恢复 initial。
    fn fail_hrule(&mut self, offset: usize) -> Result<(), LineStartError> {
        self.state.min_hrule_offset = offset;
        self.consume_bullet()
    }

    /// bullet 本身和第一个逻辑空格产生原 indent+2，随后走共享 finish 阶段。
    fn consume_bullet(&mut self) -> Result<(), LineStartError> {
        let LineStartOperation::List(indent) = self.operation else {
            unreachable!()
        };
        self.state.ix += 1;
        self.marker_indent = indent
            .checked_add(2)
            .ok_or(LineStartError::ArithmeticOverflow)?;
        self.start_space(1, AfterSpace::ListFirst);
        Ok(())
    }

    /// blank line lookahead 不消费提交 cursor；原标量 save 保留 tab 的尚余宽度。
    fn start_finish_list(&mut self) {
        self.marker_save = self.state;
        self.probe = self.state.ix;
        self.phase = Phase::Blank;
    }

    /// 列表结果只发布源 marker 的值和 indent，不创建 row/cell/AST 或正文副本。
    fn complete_list(&mut self) -> Result<(), LineStartError> {
        self.phase = Phase::Complete(LineStartValue::List(Some((
            self.marker,
            self.marker_value,
            self.marker_indent,
        ))));
        Ok(())
    }

    /// marker 失败整体回滚，区别于 scan_space 的部分消费行为。
    fn reject_list(&mut self) {
        self.state = self.initial;
        self.phase = Phase::Complete(LineStartValue::List(None));
    }
}

#[cfg(test)]
pub(crate) mod oracle;
#[cfg(test)]
mod tests;
