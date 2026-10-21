//! Keep worker-start feedback in Chinese. Diagnostics contain only bounded OS
//! error categories and numeric codes, without arbitrary error text or payload.

pub(super) enum NativeMarkdownWorker {
    Search,
    Directory,
}

/// Avoid io::Error's Display so system English or attached arbitrary strings
/// cannot enter status/tooltip feedback.
pub(super) fn native_markdown_worker_start_error(
    operation: NativeMarkdownWorker,
    error: &std::io::Error,
) -> String {
    let (label, message) = match operation {
        NativeMarkdownWorker::Search => {
            // Preserve the search field and body so the original query may be submitted again.
            ("search", "无法开始搜索笔记，请稍后重新搜索")
        }
        NativeMarkdownWorker::Directory => {
            // Directory failure changes no document or file; explicit refresh may start a new scan.
            ("directory", "无法开始读取笔记目录，请稍后刷新")
        }
    };
    // Record one bounded diagnostic per failed spawn, without Error's arbitrary
    // text, paths, or document content.
    eprintln!(
        "omni_md_worker_start_failure operation={label} kind={:?} raw_os_error={:?}",
        error.kind(),
        error.raw_os_error()
    );
    message.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Neither arbitrary English payloads nor localized OS errors may enter visible feedback.
    fn visible_worker_failure_never_includes_os_display_or_custom_payload() {
        for error in [
            std::io::Error::from_raw_os_error(35),
            std::io::Error::other("English system failure /private/sensitive"),
        ] {
            let search = native_markdown_worker_start_error(NativeMarkdownWorker::Search, &error);
            let directory =
                native_markdown_worker_start_error(NativeMarkdownWorker::Directory, &error);
            assert!(!search.contains(&error.to_string()));
            assert!(!directory.contains(&error.to_string()));
        }
    }
}
