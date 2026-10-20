//! 显式原生探针的有界调用诊断；只采本进程标量与首次全文读取调用栈，不改变原文访问。

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

#[cfg(feature = "native-raster-surface-probe")]
mod layer_tree;
#[cfg(feature = "native-raster-surface-probe")]
pub(crate) use layer_tree::layer_audit_point;

const MAX_MEMORY_POINTS: u16 = 512;
#[cfg(feature = "native-block-fragment-diagnostic")]
const MAX_FRAMES: usize = 64;
#[cfg(any(feature = "native-block-fragment-diagnostic", test))]
const MAX_SYMBOL_BYTES: usize = 384;

// 上限覆盖整个进程，不因新的窗口、线程或阶段重置；原业务状态完全不使用这些标量。
static MEMORY_POINTS: AtomicU16 = AtomicU16::new(0);
// 分配审计独立于高频绘制采样，避免正文回调先耗尽真正功能阶段的观察预算。
static ALLOCATION_ONCE: AtomicU16 = AtomicU16::new(0);
static ALLOCATION_AUDIT_POINTS: AtomicU16 = AtomicU16::new(0);
#[cfg(feature = "native-block-fragment-diagnostic")]
static FULL_READ_TRACED: AtomicBool = AtomicBool::new(false);
static DIAGNOSTIC_BUSY: AtomicBool = AtomicBool::new(false);

/// 固定环境名只检查存在性，不复制值，也不访问可能包含个人信息的其他环境字段。
fn enabled() -> bool {
    // SAFETY: 固定 C 字符串以 NUL 结束；只检查 libc 借用指针是否非空，不读取或持有环境值。
    // 产品启动后不并发修改环境；此探针也不调用 setenv/unsetenv 或申请任何系统权限。
    !unsafe { libc::getenv(c"OMNI_NATIVE_PROBE_TRACE".as_ptr()) }.is_null()
}

/// 重入或并发诊断不能递归展开回溯；原调用照常继续，不将缺少诊断变为业务拒绝。
struct DiagnosticScope;

