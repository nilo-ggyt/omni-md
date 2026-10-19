//! 私有剪贴板的真实跨进程复制回归；只验证显式输出，不代表完整产品或窗口输入验收。

use super::*;
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use std::time::{Duration, Instant};

const DOCUMENT_LIMIT: usize = 32 * 1024 * 1024;
const SCRATCH_BYTES: usize = 64 * 1024;
const ORIGINAL_PATTERN: &[u8] = b"0123456789abcdef";
const MIXED_SOURCE: &str = "English\r\n中文 😀 e\u{301}\r\n末尾";
const UNSAVED_PREFIX: &str = "未保存😀\r\n";
const PRIVATE_BOARD_PREFIX: &str = "nilo.omni.copyall-native.";

#[derive(Clone, Copy)]
/// 场景直接描述三种真实canonical输入，不另建期待全文或复制接口。
enum CopyCase {
    Empty,
    Unicode,
    Limit,
}

impl CopyCase {
    /// 子进程只接受本回归预声明的场景，不能把未知输入当成较小载荷。
    fn from_name(name: &str) -> Result<Self, String> {
        match name {
            "empty" => Ok(Self::Empty),
            "unicode" => Ok(Self::Unicode),
            "limit" => Ok(Self::Limit),
            _ => Err("私有剪贴板读取场景无效".into()),
        }
    }

    /// 仅序列化场景身份供独立进程选择同一固定oracle，不传正文或临时文件路径。
    fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Unicode => "unicode",
            Self::Limit => "limit",
        }
    }

    /// 上限场景预留真实未保存前缀的字节空间，最终canonical恰好达到产品32MiB边界。
    fn original_len(self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Unicode => MIXED_SOURCE.len(),
            Self::Limit => DOCUMENT_LIMIT - UNSAVED_PREFIX.len(),
        }
    }

    /// 非空场景先通过原编辑事务插入有限Unicode草稿；空文档保持真正的零字节值。
    fn prefix(self) -> &'static str {
        match self {
            Self::Empty => "",
            Self::Unicode | Self::Limit => UNSAVED_PREFIX,
        }
    }

    /// 最终期待长度只由固定夹具和真实编辑决定，不从待测输出反推。
    fn copied_len(self) -> usize {
        self.prefix().len() + self.original_len()
    }

    /// 大输入按坐标算单个期待字节；读取进程始终不持有第二份32MiB缓冲。
    fn expected_byte(self, offset: usize) -> u8 {
        let prefix = self.prefix().as_bytes();
        if offset < prefix.len() {
            return prefix[offset]; // 中文、CRLF和emoji按原UTF-8字节保留，不做正规化。
        }
        let original_offset = offset - prefix.len();
        match self {
            Self::Unicode => MIXED_SOURCE.as_bytes()[original_offset],
            Self::Limit => ORIGINAL_PATTERN[original_offset % ORIGINAL_PATTERN.len()],
            Self::Empty => unreachable!("空载荷没有可比较的字节"),
        }
    }
}

/// 只拥有本回归创建的唯一命名板；任何正常或失败退出均不操作用户通用剪贴板。
struct PrivatePasteboard(Retained<NSPasteboard>);

impl Drop for PrivatePasteboard {
    /// 归还私有系统名称后，由Native原NSData deallocator决定实际输出映射释放时点。
    fn drop(&mut self) {
        // SAFETY: 本板由本函数独立创建，Drop仍在原主线程；releaseGlobally无参数且返回void。
        // objc2-app-kit当前绑定缺少此公开方法，因此只在这个实际资源归还点发送原选择器。
        unsafe {
            let _: () = msg_send![&*self.0, releaseGlobally];
        }
    }
}

