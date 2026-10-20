//! 系统有序字体候选的有限owner；不接收正文，不执行缺字裁决或整形。
//!
//! CoreText只保证descriptor的顺序和best-match构造。本层每个实际CTFont仍经过原
//! PagedFont的文件、face、TTC与轴位核验；候选存在不等于某个字素已经可以正确显示。
use std::{
    cell::Cell,
    rc::{Rc, Weak},
};

use objc2_core_foundation::{CFArray, CFRetained, CFString, CFType, Type};
use objc2_core_text::{CTFont, CTFontDescriptor, CTFontOptions};
use objc2_foundation::MainThreadMarker;
use omni_markdown_core::MarkdownLayoutFontAuthority;
use thiserror::Error;

use super::paged_fonts::{
    NativeMarkdownFontCapture, NativeMarkdownPagedFont, NativeMarkdownPagedFontError,
};

/// 系统列表超过此硬上限时明确拒绝，不静默删掉列表尾部的回退字体。
pub const NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES: usize = 256;
const MAX_LANGUAGES: usize = 8;
const MAX_LANGUAGE_BYTES: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownFontCascadeBudget {
    pub maximum_candidates: usize,
}
impl Default for NativeMarkdownFontCascadeBudget {
    fn default() -> Self {
        Self {
            maximum_candidates: NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES,
        }
    }
}

#[derive(Debug, Error)]
pub enum NativeMarkdownFontCascadeError {
    #[error("字体候选配额无效")]
    InvalidBudget,
    #[error("字体语言偏好不是有限有效语言标识符列表")]
    InvalidLanguages,
    #[error("原字体payload权限不匹配")]
    FontAuthorityMismatch,
    #[error("原字体候选权限已撤销")]
    Revoked,
    #[error("系统字体候选列表不可用")]
    Unavailable,
    #[error("系统字体候选数{actual}超过明确配额{maximum}")]
    CandidateBudgetExceeded { actual: usize, maximum: usize },
    #[error("系统候选不是真实字体描述符")]
    InvalidDescriptor,
    #[error("无法捕获候选字体的精确来源")]
    Font(#[from] NativeMarkdownPagedFontError),
}
type Result<T> = std::result::Result<T, NativeMarkdownFontCascadeError>;

/// 只含弱Core身份与永久撤权标量；不保活base、字体字节、视图、文档或调用方闭包。
struct CascadePermit {
    original: MarkdownLayoutFontAuthority,
    revoked: Cell<bool>,
}
impl CascadePermit {
    fn check(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        if self.revoked.get() {
            return Err(NativeMarkdownFontCascadeError::Revoked);
        }
        if !base.core_font().permits(&self.original) || !base.core_font().permits(authority) {
            self.revoked.set(true);
            return Err(NativeMarkdownFontCascadeError::FontAuthorityMismatch);
        } // 同名字、同CTFont或同轴的新capture仍没有原Core allocation权限。
        if !current() {
            self.revoked.set(true);
            return Err(NativeMarkdownFontCascadeError::Revoked);
        }
        Ok(())
    }