impl DiagnosticScope {
    fn enter() -> Option<Self> {
        DIAGNOSTIC_BUSY
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for DiagnosticScope {
    fn drop(&mut self) {
        // 只撤销诊断自身标量，不调用 Native、产品清理或文本回调。
        DIAGNOSTIC_BUSY.store(false, Ordering::Relaxed);
    }
}

// libc 0.2.189 已绑定 task_info 与 malloc 统计，但未声明 TASK_VM_INFO 的公开结构。
// 此处只声明本机 mach/task_info.h 的 REV3 前缀；macOS 14 的公开 XNU 头也含相同字段。
// 只请求这84个 natural_t，不读取后续版本字段，不把较短返回或查询失败解释成零内存。
#[repr(C, packed(4))]
struct DiagnosticTaskVmInfoRev3 {
    _virtual_size: u64,
    _region_count: i32,
    _page_size: i32,
    resident_size: u64,
    _resident_size_peak: u64,
    device: u64,
    _device_peak: u64,
    internal: u64,
    _internal_peak: u64,
    external: u64,
    _external_peak: u64,
    reusable: u64,
    _reusable_peak: u64,
    _purgeable_volatile_pmap: u64,
    _purgeable_volatile_resident: u64,
    _purgeable_volatile_virtual: u64,
    compressed: u64,
    _compressed_peak: u64,
    _compressed_lifetime: u64,
    phys_footprint: u64,
    _min_address: u64,
    _max_address: u64,
    _ledger_phys_footprint_peak: i64,
    _ledger_purgeable_nonvolatile: i64,
    _ledger_purgeable_nonvolatile_compressed: i64,
    _ledger_purgeable_volatile: i64,
    _ledger_purgeable_volatile_compressed: i64,
    _ledger_tag_network_nonvolatile: i64,
    _ledger_tag_network_nonvolatile_compressed: i64,
    _ledger_tag_network_volatile: i64,
    _ledger_tag_network_volatile_compressed: i64,
    ledger_tag_media_footprint: i64,
    ledger_tag_media_footprint_compressed: i64,
    ledger_tag_media_nofootprint: i64,
    ledger_tag_media_nofootprint_compressed: i64,
    ledger_tag_graphics_footprint: i64,
    ledger_tag_graphics_footprint_compressed: i64,
    ledger_tag_graphics_nofootprint: i64,
    ledger_tag_graphics_nofootprint_compressed: i64,
    _ledger_tag_neural_footprint: i64,
    _ledger_tag_neural_footprint_compressed: i64,
    _ledger_tag_neural_nofootprint: i64,
    _ledger_tag_neural_nofootprint_compressed: i64,
}

const TASK_VM_INFO_FLAVOR: libc::task_flavor_t = 22;
const TASK_VM_INFO_REV3_COUNT: libc::mach_msg_type_number_t = 84;
const _: () = {
    // 与公开C头的 pack(4)、sizeof 及实际被读取字段的 offsetof 逐项相同。
    // 编译期拒绝Rust目标布局变化，避免一次诊断把后继整数误当成另一类字节数。
    assert!(std::mem::size_of::<libc::integer_t>() == 4);
    assert!(std::mem::size_of::<DiagnosticTaskVmInfoRev3>() == 336);
    assert!(std::mem::align_of::<DiagnosticTaskVmInfoRev3>() == 4);
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, resident_size) == 16);
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, device) == 32);
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, internal) == 48);
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, external) == 64);
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, reusable) == 80);
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, compressed) == 120);
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, phys_footprint) == 144);
    assert!(std::mem::size_of::<libc::malloc_statistics_t>() == 32);
    assert!(std::mem::offset_of!(libc::malloc_statistics_t, size_in_use) == 8);
    assert!(std::mem::offset_of!(libc::malloc_statistics_t, max_size_in_use) == 16);
    assert!(std::mem::offset_of!(libc::malloc_statistics_t, size_allocated) == 24);
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, ledger_tag_media_footprint) == 240);
    assert!(
        std::mem::offset_of!(
            DiagnosticTaskVmInfoRev3,
            ledger_tag_media_footprint_compressed
        ) == 248
    );
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, ledger_tag_media_nofootprint) == 256);
    assert!(
        std::mem::offset_of!(
            DiagnosticTaskVmInfoRev3,
            ledger_tag_media_nofootprint_compressed
        ) == 264
    );
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, ledger_tag_graphics_footprint) == 272);
    assert!(
        std::mem::offset_of!(
            DiagnosticTaskVmInfoRev3,
            ledger_tag_graphics_footprint_compressed
        ) == 280
    );
    assert!(std::mem::offset_of!(DiagnosticTaskVmInfoRev3, ledger_tag_graphics_nofootprint) == 288);
    assert!(
        std::mem::offset_of!(
            DiagnosticTaskVmInfoRev3,
            ledger_tag_graphics_nofootprint_compressed
        ) == 296
    );
};

unsafe extern "C" {
    // mach/mach_init.h 的 mach_task_self() 就是这个公开进程本地端口变量。
    // libc同名绑定标为弃用以推荐另一个crate；无需为三点诊断新增整个Mach依赖。
    #[link_name = "mach_task_self_"]
    static DIAGNOSTIC_CURRENT_TASK: libc::mach_port_t;
}

struct MemoryClasses {
    vm: DiagnosticTaskVmInfoRev3,
    vm_status: libc::kern_return_t,
    vm_count: libc::mach_msg_type_number_t,
    malloc: libc::malloc_statistics_t,
}

/// 每个已准入调用点只查询一次；不遍历地址空间、不找远程task、不收集指针或正文。
fn capture_memory_classes() -> MemoryClasses {
    // SAFETY: 两个公开ABI结构均只含整数；零初始化只作完整输出存储，失败时不发布其零值。
    let mut vm: DiagnosticTaskVmInfoRev3 = unsafe { std::mem::zeroed() };
    // SAFETY: libc的实际malloc_statistics_t与公开头一致；函数同步写入全部四个统计字段。
    let mut malloc: libc::malloc_statistics_t = unsafe { std::mem::zeroed() };
    let mut vm_count = TASK_VM_INFO_REV3_COUNT;
    // SAFETY: 仅取本进程初始化后的公开task端口；输出指向可写336字节，count恰为84个32位整数。
    // task_info不拥有输出或端口，也不申请task_for_pid权限；返回count仍须认证后才读分类值。
    let vm_status = unsafe {
        libc::task_info(
            DIAGNOSTIC_CURRENT_TASK,
            TASK_VM_INFO_FLAVOR,
            (&mut vm as *mut DiagnosticTaskVmInfoRev3).cast::<libc::integer_t>(),
            &mut vm_count,
        )
    };
    // SAFETY: 公开合同允许null表示汇总全部zone，输出是完整可写的libc绑定结构。
    // 此调用位于产品借用之外、分配器调用栈之外；不触发pressure_relief或改变分配/回收策略。
    unsafe { libc::malloc_zone_statistics(std::ptr::null_mut(), &mut malloc) };
    MemoryClasses {
        vm,
        vm_status,
        vm_count,
        malloc,
    }
}