/// 只在生产prepare的同步区间对照实际普通文件descriptor；不把AppKit后续FD归给正文。
fn regular_file_descriptors() -> Result<Vec<(u32, u64, u64)>, String> {
    let mut descriptors = Vec::new();
    for entry in std::fs::read_dir("/dev/fd").map_err(|cause| cause.to_string())? {
        let entry = entry.map_err(|cause| cause.to_string())?;
        let name = entry.file_name();
        let Some(fd) = name.to_str().and_then(|name| name.parse().ok()) else {
            continue; // fdescfs的目录元数据不是进程descriptor。
        };
        match std::fs::metadata(entry.path()) {
            Ok(metadata) if metadata.is_file() => {
                descriptors.push((fd, metadata.dev(), metadata.ino()));
            }
            Ok(_) => {} // 标准管道、套接字和枚举目录不属于这次文件输出。
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {} // 刚关闭的目录项没有活FD。
            Err(cause) => return Err(cause.to_string()), // 无法读取实际资源身份不能冒称已关闭。
        }
    }
    descriptors.sort_unstable();
    Ok(descriptors)
}

/// 固定64KiB写scratch生成上限文件；普通Unicode样本只用静态短串。
fn write_original(path: &Path, case: CopyCase) -> Result<(), String> {
    let mut file = std::fs::File::create(path).map_err(|cause| cause.to_string())?;
    match case {
        CopyCase::Empty => {} // 真实空文件不能用含换行的占位文本代替。
        CopyCase::Unicode => {
            file.write_all(MIXED_SOURCE.as_bytes())
                .map_err(|cause| cause.to_string())?;
        }
        CopyCase::Limit => {
            let scratch: [u8; SCRATCH_BYTES] =
                std::array::from_fn(|offset| ORIGINAL_PATTERN[offset % ORIGINAL_PATTERN.len()]);
            let mut remaining = case.original_len();
            while remaining != 0 {
                let count = remaining.min(scratch.len());
                file.write_all(&scratch[..count])
                    .map_err(|cause| cause.to_string())?;
                remaining -= count; // 模式长度整除scratch，最后短片也保持从原文零点开始的序列。
            }
        }
    }
    file.sync_all().map_err(|cause| cause.to_string())?;
    drop(file); // 后续Doc和输出资源观察不包含夹具生成器自己的FD。
    Ok(())
}

/// 准备完成后先释放原Doc及其文件，再经同一生产publish交付独立输出。
fn publish_case(pasteboard: &NSPasteboard, case: CopyCase) -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|cause| cause.to_string())?;
    let path = directory.path().join("复制边界.md");
    write_original(&path, case)?;
    let before = regular_file_descriptors()?;
    let mut document = MarkdownDocumentBuffer::open(&path, DOCUMENT_LIMIT as u64)
        .map_err(|cause| cause.to_string())?;
    let backing = document.original_backing_observer();
    if !case.prefix().is_empty() {
        // 真实canonical编辑产生独立有限piece和Undo；不能只在夹具磁盘内容中预填草稿。
        document
            .replace_utf16_range(
                0..0,
                case.prefix(),
                MarkdownTextSelection::caret(case.prefix().encode_utf16().count()),
            )
            .map_err(|cause| cause.to_string())?;
    }
    let generation = document.generation();
    let revision = document
        .current_revision()
        .map_err(|cause| cause.to_string())?;
    let selection = document.selection();
    let edited = !case.prefix().is_empty();
    if document.byte_len() != case.copied_len()
        || document.is_dirty() != edited
        || document.can_undo() != edited
        || document.can_redo()
    {
        return Err("复制夹具没有形成预声明的canonical长度或未保存历史".into());
    }
    let output = MarkdownPasteboardOutput::prepare(&document).map_err(|cause| cause.to_string())?;
    if document.generation() != generation
        || document
            .current_revision()
            .map_err(|cause| cause.to_string())?
            != revision
        || document.selection() != selection
        || document.is_dirty() != edited
        || document.can_undo() != edited
        || document.can_redo()
    {
        return Err("复制准备改变了原canonical、选区或Undo历史".into());
    }
    drop(document);
    directory.close().map_err(|cause| cause.to_string())?;
    match (case, backing) {
        (CopyCase::Empty, None) => {} // 空原文从未拥有源映射，不能要求虚构的munmap收据。
        (CopyCase::Unicode | CopyCase::Limit, Some(backing))
            if backing.unmap_succeeded()
                && backing.mapped_bytes() == 0
                && backing.managed_resident_bytes() == 0
                && !backing.failed() => {}
        (_, backing) => {
            return Err(format!(
                "原文没有真实释放：mapped={:?} errno={:?}",
                backing.as_ref().map(|owner| owner.mapped_bytes()),
                backing.as_ref().and_then(|owner| owner.first_errno()),
            ));
        }
    }
    if regular_file_descriptors()? != before {
        return Err("原文释放后复制准备仍改变了普通文件descriptor集合".into());
    }
    println!(
        "native_clipboard role=writer stage=original_released case={} bytes={} source_map=0 source_files=0 canonical_unchanged=true whole_product=false formal=false",
        case.name(),
        case.copied_len(),
    );
    // 上面的Doc和原路径已经不存在；Native重入及独立读取只能依赖用户复制的输出本身。
    autoreleasepool(|_| output.publish(pasteboard)).map_err(|cause| cause.to_string())
}

