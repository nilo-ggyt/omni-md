//! 严格关闭已失败时的一次性弱角色证据；不持图、正文或像素，也不改变关闭结论。

use super::*;
use crate::native_surface::raster_surface::{
    NativeMarkdownRasterReleaseReceipt, NativeMarkdownRasterSurface,
};

pub(super) struct RasterCloseObservation {
    ledger: NativeMarkdownOwnedSnapshot,
    raster: Option<NativeMarkdownRasterReleaseReceipt>,
}

impl NativeMarkdownMemoryWorkload {
    pub(super) fn capture_raster_close_observation(&self) -> Result<(), String> {
        if !NativeMarkdownRasterSurface::requested() {
            return Ok(());
        }
        let observation = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("关闭诊断缺少原 editor")?;
            Ok(RasterCloseObservation {
                ledger: editor.backend.owned_ledger.snapshot(),
                raster: editor.backend.raster_release.clone(),
            })
        })?;
        let previous = self
            .raster_close_observation
            .borrow_mut()
            .replace(observation);
        drop(previous); // 旧 Weak 控制块在字段借用外归还，不延长任何 Native 或正文生命周期。
        Ok(())
    }

    pub(super) fn clear_raster_close_observation(&self) {
        let observation = self.raster_close_observation.borrow_mut().take();
        drop(observation);
    }

    /// 仅原 failure-only deadline 调用；take 之后不能重复采样或把 survivor 变成未来通过。
    pub(crate) fn diagnose_raster_close_timeout(&self) {
        let observation = self.raster_close_observation.borrow_mut().take();
        if !NativeMarkdownRasterSurface::requested() {
            drop(observation);
            return;
        }
        let Some(observation) = observation else {
            return;
        };
        let (complete, roles, raster, strings) = autoreleasepool(|_| {
            // 已有 weak role API 每次临时升级在本池内结束；计数不依赖字符串、路径或布局 getter。
            let (complete, roles) = observation.ledger.diagnostic_alive_roles();
            let strings = observation
                .ledger
                .diagnostic_raster_surviving_strings(observation.raster.as_ref());
            let raster = observation
                .raster
                .as_ref()
                .map(NativeMarkdownRasterReleaseReceipt::snapshot);
            drop(observation);
            (complete, roles, raster, strings)
        });
        // 池已排空，输出只剩创建时的固定角色名及标量，观察本身不会保活被测对象。
        eprintln!(
            "native_raster_close event=deadline_observation ledger_complete={complete} role_count={} raster_present={} formal=false",
            roles.len(),
            raster.is_some()
        );
        if let Some(raster) = raster {
            eprintln!("native_raster_close event=resources snapshot={raster:?} formal=false");
        }
        for (role, total, alive) in roles {
            eprintln!(
                "native_raster_close event=role role={role} total={total} alive={alive} formal=false"
            );
        }
        for string in strings {
            let NativeMarkdownSurvivingRasterString {
                class,
                length,
                live_adapter,
                origins,
            } = string;
            eprintln!(
                "native_raster_close event=surviving_line_string class={class} utf16_length={length:?} live_adapter={live_adapter} origins={origins:?} formal=false"
            ); // 只有类元数据和长度；不输出笔记内容，也不改变严格关闭的既有失败结果。
        }
    }
}
