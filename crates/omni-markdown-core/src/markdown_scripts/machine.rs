//! 只保存当前cluster/run和配对栈头；任意深度的栈记录在有限分页owner中。
use super::{
    MarkdownScript as Script, MarkdownScriptBudget as Budget, MarkdownScriptError as Error,
    MarkdownScriptRun as Run, MarkdownScriptStep as Step, Result,
    properties::{self, Bracket, Set},
    store::{self, Frame, Store},
    unicode_data,
};
use crate::markdown_graphemes::MarkdownGraphemeScalarCursor;

struct Decoded {
    properties: properties::Properties,
    start_byte: usize,
    end_byte: usize,
    start_utf16: usize,
    end_utf16: usize,
    boundary: bool,
}
#[derive(Clone, Copy)]
struct Cluster {
    start_byte: usize,
    end_byte: usize,
    start_utf16: usize,
    end_utf16: usize,
    scripts: Option<Set>,
    primary: Option<u8>,
    anchored: bool,
    paragraph: bool,
    bracket: Option<Bracket>,
}
impl Cluster {
    fn new(value: &Decoded) -> Self {
        Self {
            start_byte: value.start_byte,
            end_byte: value.start_byte,
            start_utf16: value.start_utf16,
            end_utf16: value.start_utf16,
            scripts: None,
            primary: None,
            anchored: false,
            paragraph: false,
            bracket: None,
        }
    }
    fn add(&mut self, value: Decoded) -> Result<()> {
        let p = value.properties;
        if !self.anchored {
            if !matches!(p.primary, unicode_data::COMMON | unicode_data::INHERITED) {
                self.scripts = p.scripts;
                self.primary = Some(p.primary);
                self.anchored = true; // 包括Unknown；未知字符不伪装成Latin或任意通配脚本。
            } else if let Some(next) = p.scripts {
                self.scripts = match self.scripts {
                    Some(current) => Some(current.intersection(next).unwrap_or(current)),
                    None => Some(next),
                }; // 没有specific base时合并受限SCX；冲突仍不能拆开完整cluster。
            }
        } // 第一个specific base拥有完整SCX，后续mark/字素成员随base，不按其单字属性拆分。
        if let Some(bracket) = p.bracket {
            if self.bracket.is_some() {
                return Err(Error::CorruptMetadata);
            }
            self.bracket = Some(bracket); // Unicode18的配对标点均非Mark且GCB=Other，同一cluster最多一个。
        }
        self.paragraph |= p.paragraph;
        self.end_byte = value.end_byte;
        self.end_utf16 = value.end_utf16;
        Ok(())
    }
}
struct CurrentRun {
    start_byte: usize,
    start_utf16: usize,
    scripts: Option<Set>,
    primary: Option<u8>,
    // 只推进到完整cluster及其配对action都结束的位置，不以已解码/已借用byte代替。
    #[cfg(feature = "paged-shaping-frontier")]
    settled_end: (usize, usize),
}
impl CurrentRun {
    fn script(&self, preceding: Option<u8>) -> Result<u8> {
        let Some(set) = self.scripts else {
            return Ok(unicode_data::COMMON);
        };
        if let Some(script) = preceding.filter(|&script| set.contains(script)) {
            return Ok(script);
        }
        if let Some(script) = self.primary.filter(|&script| set.contains(script)) {
            return Ok(script);
        }
        set.first().ok_or(Error::CorruptMetadata) // IDs按ISO tag排序；空集合是损坏，不伪装Unknown。
    }
}
enum Phase {
    Locate,
    Resolve { target: usize, run: usize },
    Pop { target: usize },
    Merge,
    Open,
    Paragraph,
    Done,
}
struct Action {
    cluster: Cluster,
    phase: Phase,
}