/// 只覆盖本进程一次真实prepare的soft limit；提前退出也先恢复原值，不能影响后续Native或子进程。
struct OpenFileLimit(libc::rlimit);

impl Drop for OpenFileLimit {
    /// 原hard limit始终未变，恢复失败属于测试进程不可继续的OS资源错误，不带受限状态进入AppKit。
    fn drop(&mut self) {
        // SAFETY: 保存值来自本进程成功getrlimit，hard limit从未降低；这里只恢复同一公开资源限制。
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.0) } != 0 {
            eprintln!(
                "恢复复制失败回归的文件数限制失败：{}",
                io::Error::last_os_error()
            );
            std::process::abort(); // 不执行私有板Drop或启动reader，避免把未恢复的限制带到其他Native操作。
        }
    }
}

/// 真实EMFILE只能使输出准备失败，不能改变canonical或已经发布的私有剪贴内容。
fn prepare_failure_preserves_previous_copy(
    pasteboard: &NSPasteboard,
    name: &str,
) -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|cause| cause.to_string())?;
    let path = directory.path().join("输出准备失败.md");
    write_original(&path, CopyCase::Unicode)?;
    let before = regular_file_descriptors()?;
    let mut document = MarkdownDocumentBuffer::open(&path, DOCUMENT_LIMIT as u64)
        .map_err(|cause| cause.to_string())?;
    document
        .replace_utf16_range(
            0..0,
            UNSAVED_PREFIX,
            MarkdownTextSelection::caret(UNSAVED_PREFIX.encode_utf16().count()),
        )
        .map_err(|cause| cause.to_string())?;
    let backing = document
        .original_backing_observer()
        .ok_or("非空原文缺少实际映射观察者")?;
    let generation = document.generation();
    let revision = document
        .current_revision()
        .map_err(|cause| cause.to_string())?;
    let selection = document.selection();
    let change = pasteboard.changeCount(); // 限制前取得原已发布版本，soft=0期间不调用任何AppKit方法。
    let mut original = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: original是可写的完整rlimit，本进程只读取标准RLIMIT_NOFILE，不访问其他进程。
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) } != 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let restore = OpenFileLimit(original);
    let reduced = libc::rlimit {
        rlim_cur: 0,
        rlim_max: original.rlim_max,
    };
    // SAFETY: 只降低本进程soft值，保留原hard；上方真实Doc及其有限草稿已完整创建。
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &reduced) } != 0 {
        return Err(io::Error::last_os_error().to_string()); // restore先析构，才允许上层归还私有板。
    }
    let attempted = MarkdownPasteboardOutput::prepare(&document);
    drop(restore); // 紧接prepare显式恢复；检查错误、查询剪贴板或spawn之前不再保留soft=0。
    let error = match attempted {
        Err(error) => error,
        Ok(output) => {
            drop(output); // 意外成功也只释放本次独立输出，绝不发布或清空原私有板。
            return Err("文件数软限制为零时生产复制准备仍意外成功".into());
        }
    };
    // OutputUnavailable携带真实io::Error；tempfile的PathError保留kind，却不在source链公开内层OS错误。
    // 有直接errno时必须精确匹配；被包装时使用实际保留的kind，不解析路径/文案或伪造raw错误码。
    let output_error =
        std::error::Error::source(&error).and_then(|cause| cause.downcast_ref::<io::Error>());
    let actual_errno = output_error.and_then(io::Error::raw_os_error);
    let actual_kind = output_error.map(io::Error::kind);
    let exhausted = match actual_errno {
        Some(errno) => errno == libc::EMFILE,
        None => actual_kind == Some(io::Error::from_raw_os_error(libc::EMFILE).kind()),
    };
    if !exhausted {
        return Err(format!("生产复制准备没有报告真实EMFILE：{error:?}"));
    }
    if document.generation() != generation
        || document
            .current_revision()
            .map_err(|cause| cause.to_string())?
            != revision
        || document.selection() != selection
        || document.byte_len() != CopyCase::Unicode.copied_len()
        || !document.is_dirty()
        || !document.can_undo()
        || document.can_redo()
    {
        return Err("输出准备失败改变了canonical、选区或未保存历史".into());
    }
    drop(document);
    directory.close().map_err(|cause| cause.to_string())?;
    if !backing.unmap_succeeded()
        || backing.mapped_bytes() != 0
        || backing.managed_resident_bytes() != 0
        || backing.failed()
        || regular_file_descriptors()? != before
    {
        return Err("输出准备失败后原文映射或普通文件descriptor没有真实归还".into());
    }
    if pasteboard.changeCount() != change {
        return Err("输出准备失败改变了原私有剪贴板版本".into());
    }
    read_in_child(name, CopyCase::Unicode)?; // 原文已释放、limit已恢复，独立进程仍须完整读到先前那次复制。
    println!(
        "native_clipboard role=writer stage=prepare_emfile_preserved actual_errno={actual_errno:?} actual_kind={actual_kind:?} restored_soft={} restored_hard={} canonical_unchanged=true previous_reader_exact=true source_map=0 source_files=0 whole_product=false formal=false",
        original.rlim_cur, original.rlim_max,
    );
    Ok(())
}