/// VM与malloc是先后取得的快照，不能逐项相加或据此冒充区间峰值/原子分类。
fn emit_memory_classes(
    phase: &'static str,
    sequence: u16,
    classes: MemoryClasses,
    output: &mut impl Write,
) {
    let MemoryClasses {
        vm,
        vm_status,
        vm_count,
        malloc,
    } = classes;
    if vm_status == libc::KERN_SUCCESS && vm_count == TASK_VM_INFO_REV3_COUNT {
        // packed(4)字段只按值复制到自然对齐局部量；format不取得非对齐Rust引用。
        let resident = vm.resident_size;
        let footprint = vm.phys_footprint;
        let device = vm.device;
        let internal = vm.internal;
        let external = vm.external;
        let reusable = vm.reusable;
        let compressed = vm.compressed;
        let media = vm.ledger_tag_media_footprint;
        let media_compressed = vm.ledger_tag_media_footprint_compressed;
        let media_nofootprint = vm.ledger_tag_media_nofootprint;
        let media_nofootprint_compressed = vm.ledger_tag_media_nofootprint_compressed;
        let graphics = vm.ledger_tag_graphics_footprint;
        let graphics_compressed = vm.ledger_tag_graphics_footprint_compressed;
        let graphics_nofootprint = vm.ledger_tag_graphics_nofootprint;
        let graphics_nofootprint_compressed = vm.ledger_tag_graphics_nofootprint_compressed;
        let _ = writeln!(
            output,
            "native_memory_classes phase={phase} seq={sequence} vm_available=true vm_count={vm_count} resident_bytes={resident} physical_footprint_bytes={footprint} device_bytes={device} internal_bytes={internal} external_bytes={external} reusable_bytes={reusable} compressed_bytes={compressed} media_footprint_bytes={media} media_compressed_bytes={media_compressed} media_nofootprint_bytes={media_nofootprint} media_nofootprint_compressed_bytes={media_nofootprint_compressed} graphics_footprint_bytes={graphics} graphics_compressed_bytes={graphics_compressed} graphics_nofootprint_bytes={graphics_nofootprint} graphics_nofootprint_compressed_bytes={graphics_nofootprint_compressed} scope=sequential_single_pid_snapshots whole_product_budget_pass=false"
        );
    } else {
        // 旧系统/查询失败/较短输出都保留真实状态；未填充字段不能以0字节假装有效分类。
        let _ = writeln!(
            output,
            "native_memory_classes phase={phase} seq={sequence} vm_available=false vm_status={vm_status} vm_count={vm_count} requested_count={TASK_VM_INFO_REV3_COUNT} scope=sequential_single_pid_snapshots whole_product_budget_pass=false"
        );
    }
    // malloc统计没有错误返回；null-zone公开合同汇总真实in-use及保留容量。
    // max_size_in_use在SDK标为touched-memory高水位，不重命名成这个阶段的活值峰值或RSS。
    let _ = writeln!(
        output,
        "native_malloc_classes phase={phase} seq={sequence} available=true blocks_in_use={} size_in_use_bytes={} max_size_in_use_bytes={} size_allocated_bytes={} scope=sequential_all_zone_snapshot whole_product_budget_pass=false",
        malloc.blocks_in_use, malloc.size_in_use, malloc.max_size_in_use, malloc.size_allocated
    );
}

