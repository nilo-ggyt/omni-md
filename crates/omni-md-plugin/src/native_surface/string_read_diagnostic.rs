//! 显式诊断构建中的NSString批量读取统计；只保存原调用已有的固定标量，不读取正文。

use std::sync::Mutex;

const MAX_REPORTS: u16 = 128;

#[derive(Clone, Copy, Default)]
struct ReadCounters {
    requests: usize,
    requested_units: usize,
    max_request_units: usize,
    max_document_units: usize,
    full_document_requests: usize,
    nonempty_full_document_requests: usize,
    invalid_ranges: usize,
    character_requests: usize,
    character_invalid_ranges: usize,
    saturated: bool,
}

/// 只描述已经观察到的违规；没有违规不代表覆盖了所有字符串访问或证明完整零拷贝。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadViolation {
    SaturatedCounters,
    InvalidRange,
    NonemptyFullDocument,
}

impl ReadViolation {
    fn code(self) -> &'static str {
        match self {
            Self::SaturatedCounters => "saturated_counters", // 已丢失精确计数，不能继续签发有限读取证据。
            Self::InvalidRange => "invalid_range", // 越界及范围加法溢出都必须失败，不能用替换字符掩盖。
            Self::NonemptyFullDocument => "nonempty_full_document", // 一次实际全文 primitive 已违反此门禁。
        }
    }
}

impl ReadCounters {
    fn violation(self) -> Option<ReadViolation> {
        if self.saturated {
            return Some(ReadViolation::SaturatedCounters);
        }
        if self.invalid_ranges != 0 || self.character_invalid_ranges != 0 {
            return Some(ReadViolation::InvalidRange);
        }
        if self.nonempty_full_document_requests != 0 {
            return Some(ReadViolation::NonemptyFullDocument);
        }
        None // 仅表示此批量 primitive 计数未观察到违规，不提供完整零拷贝结论。
    }

    #[cfg(test)]
    fn observe(&mut self, document_units: usize, location: usize, length: usize) {
        let valid = location
            .checked_add(length)
            .is_some_and(|end| end <= document_units);
        self.observe_primitive(
            document_units,
            Some(location),
            length,
            valid,
            ReadPrimitive::Bulk,
        );
    }

