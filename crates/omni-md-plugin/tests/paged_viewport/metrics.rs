//! 同进程公开内核分类；与已独立C头核验的REV3/84整数前缀保持相同ABI。

pub struct Snapshot {
    pub graphics: i64,
    pub process_within_bound: bool,
}

unsafe extern "C" {
    #[link_name = "mach_task_self_"]
    static CURRENT_TASK: libc::mach_port_t;
}

fn word64(words: &[libc::integer_t; 84], offset: usize) -> u64 {
    assert!(offset.is_multiple_of(4) && offset + 8 <= std::mem::size_of_val(words));
    // SAFETY: 调用方只给公开REV3内已核offsetof；范围经上方校验。Mach存储仅4对齐，
    // 因此按native-endian read_unaligned按值读取，不构造u64引用或读取未返回字段。
    unsafe {
        words
            .as_ptr()
            .add(offset / 4)
            .cast::<u64>()
            .read_unaligned()
    }
}

pub fn sample(stage: &str, height: f64) -> Snapshot {
    let mut words = [0; 84];
    let mut count = 84;
    // SAFETY: 仅查询公开当前task端口；输出为336字节/84个integer_t可写栈存储。
    let status = unsafe { libc::task_info(CURRENT_TASK, 22, words.as_mut_ptr(), &mut count) };
    assert_eq!(status, libc::KERN_SUCCESS, "分类查询不可用不能以0通过");
    assert_eq!(count, 84, "REV3字段不足不能读取未返回的尾部");
    let resident = word64(&words, 16);
    let internal = word64(&words, 48);
    let external = word64(&words, 64);
    let footprint = word64(&words, 144);
    let graphics = word64(&words, 272) as i64;
    // SAFETY: rusage_info_v4只含整数/固定字节字段，零初始化合法，失败字段不被发布。
    let mut usage: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: 真实v4存储按libproc公开rusage_info_t*签名传递；不读取目标进程或用户数据。
    let usage_status = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            (&mut usage as *mut libc::rusage_info_v4).cast::<libc::rusage_info_t>(),
        )
    };
    assert_eq!(usage_status, 0, "实际峰值不可用不能称控制通过");
    // Darwin公开getrusage的ru_maxrss单位为字节；真实最大RSS也纳入隔离控制，不能只压低graphics。
    // SAFETY: rusage由公开C ABI定义，完整可写零初始化；调用失败时不发布任何未返回字段。
    let mut resident_usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: RUSAGE_SELF只查询本进程，指针指向本语句完整有效的公开rusage存储。
    let resident_status = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut resident_usage) };
    assert_eq!(resident_status, 0, "真实RSS峰值不可用不能通过");
    let max_resident = u64::try_from(resident_usage.ru_maxrss).expect("RSS峰值不能为负");
    println!(
        "paged_viewport stage={stage} virtual_height={height} resident_bytes={resident} internal_bytes={internal} external_bytes={external} footprint_bytes={footprint} graphics_footprint_bytes={graphics} lifetime_footprint_peak_bytes={} max_resident_bytes={max_resident} isolated=true whole_product=false formal=false",
        usage.ri_lifetime_max_phys_footprint
    );
    Snapshot {
        graphics,
        process_within_bound: footprint < 100_000_000
            && usage.ri_lifetime_max_phys_footprint < 100_000_000
            && max_resident < 100_000_000,
    }
}
