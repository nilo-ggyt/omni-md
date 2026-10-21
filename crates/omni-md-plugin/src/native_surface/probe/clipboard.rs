//! Actual cross-process copy regression on a private pasteboard. It verifies
//! explicit output, independently from product memory or window-input acceptance.

use super::*;
use std::os::unix::fs::MetadataExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const DOCUMENT_LIMIT: usize = 32 * 1024 * 1024;
const SCRATCH_BYTES: usize = 64 * 1024;
const ORIGINAL_PATTERN: &[u8] = b"0123456789abcdef";
const MIXED_SOURCE: &str = "English\r\n中文 😀 e\u{301}\r\n末尾";
const UNSAVED_PREFIX: &str = "未保存😀\r\n";
const PRIVATE_BOARD_PREFIX: &str = "nilo.omni.copyall-native.";

#[derive(Clone, Copy)]
/// Three actual canonical input scenarios, without a second whole-document
/// expected buffer or substitute copy implementation.
enum CopyCase {
    Empty,
    Unicode,
    Limit,
}

impl CopyCase {
    /// Readers accept only a declared scenario; unknown input cannot silently
    /// select a smaller expected payload.
    fn from_name(name: &str) -> Result<Self, String> {
        match name {
            "empty" => Ok(Self::Empty),
            "unicode" => Ok(Self::Unicode),
            "limit" => Ok(Self::Limit),
            _ => Err("私有剪贴板读取场景无效".into()),
        }
    }

    /// Serialize only scenario identity to select the independent fixed oracle,
    /// without transferring document text or a temporary filesystem path.
    fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Unicode => "unicode",
            Self::Limit => "limit",
        }
    }

    /// Reserve the unsaved prefix so the limit scenario reaches the product's
    /// actual 32-MiB canonical extent after the real edit.
    fn original_len(self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Unicode => MIXED_SOURCE.len(),
            Self::Limit => DOCUMENT_LIMIT - UNSAVED_PREFIX.len(),
        }
    }

    /// Nonempty scenarios insert a finite Unicode prefix through the actual edit
    /// transaction; an empty document retains its genuine zero-byte value.
    fn prefix(self) -> &'static str {
        match self {
            Self::Empty => "",
            Self::Unicode | Self::Limit => UNSAVED_PREFIX,
        }
    }

    /// Derive expected length from declared fixtures and edits, never from copy output.
    fn copied_len(self) -> usize {
        self.prefix().len() + self.original_len()
    }

    /// Derive one expected byte by coordinate, without another 32-MiB reader buffer.
    fn expected_byte(self, offset: usize) -> u8 {
        let prefix = self.prefix().as_bytes();
        if offset < prefix.len() {
            return prefix[offset]; // Preserve original Chinese, CRLF, and emoji bytes without normalization.
        }
        let original_offset = offset - prefix.len();
        match self {
            Self::Unicode => MIXED_SOURCE.as_bytes()[original_offset],
            Self::Limit => ORIGINAL_PATTERN[original_offset % ORIGINAL_PATTERN.len()],
            Self::Empty => unreachable!("空载荷没有可比较的字节"),
        }
    }
}

/// Own only this regression's named board. Normal and failed returns never
/// access or clear the user's General pasteboard.
struct PrivatePasteboard(Retained<NSPasteboard>);

impl Drop for PrivatePasteboard {
    /// Return the private namespace; the actual NSData deallocator independently
    /// determines when the published output mapping can be unmapped.
    fn drop(&mut self) {
        // SAFETY: This independently created board remains on the original main
        // thread. The public releaseGlobally selector takes no arguments and
        // returns void; the pinned generated binding omits this method.
        unsafe {
            let _: () = msg_send![&*self.0, releaseGlobally];
        }
    }
}

/// Compare actual regular-file descriptors only around synchronous preparation,
/// without attributing later AppKit descriptors to the original document.
fn regular_file_descriptors() -> Result<Vec<(u32, u64, u64)>, String> {
    let mut descriptors = Vec::new();
    for entry in std::fs::read_dir("/dev/fd").map_err(|cause| cause.to_string())? {
        let entry = entry.map_err(|cause| cause.to_string())?;
        let name = entry.file_name();
        let Some(fd) = name.to_str().and_then(|name| name.parse().ok()) else {
            continue; // fdescfs directory metadata does not identify a process descriptor.
        };
        match std::fs::metadata(entry.path()) {
            Ok(metadata) if metadata.is_file() => {
                descriptors.push((fd, metadata.dev(), metadata.ino()));
            }
            Ok(_) => {} // Pipes, sockets, and the enumeration directory are not this file output.
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {} // The descriptor has already closed.
            Err(cause) => return Err(cause.to_string()), // Unreadable identity cannot prove descriptor release.
        }
    }
    descriptors.sort_unstable();
    Ok(descriptors)
}