/// 查询本进程已声明的完整 v4 ABI；结果只是此调用点快照与进程 lifetime，不是阶段峰值。
fn emit_memory(phase: &'static str, sequence: u16, output: &mut impl Write) {
    // SAFETY: SDK/libc 的 v4 结构仅含整数和字节数组，零初始化合法且满足自然对齐/完整长度。
    let mut usage: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: getpid 无前置条件；buffer 依照本机 libproc.h 的 rusage_info_t* 签名转换，
    // 实际指向完整可写 v4 存储，不把其中首字段当作可解引用的指针，也不手写另一套结构。
    let (pid, status) = unsafe {
        let pid = libc::getpid();
        let status = libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            (&mut usage as *mut libc::rusage_info_v4).cast::<libc::rusage_info_t>(),
        );
        (pid, status)
    };
    // 原绘制边界及固定视图观察前后读取同一分类；仍受原trace/audit门与次数预算约束。
    // 先取得全部标量再输出，避免把本条日志格式化产生的分配计入它自身的分类快照。
    let classes = matches!(
        phase,
        "table_110_request_return"
            | "body_draw_end"
            | "table_executor_pending_begin"
            | "view_geometry_begin"
            | "view_geometry_end"
    )
    .then(capture_memory_classes);
    if status == 0 {
        // lifetime 已包含本进程较早工作，不能把它重命名为 Find/关闭阶段的区间 MAX。
        let _ = writeln!(
            output,
            "native_probe_call phase={phase} seq={sequence} valid=true pid={pid} physical_footprint_bytes={} lifetime_peak_bytes={} scope=single_pid_snapshot whole_product_budget_pass=false",
            usage.ri_phys_footprint, usage.ri_lifetime_max_phys_footprint
        );
    } else {
        // 查询失败不输出零内存，不读取未成功填充的计量字段，也不进行重试或权限提升。
        let _ = writeln!(
            output,
            "native_probe_call phase={phase} seq={sequence} valid=false pid={pid} rusage_status={status} scope=single_pid_snapshot whole_product_budget_pass=false"
        );
    }
    if let Some(classes) = classes {
        emit_memory_classes(phase, sequence, classes, output);
    }
}

/// 真实 Find 与绘制边界只接受固定标签；未声明的字符串不能成为任意日志载荷。
pub(crate) fn memory_point(phase: &'static str) {
    if !matches!(
        phase,
        "selection_begin"
            | "selection_return"
            | "coarse_scroll_return"
            | "body_draw_begin"
            | "body_draw_end"
            | "body_super_draw_end"
            | "background_super_begin"
            | "background_super_end"
            | "will_draw_super_begin"
            | "will_draw_super_end"
            | "layout_super_begin"
            | "layout_super_end"
            | "gutter_draw_begin"
            | "gutter_draw_end"
            | "workload_stage_begin"
            | "workload_stage_return"
            | "workload_stage_drained"
            | "workload_terminal"
            | "view_geometry_begin"
            | "view_geometry_end"
    ) || !enabled()
    {
        return;
    }
    let Some(_scope) = DiagnosticScope::enter() else {
        return;
    };
    let Ok(previous) = MEMORY_POINTS.try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
        // 上限后永久静默；不回绕计数、不分配序列缓存，也不按阶段重置预算。
        (count < MAX_MEMORY_POINTS).then(|| count + 1)
    }) else {
        return;
    };
    emit_memory(phase, previous + 1, &mut std::io::stderr().lock());
}

/// 只为合成验收的固定功能边界读取内核标量，不暂停目标线程或遍历其地址空间。
pub(crate) fn allocation_audit_point(phase: &'static str) {
    if !matches!(
        phase,
        "exercise_begin"
            | "exercise_subscript_done"
            | "exercise_list_done"
            | "exercise_selection_done"
            | "exercise_multi_done"
            | "exercise_end"
            | "table_stage_begin"
            | "table_stage_return"
            | "table_before_oversized"
            | "table_oversized_created"
            | "table_oversized_set_return"
            | "table_commit_rejected"
            | "table_mode_rejected"
            | "table_draft_restored"
            | "table_110_layout_return"
            | "table_110_scroll_return"
            | "table_110_geometry_return"
            | "table_110_receipt_return"
            | "table_110_request_return"
            | "field_source_begin"
            | "field_source_return"
            | "field_render_begin"
            | "field_render_return"
            | "table_executor_pending_begin"
            | "table_executor_required_ready"
            | "table_executor_cache_return"
            | "table_executor_gutter_return"
            | "table_executor_decorations_return"
            | "table_executor_blocks_return"
            | "field_create_begin"
            | "field_create_return"
            | "field_value_set_return"
            | "field_layout_return"
            | "field_focus_begin"
            | "field_focus_return"
    ) {
        return; // 未声明标签不成为任意诊断或用户数据载荷。
    }
    // SAFETY: 仅查询固定NUL结束键的存在性；不复制环境、路径或产品数据。
    if unsafe { libc::getenv(c"OMNI_NATIVE_PROBE_ALLOCATION_AUDIT".as_ptr()) }.is_null() {
        return;
    }
    let Some(_scope) = DiagnosticScope::enter() else {
        return;
    };
    let Ok(previous) =
        ALLOCATION_AUDIT_POINTS.try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            (count < 64).then(|| count + 1)
        })
    else {
        return;
    }; // 整个进程固定64次，不按窗口/阶段重置或申请存储。
    emit_memory(phase, previous + 1, &mut std::io::stderr().lock());
}

