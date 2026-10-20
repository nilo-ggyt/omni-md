//! 已规范化链接目标的唯一拥有值；分页只是有限URI载荷，不是正文索引或缓存。

use std::sync::Arc;

#[derive(Clone)]
/// 目标值由真实Parser或流式Link解码器构造，整个值始终是有效UTF8；clone只分享不可变目标。
/// CPU发布不要求连续String。只有实际点击/NSURL边界才显式物化，不在排版量子内搬移完整目标。
pub struct MarkdownLinkDestination(Arc<Payload>);

struct Payload {
    chunks: Vec<Vec<u8>>,
    len: usize,
}

impl MarkdownLinkDestination {
    /// 原有小块Parser已经生成合法String；move其分配为一个目标片，不重新解码或复制URL。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn from_string(value: String) -> Self {
        Self::from_chunks(vec![value.into_bytes()])
    }

    /// 只允许同crate已验证每个规范化输出片的拥有者调用；拼接顺序保持原解码流的UTF8证明。
    /// 此处只移动有限目录与小Arc载荷，不复制片内容；不公开裸bytes构造以免外部伪造该证明。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn from_chunks(chunks: Vec<Vec<u8>>) -> Self {
        let len = chunks.iter().map(Vec::len).sum();
        Self(Arc::new(Payload { chunks, len }))
    }

    /// 实际目标库存准入使用已规范化字节数，不从chunk容量或原URL拼写猜长度。
    pub fn byte_len(&self) -> usize {
        self.0.len
    }

    /// 点击出口取得独立连续目标；只处理原有限URI，不读取Doc/Source或触发Native回调。
    /// 唯一单片直接移交原分配；共享/多片则在I/O边界一次物化，不保另一份目标缓存。
    pub fn into_string(self) -> String {
        let bytes = match Arc::try_unwrap(self.0) {
            Ok(mut payload) if payload.chunks.len() == 1 => {
                return String::from_utf8(payload.chunks.pop().unwrap())
                    .expect("私有目标构造器只接原Parser或已验证UTF8解码流");
            }
            Ok(payload) => {
                let mut bytes = Vec::with_capacity(payload.len);
                for chunk in payload.chunks {
                    bytes.extend_from_slice(&chunk);
                }
                bytes
            }
            Err(payload) => {
                let mut bytes = Vec::with_capacity(payload.len);
                for chunk in &payload.chunks {
                    bytes.extend_from_slice(chunk);
                }
                bytes
            }
        };
        String::from_utf8(bytes).expect("私有目标构造器只接原Parser或已验证UTF8解码流")
    }

    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn publication_metadata() -> usize {
        std::mem::size_of::<Payload>() + 2 * std::mem::size_of::<usize>()
    }

    /// 原publication按真实保有容量计费，包含目录、各片与Arc；外带clone不复制这些分配。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn retained_metadata(&self) -> usize {
        Self::publication_metadata()
            + self.0.chunks.capacity() * std::mem::size_of::<Vec<u8>>()
            + self.0.chunks.iter().map(Vec::capacity).sum::<usize>()
    }
}

impl std::fmt::Debug for MarkdownLinkDestination {
    /// 默认诊断只暴露有限载荷尺度，绝不将私人URL、查询串或原字节写入日志。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MarkdownLinkDestination")
            .field("len", &self.0.len)
            .field("chunks", &self.0.chunks.len())
            .finish()
    }
}

impl PartialEq for MarkdownLinkDestination {
    /// 同一个URI的片边界不是业务差异；比较原不可变字节序，不物化String或读取正文。
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || self.0.len == other.0.len
                && self
                    .0
                    .chunks
                    .iter()
                    .flatten()
                    .eq(other.0.chunks.iter().flatten())
    }
}
impl Eq for MarkdownLinkDestination {}