/// Stream the limit fixture through fixed 64-KiB scratch; Unicode fixtures use
/// only finite static strings. All paths belong to the temporary scenario.
fn write_original(path: &Path, case: CopyCase) -> Result<(), String> {
    let mut file = std::fs::File::create(path).map_err(|cause| cause.to_string())?;
    match case {
        CopyCase::Empty => {} // A newline placeholder cannot stand in for an actual empty file.
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
                remaining -= count; // The pattern divides scratch evenly, preserving tail-byte coordinates.
            }
        }
    }
    file.sync_all().map_err(|cause| cause.to_string())?;
    drop(file); // Later source/output receipts exclude the fixture writer's own descriptor.
    Ok(())
}

/// Release the actual document and its file after preparation, then publish
/// independent output through the original production path.
fn publish_case(pasteboard: &NSPasteboard, case: CopyCase) -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|cause| cause.to_string())?;
    let path = directory.path().join("复制边界.md");
    write_original(&path, case)?;
    let before = regular_file_descriptors()?;
    let mut document = MarkdownDocumentBuffer::open(&path, DOCUMENT_LIMIT as u64)
        .map_err(|cause| cause.to_string())?;
    let backing = document.original_backing_observer();
    if !case.prefix().is_empty() {
        // Create actual edit pieces and undo history; prewriting a fixture with
        // the prefix would not exercise unsaved canonical source ownership.
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
        (CopyCase::Empty, None) => {} // Empty source acquired no mapping and owes no fabricated unmap receipt.
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
    // The document and original path have ended. Native reentry and an independent
    // reader must now depend only on the explicitly copied output itself.
    autoreleasepool(|_| output.publish(pasteboard)).map_err(|cause| cause.to_string())
}

/// Own this process's temporary soft limit for one real preparation. Early
/// returns restore it before any later Native operation or reader spawn.
struct OpenFileLimit(libc::rlimit);

impl Drop for OpenFileLimit {
    /// The hard limit never changes. Failed restoration aborts this isolated
    /// writer instead of entering AppKit while descriptor creation remains restricted.
    fn drop(&mut self) {
        // SAFETY: The saved value came from this process's successful getrlimit;
        // its hard limit was never lowered. Restore the same public resource limit.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.0) } != 0 {
            eprintln!(
                "native_clipboard operation=restore_file_limit outcome=failed errno={:?}",
                io::Error::last_os_error().raw_os_error()
            );
            std::process::abort(); // The parent retains the recorded private name for cleanup after reaping this writer.
        }
    }
}

/// Actual EMFILE rejects output preparation without changing canonical data or
/// previously published private pasteboard content.
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
    let change = pasteboard.changeCount(); // Capture before restriction; soft=0 never crosses an AppKit call.
    let mut original = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: original is a complete writable rlimit. Query only this process's
    // standard RLIMIT_NOFILE without accessing another process.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) } != 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let restore = OpenFileLimit(original);
    let reduced = libc::rlimit {
        rlim_cur: 0,
        rlim_max: original.rlim_max,
    };
    // SAFETY: Lower only this process's soft limit, preserving its hard limit.
    // The real document and finite unsaved draft were completely created above.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &reduced) } != 0 {
        return Err(io::Error::last_os_error().to_string()); // Restoration precedes returning the private board.
    }
    let attempted = MarkdownPasteboardOutput::prepare(&document);
    drop(restore); // Restore immediately before error inspection, pasteboard access, or child spawning.
    let error = match attempted {
        Err(error) => error,
        Ok(output) => {
            drop(output); // Unexpected success releases this output without publishing or clearing the previous copy.
            return Err("文件数软限制为零时生产复制准备仍意外成功".into());
        }
    };
    // OutputUnavailable retains a real io::Error. tempfile's PathError preserves
    // its kind but may hide the inner errno; match actual retained facts without
    // parsing text/paths or manufacturing an OS error code.
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
    read_in_child(name, CopyCase::Unicode)?; // Source ended and limits restored; the prior copy must remain independently readable.
    println!(
        "native_clipboard role=writer stage=prepare_emfile_preserved actual_errno={actual_errno:?} actual_kind={actual_kind:?} restored_soft={} restored_hard={} canonical_unchanged=true previous_reader_exact=true source_map=0 source_files=0 whole_product=false formal=false",
        original.rlim_cur, original.rlim_max,
    );
    Ok(())
}

/// Own exactly the independent reader started below. Child::drop alone does not
/// stop or reap a process, so unwinding must also finish this concrete resource.
struct ClipboardReaderChild(Child);

impl Drop for ClipboardReaderChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return; // The original child was already reaped; never act on a reused PID.
        }
        let killed = self.0.kill();
        let waited = self.0.wait();
        if let Err(cause) = waited {
            // Drop cannot return an error. Preserve the actual owned PID and OS
            // result without claiming that a failed wait released the process.
            eprintln!(
                "native_clipboard_reader operation=cleanup outcome=failed pid={} kill_errno={:?} wait_errno={:?}",
                self.0.id(),
                killed.err().and_then(|cause| cause.raw_os_error()),
                cause.raw_os_error(),
            );
        }
    }
}

