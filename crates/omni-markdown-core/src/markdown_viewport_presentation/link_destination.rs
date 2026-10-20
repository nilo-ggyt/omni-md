//! 已规范化链接目标的唯一拥有值；连续值与小页都是实际producer的载荷，不是正文缓存。

use std::sync::Arc;

#[derive(Clone)]
/// 原Parser已经拥有的String直接共享；流式解码器移交有限小页。两条路径均保持有效UTF8，
/// clone只分享原载荷，CPU发布不要求连续表示；真正URL I/O边界才显式物化String。
pub struct MarkdownLinkDestination(Storage);

#[derive(Clone)]
enum Storage {
    Whole(Arc<String>),
    Paged(Arc<Pages>),
}
struct Pages {
    chunks: Vec<Vec<u8>>,
    len: usize,
}

impl MarkdownLinkDestination {
    /// 原小块Parser已生成合法String；只move到一个小Arc，不再复制其字节或换成第二份页。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn from_string(value: String) -> Self {
        Self::from_shared_string(Arc::new(value))
    }

    /// 原引用Map与SourceLink共享同一次规范化输出；String本身已证明UTF8，不接受裸字节。
    /// 该入口只分享值，不包含Source/pub权限；真正激活仍由原发布票与点击意图核验。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn from_shared_string(value: Arc<String>) -> Self {
        Self(Storage::Whole(value))
    }

    /// 只允许同crate已逐片验证解码输出的拥有者调用；移交目录而不复制目标内容。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn from_chunks(chunks: Vec<Vec<u8>>) -> Self {
        let len = chunks.iter().map(Vec::len).sum();
        Self(Storage::Paged(Arc::new(Pages { chunks, len })))
    }

    /// 库存准入使用规范化字节数，不从片容量或原URL词法拼写猜长度。
    pub fn byte_len(&self) -> usize {
        match &self.0 {
            Storage::Whole(value) => value.len(),
            Storage::Paged(value) => value.len,
        }
    }

    /// 只有实际URL出口取得连续目标；不读取Doc/Source或触发Native回调。
    /// 独占Whole/单页可直接移交原分配，共享/多页仅在该I/O边界物化且不缓存结果。
    pub fn into_string(self) -> String {
        let pages = match self.0 {
            Storage::Whole(value) => {
                return Arc::try_unwrap(value).unwrap_or_else(|value| value.as_ref().clone());
            }
            Storage::Paged(value) => value,
        };
        let bytes = match Arc::try_unwrap(pages) {
            Ok(mut pages) if pages.chunks.len() == 1 => {
                return String::from_utf8(pages.chunks.pop().unwrap())
                    .expect("私有目标构造器只接已验证UTF8解码流");
            }
            Ok(pages) => {
                let mut bytes = Vec::with_capacity(pages.len);
                for chunk in pages.chunks {
                    bytes.extend_from_slice(&chunk);
                }
                bytes
            }
            Err(pages) => {
                let mut bytes = Vec::with_capacity(pages.len);
                for chunk in &pages.chunks {
                    bytes.extend_from_slice(chunk);
                }
                bytes
            }
        };
        String::from_utf8(bytes).expect("私有目标构造器只接已验证UTF8解码流")
    }

    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn whole_metadata() -> usize {
        std::mem::size_of::<String>() + 2 * std::mem::size_of::<usize>()
    }

    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn paged_metadata() -> usize {
        std::mem::size_of::<Pages>() + 2 * std::mem::size_of::<usize>()
    }

    /// 按真实表示保有容量计量；Arc浅clone不会制造另一份String或目录。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn retained_metadata(&self) -> usize {
        match &self.0 {
            Storage::Whole(value) => Self::whole_metadata() + value.capacity(),
            Storage::Paged(value) => {
                Self::paged_metadata()
                    + value.chunks.capacity() * std::mem::size_of::<Vec<u8>>()
                    + value.chunks.iter().map(Vec::capacity).sum::<usize>()
            }
        }
    }

    /// 相等比较只顺序借现有字节；内部迭代器没有连续String副本或任何源读取权限。
    fn bytes(&self) -> impl Iterator<Item = &u8> {
        let (whole, chunks): (&[u8], &[Vec<u8>]) = match &self.0 {
            Storage::Whole(value) => (value.as_bytes(), &[]),
            Storage::Paged(value) => (&[], &value.chunks),
        };
        whole.iter().chain(chunks.iter().flatten())
    }
}

impl std::fmt::Debug for MarkdownLinkDestination {
    /// 诊断只报告尺度，绝不输出私人URL、title或查询串。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MarkdownLinkDestination")
            .field("len", &self.byte_len())
            .field(
                "chunks",
                &match &self.0 {
                    Storage::Whole(_) => 1,
                    Storage::Paged(value) => value.chunks.len(),
                },
            )
            .finish()
    }
}
impl PartialEq for MarkdownLinkDestination {
    /// 真实目标的表示不同不构成业务差异；同一Arc先快速相等，其余逐字节比较。
    fn eq(&self, other: &Self) -> bool {
        let shared = match (&self.0, &other.0) {
            (Storage::Whole(left), Storage::Whole(right)) => Arc::ptr_eq(left, right),
            (Storage::Paged(left), Storage::Paged(right)) => Arc::ptr_eq(left, right),
            _ => false,
        };
        shared || self.byte_len() == other.byte_len() && self.bytes().eq(other.bytes())
    }
}
impl Eq for MarkdownLinkDestination {}