pub(super) struct Machine {
    pub(super) byte: usize,
    utf16: usize,
    scalar_start: usize,
    pending_utf8: [u8; 4],
    used: usize,
    needed: usize,
    cursor: MarkdownGraphemeScalarCursor,
    held: Option<Decoded>,
    cluster: Option<Cluster>,
    action: Option<Action>,
    current: Option<CurrentRun>,
    preceding: Option<u8>,
    top: usize,
    same_kind: [usize; 65],
    finish_phase: u8,
    pub(super) ready: bool,
}
impl Default for Machine {
    fn default() -> Self {
        Self {
            byte: 0,
            utf16: 0,
            scalar_start: 0,
            pending_utf8: [0; 4],
            used: 0,
            needed: 0,
            cursor: MarkdownGraphemeScalarCursor::default(),
            held: None,
            cluster: None,
            action: None,
            current: None,
            preceding: None,
            top: store::NONE,
            same_kind: [store::NONE; 65],
            finish_phase: 0,
            ready: false,
        }
    }
}
impl Machine {
    #[cfg(all(test, feature = "paged-shaping-frontier"))]
    pub(super) fn has_pending_action(&self) -> bool {
        self.action.is_some()
    }

    /// 候选已收敛到唯一脚本时，未来兼容交集不能改变已结算前缀的脚本。
    /// 尚有held/action时不借出开放证明，避免半次配对或元数据操作被当作已完成。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn stable_open_prefix(&self) -> Option<Run> {
        if self.held.is_some() || self.action.is_some() {
            return None;
        }
        let run = self.current.as_ref()?;
        let scripts = run.scripts?;
        let script = scripts.first()?;
        if scripts != Set::singleton(script)
            || run.settled_end.0 <= run.start_byte
            || run.settled_end.1 <= run.start_utf16
        {
            return None; // Common/Inherited或多候选SCX继续等待原上下文，不能先选primary。
        }
        Some(Run {
            start_byte: run.start_byte,
            end_byte: run.settled_end.0,
            start_utf16: run.start_utf16,
            end_utf16: run.settled_end.1,
            script: Script(script),
        })
    }

    /// 仅沿原稳定SCX上下文续至真实正文末端；任何未结算动作、UTF8残余或实际括号栈均不得跳过。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn latin_suffix(&self, position: usize, content_end: usize) -> Option<Run> {
        let run = self.stable_open_prefix()?;
        (self.used == 0
            && self.utf16 <= content_end
            && run.end_utf16 <= content_end
            && self.top == store::NONE
            && run.script.0 == properties::get('a').primary
            && run.start_utf16 <= position
            && position < run.end_utf16)
            .then_some(run)
    }

    /// 同一当前run的ASCII后缀已证到真实B正文末端，只推进语义输入，不写闭run或封存Store。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn advance_proven_ascii(&mut self, byte: usize, utf16: usize) {
        self.current
            .as_mut()
            .expect("证明期间原Latn run未改变")
            .settled_end = (byte, utf16);
        self.byte = byte;
        self.utf16 = utf16;
        self.scalar_start = byte;
        self.pending_utf8 = [0; 4];
        self.used = 0;
        self.needed = 0;
        self.cluster = None;
        // 原held/action均已结算，最近实际scalar属于所证ASCII域。原GCB State::advance对
        // Other/incb=None/ep=false会清RI、EP/ZWJ与Indic后缀，再次ASCII是同一状态；保原cursor。
        // 后续B/CRLF仍由push_byte/pump读取并走Phase::Paragraph，不能把此端点签成闭SCX。
    }

    /// 原完整聚合或原上下文后缀已取得EOF证明，且原Store真实封存后，结束同一Machine。
    /// byte/utf16是语义完成坐标；真实读取量由外层独立累积，不能把聚合跳过伪报为I/O。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn finish_proven_eof(&mut self, byte: usize, utf16: usize) {
        self.byte = byte;
        self.utf16 = utf16;
        self.scalar_start = byte;
        self.pending_utf8 = [0; 4];
        self.used = 0;
        self.needed = 0;
        self.held = None;
        self.cluster = None;
        self.action = None;
        self.current = None;
        self.preceding = None;
        self.top = store::NONE;
        self.same_kind = [store::NONE; 65];
        self.finish_phase = 2;
        self.ready = true;
    }

    pub(super) fn complete_utf8(&self, expected: usize) -> bool {
        self.used == 0 && self.utf16 == expected
    }
    pub(super) fn push_byte(&mut self, byte: u8, step: &mut Step) -> Result<()> {
        if self.held.is_some() || self.action.is_some() || self.ready {
            return Err(Error::CorruptMetadata);
        }
        let position = self.byte;
        self.byte = self.byte.checked_add(1).ok_or(Error::CoordinateOverflow)?;
        step.bytes += 1; // 只有真正解引用的源byte计入；后续UTF8失败也不伪造零读取。
        if self.used == 0 {
            self.scalar_start = position;
            self.needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(Error::InvalidUtf8),
            };
        }
        self.pending_utf8[self.used] = byte;
        self.used += 1;
        if self.used != self.needed {
            return Ok(());
        }
        let scalar = std::str::from_utf8(&self.pending_utf8[..self.used])
            .ok()
            .and_then(|value| value.chars().next())
            .ok_or(Error::InvalidUtf8)?;
        let start_utf16 = self.utf16;
        self.utf16 = self
            .utf16
            .checked_add(scalar.len_utf16())
            .ok_or(Error::CoordinateOverflow)?;
        self.held = Some(Decoded {
            properties: properties::get(scalar),
            start_byte: self.scalar_start,
            end_byte: self.byte,
            start_utf16,
            end_utf16: self.utf16,
            boundary: self.cursor.push(scalar),
        });
        self.used = 0;
        step.scalars += 1;
        Ok(())
    }

    /// false只表示本次预算已用尽；held scalar与配对阶段保持原身份，下次不重读或重推GCB。
    pub(super) fn pump(
        &mut self,
        store: &mut Store,
        step: &mut Step,
        budget: Budget,
        eof: bool,
    ) -> Result<bool> {
        loop {
            if let Some(mut action) = self.action.take() {
                if !self.advance_action(&mut action, store, step, budget)? {
                    self.action = Some(action);
                    return Ok(false);
                }
                continue;
            }
            if let Some(value) = self.held.take() {
                let mark = value.properties.mark;
                let closes = self
                    .cluster
                    .as_ref()
                    .is_some_and(|cluster| value.boundary && (cluster.paragraph || !mark));
                if closes {
                    let cluster = self.cluster.take().ok_or(Error::CorruptMetadata)?;
                    self.action = Some(Action {
                        cluster,
                        phase: Phase::Locate,
                    });
                    self.held = Some(value);
                    continue;
                }
                let cluster = self.cluster.get_or_insert_with(|| Cluster::new(&value));
                cluster.add(value)?;
                continue;
            }
            if !eof {
                return Ok(true);
            }
            if let Some(cluster) = self.cluster.take() {
                self.action = Some(Action {
                    cluster,
                    phase: Phase::Locate,
                });
                continue;
            }
            if self.current.is_some() {
                if step.records == budget.records {
                    return Ok(false);
                }
                self.commit_run(self.byte, self.utf16, store, step)?;
                continue;
            }
            // 真EOF没有未定cluster或run后，分两次固定操作封存/归还两个实际owner。
            if step.stack_ops == budget.stack_ops {
                return Ok(false);
            }
            match self.finish_phase {
                0 => {
                    store.seal_frames()?;
                    self.finish_phase = 1;
                    step.stack_ops += 1;
                }
                1 => {
                    store.seal_runs()?;
                    self.finish_phase = 2;
                    step.stack_ops += 1;
                    self.ready = true;
                    return Ok(true);
                }
                _ => return Ok(true),
            }
        }
    }

    fn commit_run(
        &mut self,
        end_byte: usize,
        end_utf16: usize,
        store: &mut Store,
        step: &mut Step,
    ) -> Result<()> {
        let current = self.current.as_ref().ok_or(Error::CorruptMetadata)?;
        let script = current.script(self.preceding)?;
        store.append_run(Run {
            start_byte: current.start_byte,
            end_byte,
            start_utf16: current.start_utf16,
            end_utf16,
            script: Script(script),
        })?;
        self.current = None;
        self.preceding = Some(script);
        step.records += 1;
        Ok(()) // 只有append成功才丢弃待决候选和推进run ID，失败不会留下可发布的伪覆盖。
    }

    fn advance_action(
        &mut self,
        action: &mut Action,
        store: &mut Store,
        step: &mut Step,
        budget: Budget,
    ) -> Result<bool> {
        loop {
            match action.phase {
                Phase::Locate => {
                    if let Some(Bracket {
                        kind,
                        opening: false,
                    }) = action.cluster.bracket
                    {
                        let target = self.same_kind[usize::from(kind)];
                        if target != store::NONE {
                            if step.stack_ops == budget.stack_ops {
                                return Ok(false);
                            }
                            let frame = store.frame(target)?;
                            step.stack_ops += 1;
                            action.phase = Phase::Resolve {
                                target,
                                run: frame.run,
                            };
                            continue;
                        }
                    }
                    action.phase = Phase::Merge; // 未匹配closer不伪造pair，也不扫描整个历史栈。
                }
                Phase::Resolve { target, run } => {
                    if run < store.run_count() {
                        if step.stack_ops == budget.stack_ops {
                            return Ok(false);
                        }
                        let script = store.run(run)?.script.0;
                        step.stack_ops += 1;
                        action.cluster.scripts = Some(Set::singleton(script));
                        action.cluster.primary = Some(script);
                    } else if run > store.run_count() {
                        return Err(Error::CorruptMetadata);
                    }
                    // 同一个未定run内的pair不增加约束；最终run脚本自然同时赋给两端。
                    action.phase = Phase::Pop { target };
                }
                Phase::Pop { target } => {
                    if step.stack_ops == budget.stack_ops {
                        return Ok(false);
                    }
                    let index = self.top;
                    if index == store::NONE {
                        return Err(Error::CorruptMetadata);
                    }
                    let frame = store.frame(index)?;
                    step.stack_ops += 1;
                    self.same_kind[usize::from(frame.kind)] = frame.same_kind;
                    self.top = frame.previous;
                    if index == target {
                        action.phase = Phase::Merge;
                    }
                    // 每个被弹frame都消耗原预算；深/错误嵌套可以暂停，不丢弃底部32层之外的事实。
                }
                Phase::Merge => {
                    let cluster = action.cluster;
                    let intersection = match (
                        self.current.as_ref().and_then(|run| run.scripts),
                        cluster.scripts,
                    ) {
                        (Some(current), Some(next)) => Some(current.intersection(next)),
                        _ => None,
                    };
                    if intersection == Some(None) {
                        if step.records == budget.records {
                            return Ok(false);
                        }
                        self.commit_run(cluster.start_byte, cluster.start_utf16, store, step)?;
                    }
                    if let Some(run) = self.current.as_mut() {
                        if let Some(Some(set)) = intersection {
                            run.scripts = Some(set);
                            if run.primary.is_none() {
                                run.primary = cluster.primary;
                            }
                        } else if run.scripts.is_none() {
                            run.scripts = cluster.scripts;
                            run.primary = cluster.primary;
                        }
                    } else {
                        self.current = Some(CurrentRun {
                            start_byte: cluster.start_byte,
                            start_utf16: cluster.start_utf16,
                            scripts: cluster.scripts,
                            primary: cluster.primary,
                            #[cfg(feature = "paged-shaping-frontier")]
                            settled_end: (cluster.start_byte, cluster.start_utf16),
                        });
                    }
                    action.phase = Phase::Open;
                }
                Phase::Open => {
                    if let Some(Bracket {
                        kind,
                        opening: true,
                    }) = action.cluster.bracket
                    {
                        if step.records == budget.records || step.stack_ops == budget.stack_ops {
                            return Ok(false);
                        }
                        let index = store.append_frame(Frame {
                            previous: self.top,
                            same_kind: self.same_kind[usize::from(kind)],
                            run: store.run_count(),
                            kind,
                        })?;
                        step.records += 1;
                        step.stack_ops += 1;
                        self.top = index;
                        self.same_kind[usize::from(kind)] = index;
                    }
                    action.phase = Phase::Paragraph;
                }
                Phase::Paragraph => {
                    if action.cluster.paragraph {
                        if step.records == budget.records {
                            return Ok(false);
                        }
                        self.commit_run(
                            action.cluster.end_byte,
                            action.cluster.end_utf16,
                            store,
                            step,
                        )?;
                        self.top = store::NONE;
                        self.same_kind = [store::NONE; 65];
                        self.preceding = None;
                    } // 真B终止上下文；只重置固定65个头，不读取/清空全部旧frame或任何正文。
                    action.phase = Phase::Done;
                }
                Phase::Done => {
                    #[cfg(feature = "paged-shaping-frontier")]
                    if let Some(run) = &mut self.current {
                        // 先保留原完整run选择/写盘顺序，只有本cluster全部阶段成功才推进
                        // 开放可借范围。后继scalar仍留在原cluster/held状态，不进入此范围。
                        run.settled_end = (action.cluster.end_byte, action.cluster.end_utf16);
                    }
                    return Ok(true);
                }
            }
        }
    }
}