/// Wait only for this regression's actual reader. The deadline rejects the
/// operation; elapsed time never substitutes for a real child-exit receipt.
fn read_in_child(name: &str, case: CopyCase) -> Result<(), String> {
    let mut child = ClipboardReaderChild(
        Command::new(std::env::current_exe().map_err(|cause| cause.to_string())?)
            .args(["--reader", name, case.name()])
            .spawn()
            .map_err(|cause| cause.to_string())?,
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.0.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("独立剪贴板读取失败：{status}")),
            Ok(None) if Instant::now() < deadline => {
                // NSData is already synchronously published, without a deferred
                // provider; do not invent product events or a nested run loop.
                std::thread::sleep(Duration::from_millis(10));
            }
            pending => {
                // Stop and reap only this owned child; another application or
                // an unrelated clipboard consumer is outside this authority.
                let killed = child.0.kill();
                let waited = child.0.wait();
                return Err(format!(
                    "独立剪贴板读取未完成：state={pending:?} kill={killed:?} wait={waited:?}"
                ));
            }
        }
    }
}

/// Repeat actual production CopyAll publication on one private board, releasing
/// each original source before another process verifies every output byte.
pub fn run_native_markdown_clipboard_control(mtm: MainThreadMarker) -> Result<(), String> {
    let _application = NSApplication::sharedApplication(mtm);
    // Derive the private name from this process and current nanoseconds, without
    // accepting an external board/path or accessing General.
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|cause| cause.to_string())?
        .as_nanos();
    let name = format!("{PRIVATE_BOARD_PREFIX}{}-{timestamp}", std::process::id());
    // Flush the exact owned identity before acquisition. A parent can release
    // this name after reaping an aborted writer, without touching General.
    println!("native_clipboard private_pasteboard_name={name}");
    io::stdout().flush().map_err(|cause| cause.to_string())?;
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
            // Unicode has already been independently read. Real preparation
            // failure must preserve that copy before the limit scenario can continue.
            prepare_failure_preserves_previous_copy(&pasteboard.0, &name)?;
        }
        // The same publisher handles Unicode before and after the large output,
        // exercising actual replacement and retirement of the previous copy.
        publish_case(&pasteboard.0, case)?;
        read_in_child(&name, case)?;
        println!(
            "native_clipboard role=writer stage=reader_exact case={} bytes={} whole_product=false formal=false",
            case.name(),
            case.copied_len(),
        );
    }
    pasteboard.0.clearContents(); // Clear only this private board; previously retained NSData may have independent Native owners.
    drop(pasteboard); // Actual deallocator/munmap receipts remain distinct from returning the namespace.
    println!(
        "native_clipboard role=writer stage=private_board_released whole_product=false formal=false"
    );
    Ok(())
}

/// The independent reader compares the actual NSData in finite chunks, without
/// another expected whole-document String or large NSData copy.
pub fn read_native_markdown_clipboard_control(
    name: &str,
    case: &str,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    if !name.starts_with(PRIVATE_BOARD_PREFIX) {
        return Err("读取者只接受本回归的私有剪贴板名称".into()); // Reject authority over General or another namespace.
    }
    let case = CopyCase::from_name(case)?;
    let _application = NSApplication::sharedApplication(mtm);
    autoreleasepool(|_| {
        let pasteboard = NSPasteboard::pasteboardWithName(&NSString::from_str(name));
        let change = pasteboard.changeCount();
        // SAFETY: This exported immutable system constant is the same string
        // pasteboard type used by the actual production publisher.
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
        // SAFETY: The system returned complete NSData, retained throughout this
        // scope. No message mutates it while bytes are borrowed.
        let bytes = unsafe { data.as_bytes_unchecked() };
        for (index, chunk) in bytes.chunks(SCRATCH_BYTES).enumerate() {
            for (local, byte) in chunk.iter().copied().enumerate() {
                let offset = index * SCRATCH_BYTES + local;
                if byte != case.expected_byte(offset) {
                    return Err(format!("剪贴板原字节在{offset}处不符")); // Reject the first mismatch without logging any payload.
                }
            }
        }
        if matches!(case, CopyCase::Empty | CopyCase::Unicode) {
            // Only finite Unicode fixtures request stringForType. The 32-MiB
            // scenario never constructs a whole-output NSString mirror.
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
            return Err("独立读取期间私有剪贴板发生了变化".into()); // Never combine lengths and bytes from different publications.
        }
        println!(
            "native_clipboard role=reader stage=exact case={} bytes={} whole_product=false formal=false",
            case.name(),
            bytes.len(),
        );
        Ok(())
    })
}