    fn observe_primitive(
        &mut self,
        document_units: usize,
        location: Option<usize>,
        length: usize,
        request_valid: bool,
        primitive: ReadPrimitive,
    ) {
        self.max_document_units = self.max_document_units.max(document_units);
        let valid = request_valid
            && location
                .and_then(|start| start.checked_add(length))
                .is_some_and(|end| end <= document_units);
        if primitive == ReadPrimitive::Character {
            // 单字符访问独立计数；长度为一的文档也不能把正常characterAtIndex误报成bulk全文复制。
            self.character_requests = add(self.character_requests, 1, &mut self.saturated);
            if !valid || length != 1 {
                self.character_invalid_ranges =
                    add(self.character_invalid_ranges, 1, &mut self.saturated);
            }
            return;
        }
        self.requests = add(self.requests, 1, &mut self.saturated);
        self.requested_units = add(self.requested_units, length, &mut self.saturated);
        self.max_request_units = self.max_request_units.max(length);
        if location == Some(0) && length == document_units {
            // 精确0..canonical_len才是完整请求；空文档的0..0也按其真实范围计数。
            self.full_document_requests = add(self.full_document_requests, 1, &mut self.saturated);
            if document_units != 0 {
                // 关闭后的合法空壳读取不能与复制仍存活的整篇正文混为一项证据。
                self.nonempty_full_document_requests =
                    add(self.nonempty_full_document_requests, 1, &mut self.saturated);
            }
        }
        if !valid {
            // 只观察越界或加法溢出；既不纠正范围，也不改变原Foundation读取/替换字符逻辑。
            self.invalid_ranges = add(self.invalid_ranges, 1, &mut self.saturated);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 来源值可跨线程，诊断只区分真实UTF-16 primitive，不保存receiver或正文。
pub(super) enum ReadPrimitive {
    Character,
    Bulk,
}

/// 计数溢出只置诊断标志并饱和；不让诊断panic影响合法Foundation调用。
fn add(value: usize, amount: usize, saturated: &mut bool) -> usize {
    match value.checked_add(amount) {
        Some(total) => total,
        None => {
            *saturated = true;
            usize::MAX
        }
    }
}

#[derive(Clone, Copy)]
struct ReadDiagnostic {
    stage: Option<i16>,
    stage_reported: bool,
    reports: u16,
    current: ReadCounters,
    process: ReadCounters,
}

impl ReadDiagnostic {
    const fn new() -> Self {
        const EMPTY: ReadCounters = ReadCounters {
            requests: 0,
            requested_units: 0,
            max_request_units: 0,
            max_document_units: 0,
            full_document_requests: 0,
            nonempty_full_document_requests: 0,
            invalid_ranges: 0,
            character_requests: 0,
            character_invalid_ranges: 0,
            saturated: false,
        };
        Self {
            stage: None,
            stage_reported: false,
            reports: 0,
            current: EMPTY,
            process: EMPTY,
        }
    }

    fn begin_stage(&mut self, stage: i16) {
        self.stage = Some(stage);
        self.stage_reported = false;
        self.current = ReadCounters::default(); // 只清阶段计数；此前真实全文读取和越界永远留在 process。
    }

    #[cfg(test)]
    fn observe(&mut self, document_units: usize, location: usize, length: usize) {
        let valid = location
            .checked_add(length)
            .is_some_and(|end| end <= document_units);
        self.observe_primitive(
            document_units,
            Some(location),
            length,
            valid,
            ReadPrimitive::Bulk,
        );
    }

    fn observe_primitive(
        &mut self,
        document_units: usize,
        global_location: Option<usize>,
        length: usize,
        request_valid: bool,
        primitive: ReadPrimitive,
    ) {
        if self.stage.is_some() {
            // 只有声明后的读取才有阶段归属；report 后迟到的 Native 读取仍归前一阶段。
            self.current.observe_primitive(
                document_units,
                global_location,
                length,
                request_valid,
                primitive,
            );
        }
        // 进程累计覆盖所有真实 primitive 调用，也包括首个 begin 之前与阶段之间的读取。
        self.process.observe_primitive(
            document_units,
            global_location,
            length,
            request_valid,
            primitive,
        );
    }
}

/// 全进程只保留一组固定标量；后台Foundation读取不能落入另一个无法核验的TLS。
struct ReadDiagnosticStore {
    state: Mutex<ReadDiagnostic>,
}

impl ReadDiagnosticStore {
    const fn new() -> Self {
        Self {
            state: Mutex::new(ReadDiagnostic::new()),
        }
    }

    fn with_state<T>(&self, operation: impl FnOnce(&mut ReadDiagnostic) -> T) -> T {
        let mut state = match self.state.lock() {
            Ok(state) => state, // 同步边界只修改标量；不在持锁期间调用Native、输出或采集调用栈。
            Err(poisoned) => {
                // 已中断的观测无法保证精确计数。保留原累计并标记失败，不能换空槽继续认证。
                let mut state = poisoned.into_inner();
                state.current.saturated = true;
                state.process.saturated = true;
                state
            }
        };
        operation(&mut state)
    }
}

// 显式诊断构建才包含此槽；不保存正文、原生对象、路径或源owner，也没有每线程副本。
static READ_DIAGNOSTIC: ReadDiagnosticStore = ReadDiagnosticStore::new();

/// 不可变值直接交付其原snapshot事实；后台调用也进入同一个累计槽，不借较新活动正文。
pub(super) fn observe_frozen_read(request: super::frozen_native_source::FrozenReadRequest) {
    use super::frozen_native_source::FrozenReadKind;
    let primitive = match request.kind {
        FrozenReadKind::Character => ReadPrimitive::Character,
        FrozenReadKind::Bulk => ReadPrimitive::Bulk,
    };
    observe_primitive(
        request.snapshot_utf16_len,
        request.checked_global_start,
        request.requested_length,
        request.receiver_valid && request.request_valid,
        primitive,
    ); // 不把子串越界、全局加法溢出或旧源全文请求裁成另一个范围。
}

/// 每个driver声明begin只重置阶段标量；进程累计保留此前事件间隙中实际发生的读取。
pub(crate) fn begin(stage: i16) {
    READ_DIAGNOSTIC.with_state(|state| state.begin_stage(stage));
}

/// 在原已借用canonical处记录已有length与NSRange；不新增文档借用或任何Native getter。
pub(crate) fn observe(document_units: usize, location: usize, length: usize) {
    let valid = location
        .checked_add(length)
        .is_some_and(|end| end <= document_units);
    observe_primitive(
        document_units,
        Some(location),
        length,
        valid,
        ReadPrimitive::Bulk,
    );
}

/// 冻结子串的全局位置与接收者合法性须同时保留；不能把越过子串的请求当作合法源读取。
pub(super) fn observe_primitive(
    document_units: usize,
    global_location: Option<usize>,
    length: usize,
    request_valid: bool,
    primitive: ReadPrimitive,
) {
    READ_DIAGNOSTIC.with_state(|state| {
        state.observe_primitive(
            document_units,
            global_location,
            length,
            request_valid,
            primitive,
        );
    });
    if primitive == ReadPrimitive::Bulk
        && document_units != 0
        && global_location == Some(0)
        && length == document_units
    {
        // 原计数槽已结束借用；诊断只看已知范围，不查询 Native 或读取第二遍正文。
        super::probe_call_diagnostic::first_full_document_read(document_units);
    }
}

/// 驱动完成真实返回、严格关闭及既有采样窗口后只核一次进程累计；不提前撤图或更改实际读取。
/// 无违规也只记录有限 primitive 证据，不能据此签发完整零拷贝或视口渲染通过。
pub(crate) fn verify_process_read_gate() -> Result<(), String> {
    let pre_owner_passthrough = super::frozen_annotation_capture::pre_owner_passthrough_count();
    let counters = READ_DIAGNOSTIC.with_state(|state| {
        if pre_owner_passthrough == usize::MAX {
            // 构造期回退计量已丢精确性；空的后继阶段不能恢复可验证证据。
            state.process.saturated = true;
        }
        state.process
    });
    let violation = counters.violation();
    let status = violation.map_or("no_observed_bulk_read_violation", ReadViolation::code);
    // 全进程锁已归还；日志只输出原累计标量，不读正文、Native对象或第二份磁盘数据。
    println!(
        "native_string_read_gate scope=process_bulk_and_character_primitives status={status} nonempty_full_document_requests={} invalid_ranges={} character_requests={} character_invalid_ranges={} saturated={} pre_owner_annotation_passthrough={pre_owner_passthrough} zero_copy_certified=false",
        counters.nonempty_full_document_requests,
        counters.invalid_ranges,
        counters.character_requests,
        counters.character_invalid_ranges,
        counters.saturated
    );
    match violation {
        Some(violation) => Err(format!(
            "原生字符串累计读取门禁失败（{}）：非空全文请求={}，批量非法范围={}，单字符非法范围={}，计数饱和={}",
            violation.code(),
            counters.nonempty_full_document_requests,
            counters.invalid_ranges,
            counters.character_invalid_ranges,
            counters.saturated
        )),
        None => Ok(()), // 不把零请求或未发现违规解释成完整零拷贝、布局覆盖或整产品内存通过。
    }
}

/// pool_drained只读取一次已有统计；之后的回调仍归前阶段，并保留在下一行的进程累计中。
/// 此边界不声称精确present、稳定峰值或系统没有另外复制正文。
pub(crate) fn report(stage: i16) {
    let observed = READ_DIAGNOSTIC.with_state(|state| {
        if state.stage != Some(stage) || state.stage_reported || state.reports == MAX_REPORTS {
            return None; // 错阶段、重复或全进程输出耗尽均不产生额外日志，也不改变产品行为。
        }
        state.stage_reported = true;
        state.reports += 1;
        Some((state.current, state.process, state.reports))
    });
    if let Some((current, process, reports)) = observed {
        // 先归还全进程锁再输出；只有固定计数，调用地址、用户内容和路径均不会进入日志。
        println!(
            "native_string_read stage={stage} boundary=pool_drained requests={} requested_units={} max_request_units={} max_document_units={} full_document_requests={} nonempty_full_document_requests={} invalid_ranges={} character_requests={} character_invalid_ranges={} saturated={} process_requests={} process_requested_units={} process_max_request_units={} process_max_document_units={} process_full_document_requests={} process_nonempty_full_document_requests={} process_invalid_ranges={} process_character_requests={} process_character_invalid_ranges={} process_saturated={} reports={reports} output_cap_reached={} present=false",
            current.requests,
            current.requested_units,
            current.max_request_units,
            current.max_document_units,
            current.full_document_requests,
            current.nonempty_full_document_requests,
            current.invalid_ranges,
            current.character_requests,
            current.character_invalid_ranges,
            current.saturated,
            process.requests,
            process.requested_units,
            process.max_request_units,
            process.max_document_units,
            process.full_document_requests,
            process.nonempty_full_document_requests,
            process.invalid_ranges,
            process.character_requests,
            process.character_invalid_ranges,
            process.saturated,
            reports == MAX_REPORTS,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{ReadCounters, ReadDiagnostic, ReadDiagnosticStore, ReadPrimitive, ReadViolation};

    #[test]
    fn substring_receiver_bounds_cannot_be_hidden_by_a_valid_global_source_range() {
        let mut state = ReadDiagnostic::new();
        // 两次请求的全局范围都在100单元源内；越过真实接收者仍分别记入非法bulk/character。
        state.observe_primitive(100, Some(30), 21, false, ReadPrimitive::Bulk);
        state.observe_primitive(100, Some(51), 1, false, ReadPrimitive::Character);
        state.begin_stage(424); // 后继关闭阶段也不能丢失冻结子串的非法原参数。
        assert_eq!(state.process.requests, 1);
        assert_eq!(state.process.invalid_ranges, 1);
        assert_eq!(state.process.character_requests, 1);
        assert_eq!(state.process.character_invalid_ranges, 1);
        assert_eq!(state.process.violation(), Some(ReadViolation::InvalidRange));
    }

    #[test]
    fn character_reads_and_frozen_snapshot_lengths_keep_their_actual_meaning() {
        let mut state = ReadDiagnostic::new();
        state.observe_primitive(1, Some(0), 1, true, ReadPrimitive::Character);
        assert_eq!(state.process.character_requests, 1);
        assert_eq!(state.process.nonempty_full_document_requests, 0);
        state.observe_primitive(128, Some(0), 128, true, ReadPrimitive::Bulk);
        // 活动正文随后变短不能让旧冻结快照的真实全文复制变成一次‘越界’或局部请求。
        state.observe_primitive(8, Some(1), 1, true, ReadPrimitive::Character);
        assert_eq!(state.process.nonempty_full_document_requests, 1);
        assert_eq!(state.process.invalid_ranges, 0);
        assert_eq!(state.process.max_document_units, 128);
        assert_eq!(
            state.process.violation(),
            Some(ReadViolation::NonemptyFullDocument)
        );
        state.observe_primitive(128, None, 1, false, ReadPrimitive::Character);
        assert_eq!(state.process.character_invalid_ranges, 1);
    }

    #[test]
    fn background_reads_share_one_process_gate_across_stage_resets() {
        let store = ReadDiagnosticStore::new();
        store.with_state(|state| state.begin_stage(400));
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let store = &store;
                scope.spawn(move || {
                    // 后台固定有限请求与主线程共用同一实现；不通过全局测试状态互相污染。
                    for read in 0..100 {
                        store.with_state(|state| state.observe(10_000, 1 + worker + read, 64));
                    }
                });
            }
            // 主线程开始后继阶段仍不能丢失此前或并发的后台读取。
            store.with_state(|state| state.begin_stage(401));
        });
        store.with_state(|state| {
            state.observe(10_000, 0, 10_000); // 后台来源换成冻结值也必须计入同一全文读取门禁。
            state.observe(10_000, usize::MAX, 1); // 溢出的原始请求仍保留为非法证据。
            state.begin_stage(424);
            assert_eq!(state.current.requests, 0);
            assert_eq!(state.process.requests, 402);
            assert_eq!(state.process.nonempty_full_document_requests, 1);
            assert_eq!(state.process.invalid_ranges, 1);
            assert_eq!(state.process.violation(), Some(ReadViolation::InvalidRange));
        });
    }

    #[test]
    fn poisoned_observation_keeps_original_evidence_and_fails_the_gate() {
        let store = ReadDiagnosticStore::new();
        store.with_state(|state| state.observe(100, 1, 8));
        let interrupted = std::panic::catch_unwind(|| {
            store.with_state(|_| panic!("合成标量观测中断"));
        });
        assert!(interrupted.is_err());
        store.with_state(|state| {
            // 不清空中断前的累计；后继阶段即使没有新读取也不能签发通过。
            state.begin_stage(424);
            assert_eq!(state.process.requests, 1);
            assert_eq!(
                state.process.violation(),
                Some(ReadViolation::SaturatedCounters)
            );
        });
    }

    #[test]
    fn empty_shell_reads_do_not_certify_a_live_document_copy() {
        let mut counters = ReadCounters::default();
        counters.observe(0, 0, 0); // 空壳的精确空请求仍是完整范围，但没有正文可复制。
        counters.observe(49_208, 100, 64);
        assert_eq!(counters.full_document_requests, 1);
        assert_eq!(counters.nonempty_full_document_requests, 0);
        assert_eq!(counters.invalid_ranges, 0);
        assert_eq!(counters.violation(), None);
        counters.observe(49_208, 0, 49_208); // 活正文全范围读取必须独立暴露。
        assert_eq!(counters.full_document_requests, 2);
        assert_eq!(counters.nonempty_full_document_requests, 1);
        assert_eq!(
            counters.violation(),
            Some(ReadViolation::NonemptyFullDocument)
        );
    }

    #[test]
    fn whole_reads_before_first_stage_and_between_stages_remain_terminal_failures() {
        let mut state = ReadDiagnostic::new();
        state.observe(165_042, 0, 165_042); // 原生构造在首次 stage begin 前发生也计入最终门禁。
        state.begin_stage(400);
        state.observe(165_042, 100, 64);
        state.begin_stage(401);
        assert_eq!(state.current.requests, 0);
        assert_eq!(state.process.nonempty_full_document_requests, 1);
        assert_eq!(
            state.process.violation(),
            Some(ReadViolation::NonemptyFullDocument)
        );
        state.observe(165_042, 0, 165_042);
        state.begin_stage(424); // strict 关闭与下一阶段都不能冲销此前非空全文读取。
        state.observe(0, 0, 0);
        assert_eq!(state.process.nonempty_full_document_requests, 2);
        assert_eq!(
            state.process.violation(),
            Some(ReadViolation::NonemptyFullDocument)
        );
    }

    #[test]
    fn invalid_ranges_and_saturated_counters_fail_without_changing_observation() {
        for (location, length) in [(10, 1), (usize::MAX, 1)] {
            let mut counters = ReadCounters::default();
            counters.observe(10, location, length);
            assert_eq!(counters.invalid_ranges, 1);
            assert_eq!(counters.violation(), Some(ReadViolation::InvalidRange));
        }
        let mut state = ReadDiagnostic::new();
        state.process.requests = usize::MAX;
        state.observe(10, 0, 1);
        assert_eq!(state.process.requests, usize::MAX);
        assert!(state.process.saturated);
        state.begin_stage(424);
        assert_eq!(
            state.process.violation(),
            Some(ReadViolation::SaturatedCounters)
        );
    }

    #[test]
    fn empty_or_partial_evidence_does_not_invent_a_full_copy_violation() {
        let mut counters = ReadCounters::default();
        assert_eq!(counters.violation(), None); // 没有读取同样不等于已经覆盖完整 Native 行为。
        counters.observe(300_000, 0, 4_096);
        counters.observe(300_000, 4_096, 4_096);
        assert_eq!(counters.max_request_units, 4_096);
        assert_eq!(counters.violation(), None); // 仅放行此必要门禁，完整零拷贝仍未被认证。
    }
}