/// 每个固定异步阶段只记录第一次实际到达；循环推进不会消耗其余阶段的采样预算。
pub(crate) fn allocation_audit_once(phase: &'static str) {
    let bit = match phase {
        "table_executor_pending_begin" => 1,
        "table_executor_required_ready" => 2,
        "table_executor_cache_return" => 4,
        "table_executor_gutter_return" => 8,
        "table_executor_decorations_return" => 16,
        "table_executor_blocks_return" => 32,
        _ => return, // 固定标签外不建立持久标识或日志载荷。
    };
    if ALLOCATION_ONCE.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        allocation_audit_point(phase); // 真实第一次到达才采样；不按请求或窗口重置。
    }
}

/// 只从 dladdr 返回的符号借用读取至多384字节；不碰 image 路径、源码或绝对地址。
///
/// # Safety
///
/// `symbol` 必须来自当前活动调用栈地址的成功 dladdr 结果，在本次同步读取期间所属 image 仍加载。
#[cfg(any(feature = "native-block-fragment-diagnostic", test))]
unsafe fn bounded_symbol(
    symbol: *const libc::c_char,
    bytes: &mut [u8; MAX_SYMBOL_BYTES],
) -> (usize, bool) {
    if symbol.is_null() {
        return (0, false); // 未解析符号不能通过另一映像路径或地址猜测其名字。
    }
    for (index, destination) in bytes.iter_mut().enumerate() {
        // SAFETY: dladdr 对已加载 image 返回有效 NUL 结尾符号名；遇 NUL 立即停止，最多读取固定上限。
        let value = unsafe { symbol.add(index).read() } as u8;
        if value == 0 {
            return (index, true);
        }
        // 控制字符和路径分隔符替换为 ?，保证一帧一行且不把 symbol 名当作路径或任意文本输出。
        *destination =
            if value.is_ascii_graphic() && !matches!(value, b'/' | b'\\') || value == b' ' {
                value
            } else {
                b'?'
            };
    }
    (bytes.len(), false) // 达到上限明确报告截断，不无界查找 NUL 或创建 String。
}

