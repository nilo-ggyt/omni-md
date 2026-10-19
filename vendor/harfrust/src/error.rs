use crate::{Direction, Script};

/// The reason a call to [`shape`](crate::shape) could not produce glyphs.
///
/// 参数错误与协作撤权都明确返回；`Cancelled`、Source或分页错误后必须丢弃整个候选Buffer，
/// 不能把已处理前缀发布成字形。原上游操作/容量预算仍独立通过
/// [`Buffer::allocation_successful`](crate::Buffer::allocation_successful) 核验。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum ShapeError {
    /// 同一原调用收到协作停止请求；没有完整字形结果。
    Cancelled,
    /// 原不可变Source的永久健康错误，优先于取消。
    SourceUnavailable(i32),
    /// 原分页scope的首次真实IO错误，不能被取消抹去。
    Paging {
        /// 原PagingOperation的稳定数值。
        operation: u32,
        /// 同一个实际IO失败的系统errno。
        errno: i32,
    },
    /// The buffer already holds the glyphs from an earlier call.
    ///
    /// To shape the same contents again, set the content type back to
    /// [`ContentType::Unicode`](crate::ContentType::Unicode) with
    /// [`set_content_type`](crate::Buffer::set_content_type); to shape something else,
    /// clear the buffer and fill it afresh.
    AlreadyShaped,

    /// The buffer has no direction, so no plan could be built for it.
    ///
    /// Call [`guess_segment_properties`](crate::Buffer::guess_segment_properties) to
    /// infer one from the contents, or set it with
    /// [`set_direction`](crate::Buffer::set_direction).
    DirectionUnset,

    /// The buffer's direction is not the one the supplied plan was built for.
    DirectionMismatch {
        /// The direction the plan was built for.
        plan: Direction,
        /// The direction the buffer carries.
        buffer: Direction,
    },

    /// The buffer's script is not the one the supplied plan was built for.
    ScriptMismatch {
        /// The script the plan was built for.
        plan: Script,
        /// The script the buffer carries.
        buffer: Script,
    },
}

impl core::fmt::Display for ShapeError {
    fn fmt(&self, fmt: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Cancelled => fmt.write_str("原字形任务已取消"),
            Self::SourceUnavailable(errno) => write!(fmt, "原字形Source已撤权（errno={errno}）"),
            Self::Paging { operation, errno } => {
                write!(fmt, "原字形分页操作{operation}失败（errno={errno}）")
            }
            Self::AlreadyShaped => fmt.write_str("buffer already holds shaped glyphs"),
            Self::DirectionUnset => fmt.write_str("buffer has no direction set"),
            Self::DirectionMismatch { plan, buffer } => write!(
                fmt,
                "buffer direction does not match plan direction: {buffer:?} != {plan:?}"
            ),
            Self::ScriptMismatch { plan, buffer } => write!(
                fmt,
                "buffer script does not match plan script: {buffer:?} != {plan:?}"
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ShapeError {}