/// 只等待当前回归自己启动的独立读取者；deadline是失败边界，不能据时间宣布资源释放。
fn read_in_child(name: &str, case: CopyCase) -> Result<(), String> {
    let mut child = Command::new(std::env::current_exe().map_err(|cause| cause.to_string())?)
        .args(["--reader", name, case.name()])
        .spawn()
        .map_err(|cause| cause.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("独立剪贴板读取失败：{status}")),
            Ok(None) if Instant::now() < deadline => {
                // 完整NSData已同步发布，没有延迟data provider；此处不发明产品事件或嵌套runloop。
                std::thread::sleep(Duration::from_millis(10));
            }
            pending => {
                // 只终止并回收本函数创建的子进程，不遗留挂起消费者或误触别的应用。
                let killed = child.kill();
                let waited = child.wait();
                return Err(format!(
                    "独立剪贴板读取未完成：state={pending:?} kill={killed:?} wait={waited:?}"
                ));
            }
        }
    }
}

/// 真实生产CopyAll经同一私有板重复发布；先撤原文，再由另一个进程逐字节核验。
pub fn run_native_markdown_clipboard_control(mtm: MainThreadMarker) -> Result<(), String> {
    let _application = NSApplication::sharedApplication(mtm);
    // 名称由固定前缀、进程身份和当前纳秒组成，不接收外部名称或路径，也不访问General板。
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|cause| cause.to_string())?
        .as_nanos();
    let name = format!("{PRIVATE_BOARD_PREFIX}{}-{timestamp}", std::process::id());
    let pasteboard =
        PrivatePasteboard(NSPasteboard::pasteboardWithName(&NSString::from_str(&name)));
    for case in [
        CopyCase::Empty,
        CopyCase::Unicode,
        CopyCase::Limit,
        CopyCase::Unicode,
        CopyCase::Empty,
    ] {
        if matches!(case, CopyCase::Limit) {
            // 前一Unicode已成功跨进程读取；真实准备失败必须保留该已发布值，随后才继续上限复制。
            prepare_failure_preserves_previous_copy(&pasteboard.0, &name)?;
        }
        // Limit前的Unicode与之后的Unicode都经同一原发布器，覆盖真实覆盖旧输出的寿命边界。
        publish_case(&pasteboard.0, case)?;
        read_in_child(&name, case)?;
        println!(
            "native_clipboard role=writer stage=reader_exact case={} bytes={} whole_product=false formal=false",
            case.name(),
            case.copied_len(),
        );
    }
    pasteboard.0.clearContents(); // 只清空本回归私有板；不代表系统早先持有的NSData已销毁。
    drop(pasteboard); // 真正releaseGlobally后，生产deallocator的实际munmap日志独立保留。
    println!(
        "native_clipboard role=writer stage=private_board_released whole_product=false formal=false"
    );
    Ok(())
}

