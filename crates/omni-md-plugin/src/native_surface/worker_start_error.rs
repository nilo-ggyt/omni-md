//! worker启动失败的产品文案固定中文；只把有限OS类别和数值送到诊断。

pub(super) enum NativeMarkdownWorker {
    Search,
    Directory,
}

/// 不调用io::Error的Display，避免系统英文或任意附加字符串进入status/tooltip。
pub(super) fn native_markdown_worker_start_error(
    operation: NativeMarkdownWorker,
    error: &std::io::Error,
) -> String {
    let (label, message) = match operation {
        NativeMarkdownWorker::Search => {
            // 搜索字段和正文仍保留，用户可再次提交原查询。
            ("search", "无法开始搜索笔记，请稍后重新搜索")
        }
        NativeMarkdownWorker::Directory => {
            // 目录失败不改正文或文件，显式刷新可建立新的扫描任务。
            ("directory", "无法开始读取笔记目录，请稍后刷新")
        }
    };
    // 每次spawn失败只有一条有限诊断；不打印Error的任意文本、路径或正文。
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
    /// 任意英文Error载荷和OS本地化文本都不能拼入产品文案。
    fn visible_worker_failure_never_includes_os_display_or_custom_payload() {
        for error in [
            std::io::Error::from_raw_os_error(35),
            std::io::Error::other("English system failure /private/sensitive"),
        ] {
            let search = native_markdown_worker_start_error(NativeMarkdownWorker::Search, &error);
            let directory =
                native_markdown_worker_start_error(NativeMarkdownWorker::Directory, &error);
            assert_eq!(search, "无法开始搜索笔记，请稍后重新搜索");
            assert_eq!(directory, "无法开始读取笔记目录，请稍后刷新");
            assert!(!search.contains(&error.to_string()));
            assert!(!directory.contains(&error.to_string()));
        }
    }
}