/// 首次非空精确全文 primitive 在原调用栈内观察；至多一份64帧回溯，不复制/替换/阻断原读取。
#[cfg(feature = "native-block-fragment-diagnostic")]
pub(crate) fn first_full_document_read(document_units: usize) {
    if document_units == 0 || !enabled() {
        return;
    }
    let Some(_scope) = DiagnosticScope::enter() else {
        return;
    };
    if FULL_READ_TRACED.swap(true, Ordering::Relaxed) {
        return; // 后续真实全文请求仍由原计数器完整记录，但不重复采栈或增加输出。
    }
    let mut output = std::io::stderr().lock();
    // 全文诊断有独立的一次保留位；Find预算耗尽也不能挤掉首次全读调用点的内存快照。
    emit_memory("first_nonempty_full_document_read", 0, &mut output);
    let mut frames = [std::ptr::null_mut::<libc::c_void>(); MAX_FRAMES];
    // SAFETY: 栈数组可写64个 void*；libc/SDK 声明 backtrace 最多填充给定 c_int 数量，不创建字符串表。
    let count = unsafe { libc::backtrace(frames.as_mut_ptr(), MAX_FRAMES as libc::c_int) };
    let Ok(count) = usize::try_from(count) else {
        let _ = writeln!(
            output,
            "native_probe_full_read valid=false reason=backtrace_unavailable document_units={document_units}"
        );
        return;
    };
    if count == 0 || count > frames.len() {
        let _ = writeln!(
            output,
            "native_probe_full_read valid=false reason=backtrace_count_invalid document_units={document_units}"
        );
        return; // 与已声明 ABI 不符时不索引或尝试恢复未知数量。
    }
    let _ = writeln!(
        output,
        "native_probe_full_read valid=true document_units={document_units} frames={count} frame_cap={MAX_FRAMES}"
    );
    for (index, frame) in frames[..count].iter().copied().enumerate() {
        // SAFETY: Dl_info 仅包含可空原始指针，零初始化合法；dladdr 读取当前活动栈地址并写入完整结构。
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        // SAFETY: frame 只作为 dladdr 的查找键，不解引用；info 为完整可写、自然对齐的 libc 结构。
        let resolved = unsafe { libc::dladdr(frame.cast_const(), &mut info) } != 0;
        if !resolved || info.dli_sname.is_null() || info.dli_saddr.is_null() {
            let _ = writeln!(
                output,
                "native_probe_full_read_frame index={index} symbol=<unresolved> offset=unavailable"
            );
            continue; // 不用 image filename 或裸地址补充无法解析的符号。
        }
        let mut name = [0_u8; MAX_SYMBOL_BYTES];
        // SAFETY: 成功 dladdr 的符号属于仍在本主线程调用栈使用的 image；本同步范围不卸载任何 image。
        let (length, terminated) = unsafe { bounded_symbol(info.dli_sname, &mut name) };
        let symbol = std::str::from_utf8(&name[..length]).unwrap_or("<invalid_symbol>");
        let offset = (frame as usize).checked_sub(info.dli_saddr as usize);
        if let Some(offset) = offset {
            let _ = writeln!(
                output,
                "native_probe_full_read_frame index={index} symbol={symbol} offset={offset} symbol_truncated={}",
                !terminated
            );
        } else {
            // 非同向地址不做无符号回绕；仍只输出有限符号，绝对坐标始终不输出。
            let _ = writeln!(
                output,
                "native_probe_full_read_frame index={index} symbol={symbol} offset=unavailable symbol_truncated={}",
                !terminated
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_vm_classification_never_publishes_zero_or_partial_fields() {
        // 失败状态与成功但较短的返回都不应泄漏未被内核完整认证的字段。
        for (status, count) in [
            (libc::KERN_FAILURE, TASK_VM_INFO_REV3_COUNT),
            (libc::KERN_SUCCESS, TASK_VM_INFO_REV3_COUNT - 1),
        ] {
            // SAFETY: 两个固定ABI结构只含整数；本测试构造输出事实，不调用内核或分配器。
            let mut vm: DiagnosticTaskVmInfoRev3 = unsafe { std::mem::zeroed() };
            vm.internal = u64::MAX;
            // SAFETY: libc实际结构只含四个整数；此零值是测试明确提供的malloc统计值。
            let malloc = unsafe { std::mem::zeroed() };
            let mut output = Vec::new();
            emit_memory_classes(
                "table_110_request_return",
                1,
                MemoryClasses {
                    vm,
                    vm_status: status,
                    vm_count: count,
                    malloc,
                },
                &mut output,
            );
            let output = std::str::from_utf8(&output).expect("固定ASCII分类日志");
            assert!(output.contains("vm_available=false"));
            assert!(!output.contains("internal_bytes="));
            assert!(!output.contains("resident_bytes="));
            assert!(!output.contains("graphics_footprint_bytes="));
            assert!(output.contains("native_malloc_classes")); // 独立的成功malloc快照仍然可见。
        }
    }

    #[test]
    fn symbol_reader_stops_at_nul_bounds_output_and_cannot_emit_paths_or_extra_lines() {
        let mut bytes = [0; MAX_SYMBOL_BYTES];
        // SAFETY: 测试静态C字符串在整个调用中存活且明确以NUL结束，不依赖dladdr或实际栈帧。
        let (length, complete) =
            unsafe { bounded_symbol(c"symbol/name\\part\nnext".as_ptr(), &mut bytes) };
        assert!(complete);
        assert_eq!(&bytes[..length], b"symbol?name?part?next");
        let mut long = [b'x'; MAX_SYMBOL_BYTES + 2];
        long[MAX_SYMBOL_BYTES + 1] = 0;
        // SAFETY: 长测试存储至少386字节且含NUL，超过384的内容不会读取或写入输出缓冲。
        let (length, complete) = unsafe { bounded_symbol(long.as_ptr().cast(), &mut bytes) };
        assert_eq!(length, MAX_SYMBOL_BYTES);
        assert!(!complete);
        assert!(bytes.iter().all(|value| *value == b'x'));
        assert_eq!(
            // SAFETY: 空指针是显式支持的无符号输入，不在此路径解引用。
            unsafe { bounded_symbol(std::ptr::null(), &mut bytes) },
            (0, false)
        );
    }
}