/// 独立消费者只借系统NSData作固定片比较；不创建完整期待String或大NSData副本。
pub fn read_native_markdown_clipboard_control(
    name: &str,
    case: &str,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    if !name.starts_with(PRIVATE_BOARD_PREFIX) {
        return Err("读取者只接受本回归的私有剪贴板名称".into()); // 外部输入不能指向用户通用板。
    }
    let case = CopyCase::from_name(case)?;
    let _application = NSApplication::sharedApplication(mtm);
    autoreleasepool(|_| {
        let pasteboard = NSPasteboard::pasteboardWithName(&NSString::from_str(name));
        let change = pasteboard.changeCount();
        // SAFETY: 绑定导出的是系统只读常量，类型与生产publish使用的NSPasteboardTypeString相同。
        let string_type = unsafe { NSPasteboardTypeString };
        let data = pasteboard
            .dataForType(string_type)
            .ok_or("独立进程没有读取到复制的UTF-8数据")?;
        if data.length() != case.copied_len() {
            return Err(format!(
                "剪贴板实际长度不符：actual={} expected={}",
                data.length(),
                case.copied_len(),
            ));
        }
        // SAFETY: NSData由系统完整返回且本作用域不发送任何修改data的消息；Retained覆盖全部借用。
        let bytes = unsafe { data.as_bytes_unchecked() };
        for (index, chunk) in bytes.chunks(SCRATCH_BYTES).enumerate() {
            for (local, byte) in chunk.iter().copied().enumerate() {
                let offset = index * SCRATCH_BYTES + local;
                if byte != case.expected_byte(offset) {
                    return Err(format!("剪贴板原字节在{offset}处不符")); // 首个差异即失败，不记录用户载荷。
                }
            }
        }
        if matches!(case, CopyCase::Empty | CopyCase::Unicode) {
            // 只有有限Unicode样本调用stringForType；32MiB场景不创建NSString镜像。
            let string = pasteboard
                .stringForType(string_type)
                .ok_or("系统没有将短UTF-8载荷识别成文本")?;
            let expected = match case {
                CopyCase::Empty => String::new(),
                CopyCase::Unicode => format!("{UNSAVED_PREFIX}{MIXED_SOURCE}"),
                CopyCase::Limit => unreachable!("上限场景不进入字符串语义检查"),
            };
            if string.to_string() != expected {
                return Err("短Unicode载荷的系统文本语义改变了原内容或换行".into());
            }
        }
        if pasteboard.changeCount() != change {
            return Err("独立读取期间私有剪贴板发生了变化".into()); // 不能混读不同发布的长度与内容。
        }
        println!(
            "native_clipboard role=reader stage=exact case={} bytes={} whole_product=false formal=false",
            case.name(),
            bytes.len(),
        );
        Ok(())
    })
}
