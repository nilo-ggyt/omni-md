//! 兼容原独立harness入口；合成内容由probe-only库中的同一流式生成器维护。

use std::io;
use std::path::Path;

/// 仅转发到共享create_new生成器，不保留另一份夹具正文或扩大原路径授权。
pub(super) fn write_fixture(path: &Path) -> io::Result<()> {
    omni_md_plugin::write_native_markdown_memory_fixture(path)
}