    fn native<T>(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
        operation: impl FnOnce() -> T,
    ) -> Result<T> {
        self.check(base, authority, current)?;
        let value = operation();
        if let Err(error) = self.check(base, authority, current) {
            drop(value); // 原语已经返回的+1临时值在失权出口真实归还，不能发布给后继票。
            return Err(error);
        }
        Ok(value)
    }
}

/// 会话局部的原有序CFArray；只保元数据，不创建或缓存每个候选的字体文件映射。
///
/// MainThreadMarker保持与原PagedFont相同的Native线程边界；析构只归还本owner的CF引用，
/// 不把框架共享descriptor/font实际销毁伪装为已证明。正文和缺字处理属于后续composer。
pub struct NativeMarkdownFontCascade {
    descriptors: CFRetained<CFArray>,
    permit: Rc<CascadePermit>,
    count: usize,
    next: usize,
    point_size: f64,
    mtm: MainThreadMarker,
}
impl NativeMarkdownFontCascade {
    /// 捕获系统真实顺序；languages只来自固定配置，不能用笔记内容探测字体。
    ///
    /// # Errors
    /// 配额、语言、原payload权限、系统Copy失败或列表超预算均明确拒绝；不返回截断列表。
    pub fn capture(
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        languages: &[&'static str],
        budget: NativeMarkdownFontCascadeBudget,
        mtm: MainThreadMarker,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Self> {
        if !(1..=NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES).contains(&budget.maximum_candidates) {
            return Err(NativeMarkdownFontCascadeError::InvalidBudget);
        }
        if languages.len() > MAX_LANGUAGES
            || languages.iter().any(|language| {
                language.is_empty()
                    || language.len() > MAX_LANGUAGE_BYTES
                    || !language
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
        {
            return Err(NativeMarkdownFontCascadeError::InvalidLanguages);
        } // 仅校验有限语言tag形状，不把任意UTF8正文交给系统字体查询。
        let permit = Rc::new(CascadePermit {
            original: authority.clone(),
            revoked: Cell::new(false),
        });
        permit.check(base, authority, current)?; // 任何CF构造前先核同一个实际Core payload。
        let mut values = Vec::with_capacity(languages.len());
        for language in languages {
            values.push(permit.native(base, authority, current, || CFString::from_str(language))?);
        } // Vec仅有最多8个固定语言值，不按文档或系统字体总量扩张。
        let language_array = permit.native(base, authority, current, || {
            CFArray::<CFString>::from_retained_objects(&values)
        })?;
        // SAFETY: 原base与有限CFString数组贯穿公开Copy调用；返回列表自身拥有全部descriptor。
        let descriptors = permit
            .native(base, authority, current, || unsafe {
                base.native_font()
                    .default_cascade_list_for_languages(Some(language_array.as_opaque()))
            })?
            .ok_or(NativeMarkdownFontCascadeError::Unavailable)?;
        let count = permit.native(base, authority, current, || descriptors.len())?;
        if count > budget.maximum_candidates {
            return Err(NativeMarkdownFontCascadeError::CandidateBudgetExceeded {
                actual: count,
                maximum: budget.maximum_candidates,
            });
        } // 原完整list在失败出口归还；没有用预算后的前缀冒充系统完整候选。
        permit.native(base, authority, current, || drop(language_array))?;
        for value in values {
            permit.native(base, authority, current, || drop(value))?;
        } // 临时语言claim归还后再次核票，列表才能进入外部可见owner。
        permit.check(base, authority, current)?;
        Ok(Self {
            descriptors,
            permit,
            count,
            next: 0,
            point_size: base.point_size(),
            mtm,
        })
    }

    /// 返回实际系统列表总数，包含尚未构造字体的候选；不是已经支持的字素数。
    #[must_use]
    pub fn candidate_count(&self) -> usize {
        self.count
    }

    /// 按原列表逐项交付；不排序、不按同名去重、不扫描字符，也不隐式重试失败字体。
    ///
    /// # Errors
    /// 任意失权永久撤销本游标；不合格CF类型明确失败，不能跳过后伪称完整候选遍历。
    pub fn next(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownFontCandidate>> {
        self.permit.check(base, authority, current)?;
        if self.next == self.count {
            return Ok(None); // 仅在实际count全部交付后结束；空系统列表也不表示字体功能Ready。
        }
        let ordinal = self.next;
        // SAFETY: 原不可变list保活，ordinal<count≤256；CoreText公开约定每个值是CFType descriptor。
        let raw = self.permit.native(base, authority, current, || unsafe {
            self.descriptors.value_at_index(ordinal as _)
        })?;
        if raw.is_null() {
            self.permit.revoked.set(true);
            return Err(NativeMarkdownFontCascadeError::InvalidDescriptor);
        }
        // SAFETY: 刚取得的非空CFType由原list保活；先动态核类型，再取得自己的+1 claim。
        let value = unsafe { &*raw.cast::<CFType>() };
        let descriptor = self.permit.native(base, authority, current, || {
            value.downcast_ref::<CTFontDescriptor>()
        })?;
        let Some(descriptor) = descriptor else {
            self.permit.revoked.set(true);
            return Err(NativeMarkdownFontCascadeError::InvalidDescriptor);
        };
        let descriptor = self
            .permit
            .native(base, authority, current, || descriptor.retain())?;
        self.next += 1; // 只有原类型与retain都按同票完成，才消费这个固定系统ordinal。
        Ok(Some(NativeMarkdownFontCandidate {
            descriptor,
            permit: Rc::downgrade(&self.permit),
            ordinal,
            point_size: self.point_size,
            mtm: self.mtm,
        }))
    }

    /// 停止签发候选并归还原CFArray；已有不可变描述符不被清空，旧候选不能再构造字体。
    pub fn close(self) {
        drop(self);
    }
}
impl Drop for NativeMarkdownFontCascade {
    fn drop(&mut self) {
        self.permit.revoked.set(true); // 先撤所有Weak候选，再由字段顺序交还原list与唯一permit。
    }
}

/// 原列表一个真实descriptor及Weak签发身份；不保活列表或原base Font。
pub struct NativeMarkdownFontCandidate {
    descriptor: CFRetained<CTFontDescriptor>,
    permit: Weak<CascadePermit>,
    ordinal: usize,
    point_size: f64,
    mtm: MainThreadMarker,
}
impl NativeMarkdownFontCandidate {
    /// 原系统数组下标；不是字体名、glyph ID或脚本范围。
    #[must_use]
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }
    /// 只借出原不可变描述符供有限元数据观察；不授予新的capture、正文或绘制权限。
    /// 级联关闭后该值仍保持不可变，不能为释放验收强制改成空对象。
    #[must_use]
    pub fn descriptor(&self) -> &CTFontDescriptor {
        &self.descriptor
    }

    /// 按需构造一个实际best-match字体，再复用原精确Native/Core来源捕获。
    ///
    /// # Errors
    /// 级联结束、原Font失权、Native捕获失败均显式返回；本调用不猜缺字、不改字素边界。
    pub fn capture(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        maximum_font_bytes: u64,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedFont> {
        let (permit, native) = self.construct(base, authority, current)?;
        let valid = || permit.check(base, authority, current).is_ok();
        let captured =
            NativeMarkdownPagedFont::capture(&native, maximum_font_bytes, self.mtm, &valid);
        permit.native(base, authority, current, || drop(native))?;
        permit.check(base, authority, current)?;
        captured.map_err(Into::into) // 候选失败不会自动拿后一个字体掩盖；下一步由composer显式决定。
    }
    /// 活动 Body 只复用这个实际候选的既有 capture；级联 ordinal 与原单 GCB 试次顺序不变。
    pub(crate) fn capture_reusing(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        palette: &[Rc<super::paged_line_fonts::NativeMarkdownLineFont>],
        maximum_font_bytes: u64,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontCapture> {
        let (permit, native) = self.construct(base, authority, current)?;
        let valid = || permit.check(base, authority, current).is_ok();
        let captured = NativeMarkdownPagedFont::capture_reusing(
            &native,
            palette,
            maximum_font_bytes,
            self.mtm,
            &valid,
        );
        permit.native(base, authority, current, || drop(native))?;
        permit.check(base, authority, current)?;
        captured.map_err(Into::into)
    }

    /// 独立捕获与活动 palette 复用共用实际 best-match 构造；仅原弱许可仍活时可以进入 Native。
    fn construct(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<(Rc<CascadePermit>, CFRetained<CTFont>)> {
        let permit = self
            .permit
            .upgrade()
            .ok_or(NativeMarkdownFontCascadeError::Revoked)?;
        permit.check(base, authority, current)?;
        // SAFETY: 原descriptor强claim与有限正字号来自原base；None矩阵没有外部指针。
        // 禁止自动激活/下载只是候选来源约束，实际best-match仍交原capture完整核验。
        let native = permit.native(base, authority, current, || unsafe {
            CTFont::with_font_descriptor_and_options(
                &self.descriptor,
                self.point_size,
                std::ptr::null(),
                CTFontOptions::PreventAutoActivation | CTFontOptions::PreventAutoDownload,
            )
        })?;
        Ok((permit, native))
    }
}
