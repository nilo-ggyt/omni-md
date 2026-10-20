//! 沿既有paged_viewport控制的已核REV3/V4 ABI记录Find组件真实内核分类，不签整产品通过。

unsafe extern "C" {
    #[link_name = "mach_task_self_"]
    static CURRENT_TASK: libc::mach_port_t;
}

fn word64(words: &[libc::integer_t; 84], offset: usize) -> u64 {
    assert!(offset.is_multiple_of(4) && offset + 8 <= std::mem::size_of_val(words));
    // SAFETY: 与tests/paged_viewport/metrics.rs已独立C头核验的REV3/84整数前缀相同。
    // 只读取完整返回范围；Mach存储4对齐，所以按值read_unaligned而不构造u64引用。
    unsafe {
        words
            .as_ptr()
            .add(offset / 4)
            .cast::<u64>()
            .read_unaligned()
    }
}

pub(super) fn sample(stage: &str) -> Result<(), String> {
    let mut words = [0; 84];
    let mut count = 84;
    // SAFETY: 查询原当前task公开REV3，输出为84个可写integer_t；不使用其它进程的端口。
    let status = unsafe { libc::task_info(CURRENT_TASK, 22, words.as_mut_ptr(), &mut count) };
    if status != libc::KERN_SUCCESS || count != 84 {
        return Err(format!(
            "真实Find内核分类不可用：status={status}, count={count}"
        ));
    }
    let resident = word64(&words, 16);
    let internal = word64(&words, 48);
    let external = word64(&words, 64);
    let footprint = word64(&words, 144);
    let graphics = word64(&words, 272) as i64;
    // SAFETY: 公开rusage_info_v4只有固定整数/字节字段，零初始化有效；失败后不发布字段。
    let mut usage: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: 真实V4存储沿原公开rusage_info_t*签名传递，只查询当前进程的资源账。
    let usage_status = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            (&mut usage as *mut libc::rusage_info_v4).cast::<libc::rusage_info_t>(),
        )
    };
    if usage_status != 0 {
        return Err("真实Find lifetime footprint峰值不可用".into());
    }
    // SAFETY: rusage按公开C ABI完整初始化为可写存储，调用失败时不读取未返回字段。
    let mut resident_usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: RUSAGE_SELF只查询当前进程，指针在整个同步调用中有效。
    let resident_status = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut resident_usage) };
    if resident_status != 0 {
        return Err("真实Find RSS峰值不可用".into());
    }
    let max_resident =
        u64::try_from(resident_usage.ru_maxrss).map_err(|_| "真实Find RSS峰值不能为负")?;
    println!(
        "native_body_find memory_stage={stage} resident_bytes={resident} internal_bytes={internal} external_bytes={external} footprint_bytes={footprint} graphics_footprint_bytes={graphics} lifetime_footprint_peak_bytes={} max_resident_bytes={max_resident} isolated=true whole_product=false formal=false",
        usage.ri_lifetime_max_phys_footprint
    );
    Ok(()) // 诊断只发布真实值；是否严格低于150MB必须由包含全部场景的独立产品验收判断。
}
