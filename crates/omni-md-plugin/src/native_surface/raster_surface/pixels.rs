//! 与分页正文复用同一真实mmap/context/provider所有权实现，不保留第二份unsafe释放逻辑。

pub(in crate::native_surface) use super::super::viewport_pixels::Bitmap;
