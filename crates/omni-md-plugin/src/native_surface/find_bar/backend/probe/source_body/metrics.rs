//! Record actual Find-component kernel categories using the reviewed REV3/V4 ABI
//! shared with paged_viewport controls. This grants no whole-product acceptance.

unsafe extern "C" {
    #[link_name = "mach_task_self_"]
    static CURRENT_TASK: libc::mach_port_t;
}

fn word64(words: &[libc::integer_t; 84], offset: usize) -> u64 {
    assert!(offset.is_multiple_of(4) && offset + 8 <= std::mem::size_of_val(words));
    // SAFETY: This is the same REV3/84-integer prefix independently checked against
    // C headers in tests/paged_viewport/metrics.rs. Read within the returned range;
    // Mach storage is four-byte aligned, so copy unaligned rather than form a u64 reference.
    unsafe {
        words
            .as_ptr()
            .add(offset / 4)
            .cast::<u64>()
            .read_unaligned()
    }
}

/// Read only this process's actual counters. Publish no uninitialized/partial
/// snapshot on syscall failure, and do not equate footprint with peak RSS.
pub(super) fn sample(stage: &str) -> Result<(), String> {
    let mut words = [0; 84];
    let mut count = 84;
    // SAFETY: Query the current task's public REV3 into 84 writable integers;
    // this uses no other process's task port.
    let status = unsafe { libc::task_info(CURRENT_TASK, 22, words.as_mut_ptr(), &mut count) };
    if status != libc::KERN_SUCCESS || count != 84 {
        return Err(format!(
            "真实Find内核分类不可用：status={status}, count={count}"
        ));
    }
    let resident = word64(&words, 16);
    let internal = word64(&words, 48);
    let external = word64(&words, 64);
    // REV3 already contains reusable bytes. Report that fact without an allocator
    // query or any attempt to change reclamation during frame sampling.
    let reusable = word64(&words, 80);
    let footprint = word64(&words, 144);
    let graphics = word64(&words, 272) as i64;
    // SAFETY: Public rusage_info_v4 has fixed integer/byte fields that admit zero
    // initialization; failed calls publish no fields.
    let mut usage: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: Actual V4 storage follows the public rusage_info_t* signature;
    // query only the current process's resource ledger.
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
    // SAFETY: Fully initialized writable rusage storage matches the public C ABI;
    // do not read returned fields if the call fails.
    let mut resident_usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: RUSAGE_SELF queries this process only; storage lives throughout the synchronous call.
    let resident_status = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut resident_usage) };
    if resident_status != 0 {
        return Err("真实Find RSS峰值不可用".into());
    }
    let max_resident =
        u64::try_from(resident_usage.ru_maxrss).map_err(|_| "真实Find RSS峰值不能为负")?;
    println!(
        "native_body_find memory_stage={stage} resident_bytes={resident} internal_bytes={internal} external_bytes={external} reusable_bytes={reusable} footprint_bytes={footprint} graphics_footprint_bytes={graphics} lifetime_footprint_peak_bytes={} max_resident_bytes={max_resident} isolated=true whole_product=false formal=false",
        usage.ri_lifetime_max_phys_footprint
    );
    // Report actual diagnostic values only; an independent complete product workload owns budget verification.
    Ok(())
}
