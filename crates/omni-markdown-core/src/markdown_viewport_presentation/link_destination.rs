//! Owned normalized link destinations. Contiguous values and bounded pages are
//! actual producer output, never a cache of document text.

use std::sync::Arc;

#[derive(Clone)]
/// Shares the parser's already-owned string or takes bounded pages from its
/// streaming decoder. Both paths preserve validated UTF-8;
/// clones share original payloads. CPU publication needs no contiguous form,
/// and only the actual URL I/O boundary materializes a string.
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
    /// Moves a small parser's valid string into shared ownership without copying
    /// bytes or constructing another paged representation.
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn from_string(value: String) -> Self {
        Self::from_shared_string(Arc::new(value))
    }

    /// Reference maps and source links share one normalized output. The string
    /// already proves UTF-8 validity; this entry does not accept raw bytes.
    /// Sharing a value grants no source/publication authority. Actual activation
    /// still validates the original publication receipt and current click intent.
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn from_shared_string(value: Arc<String>) -> Self {
        Self(Storage::Whole(value))
    }

    /// Only crate-local producers that validated every decoded chunk may transfer
    /// their page directory here. Destination bytes are not copied.
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn from_chunks(chunks: Vec<Vec<u8>>) -> Self {
        let len = chunks.iter().map(Vec::len).sum();
        Self(Storage::Paged(Arc::new(Pages { chunks, len })))
    }

    /// Admission uses normalized byte length, never page capacity or the original
    /// URL's lexical spelling as an estimate.
    pub fn byte_len(&self) -> usize {
        match &self.0 {
            Storage::Whole(value) => value.len(),
            Storage::Paged(value) => value.len,
        }
    }

    /// Only the actual URL handoff requests contiguous bytes. This reads no
    /// document/source and invokes no native callback.
    /// Exclusive strings/single pages transfer their existing allocation. Shared
    /// or multiple pages materialize only at this I/O boundary, without caching.
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

    /// Measures retained capacity of the actual representation. A shallow `Arc`
    /// clone creates no second string or page directory.
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

    /// Equality streams existing bytes only. Its iterator owns no contiguous
    /// string copy or permission to read document source.
    fn bytes(&self) -> impl Iterator<Item = &u8> {
        let (whole, chunks): (&[u8], &[Vec<u8>]) = match &self.0 {
            Storage::Whole(value) => (value.as_bytes(), &[]),
            Storage::Paged(value) => (&[], &value.chunks),
        };
        whole.iter().chain(chunks.iter().flatten())
    }
}

impl std::fmt::Debug for MarkdownLinkDestination {
    /// Diagnostics report sizes only, never private URLs, titles, or query strings.
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
    /// Storage representation is not a semantic difference. Shared `Arc` identity
    /// is a fast equality case; other destinations compare actual bytes.
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
