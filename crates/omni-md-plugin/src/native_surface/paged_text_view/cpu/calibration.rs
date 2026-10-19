//! 原候选门下的固定五层校准；只把精确颜色事实带回纯像素编码，不建立Native/Source缓存。

use super::*;
use crate::native_surface::viewport_pixels::{
    CertifiedMaskPairs, MaskCalibrationPixels, MaskCalibrationTable, MaskPairBlend,
    MaskPaletteRequest, PixelError,
};
use objc2::rc::{Weak as NativeWeak, autoreleasepool};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGColor;
use objc2_foundation::NSSize;

const MAX_ENDPOINTS: usize = 2;

struct Entry {
    request: MaskPaletteRequest,
    tables: [Option<MaskCalibrationTable>; 2],
    attempted: [bool; 2],
}

/// 固定最多两组端点、每组两个模式；全部纯表仍计原32MiB页账，原候选结束就归还。
#[derive(Default)]
pub(super) struct Calibrations {
    entries: Vec<Entry>,
}
impl Calibrations {
    pub(super) fn pairs(
        &mut self,
        request: &MaskPaletteRequest,
        pixels: Arc<NativeMarkdownViewportPixels>,
        validate: &impl Fn() -> Result<()>,
    ) -> Result<Option<CertifiedMaskPairs>> {
        validate()?;
        let endpoint = match self
            .entries
            .iter()
            .position(|entry| entry.request.same_endpoints(request))
        {
            Some(index) => index,
            None if self.entries.len() == MAX_ENDPOINTS => return Ok(None),
            None => {
                if self.entries.capacity() == 0 {
                    self.entries.reserve_exact(MAX_ENDPOINTS);
                }
                self.entries.push(Entry {
                    request: request.clone(),
                    tables: [None, None],
                    attempted: [false; 2],
                });
                self.entries.len() - 1
            }
        }; // 模式不是新的端点组；第二组实际颜色仍保留自己的两个固定槽。
        for (mode, blend) in [
            MaskPairBlend::ForegroundThenForeground,
            MaskPairBlend::ForegroundThenBackground,
        ]
        .into_iter()
        .enumerate()
        {
            let captured = if !self.entries[endpoint].attempted[mode] {
                let table = capture_checked(request, blend, Arc::clone(&pixels), validate)?;
                validate()?;
                self.entries[endpoint].tables[mode] = table;
                self.entries[endpoint].attempted[mode] = true;
                true
            } else {
                false // 原模式已尝试过，包括预算miss；同候选不会因后续tile反复创建校准图。
            };
            let table = &self.entries[endpoint].tables[mode];
            let pairs = table
                .as_ref()
                .and_then(|table| table.certify(request, blend));
            #[cfg(feature = "native-markdown-runtime-probe")]
            if captured {
                eprintln!(
                    "paged_pair_calibration actual_background={:?} actual_foreground={:?} blend={blend:?} original_palette_colors={} actual_native_roles={} actual_native_nil=true actual_table_ready={} actual_palette_covered={} actual_source_maps_returned={} whole_product=false formal=false",
                    request.background,
                    request.foreground,
                    request.color_count(),
                    if table.is_some() { 5 } else { 0 },
                    table.is_some(),
                    pairs.is_some(),
                    table.is_some(),
                );
            }
            #[cfg(not(feature = "native-markdown-runtime-probe"))]
            let _ = captured;
            validate()?;
            if pairs.is_some() {
                return Ok(pairs); // 先保旧同前景模式；未全色匹配才允许原背景回混的独立实际表。
            }
        }
        Ok(None) // 两种固定模式都不全覆盖，原Indexed/RGBA照常保全部颜色和功能。
    }
}

fn capture_checked(
    request: &MaskPaletteRequest,
    blend: MaskPairBlend,
    pixels: Arc<NativeMarkdownViewportPixels>,
    validate: &impl Fn() -> Result<()>,
) -> Result<Option<MaskCalibrationTable>> {
    let (result, weaks) = autoreleasepool(|_| {
        let mut weaks = Vec::with_capacity(5);
        let result = capture(request, blend, pixels, validate, &mut weaks);
        (result, weaks)
    });
    CATransaction::flush(); // 当次五层及provider先实际归还，再允许下一个模式创建新Native图。
    let returned = weaks.iter().all(|weak| weak.load().is_none());
    validate()?; // 原Source/Font失败始终先于Native清理或优化错误。
    let table = result?;
    if !returned {
        return Err(NativeMarkdownPagedTextViewError::Raster(
            "双遮罩校准的原生图未实际释放".into(),
        ));
    }
    table
        .map(MaskCalibrationTable::confirm_sources_returned)
        .transpose()
        .map_err(Into::into)
}

fn capture(
    request: &MaskPaletteRequest,
    blend: MaskPairBlend,
    counters: Arc<NativeMarkdownViewportPixels>,
    validate: &impl Fn() -> Result<()>,
    weaks: &mut Vec<NativeWeak<AnyObject>>,
) -> Result<Option<MaskCalibrationTable>> {
    validate()?;
    let observer = counters.observer();
    let pixels = MaskCalibrationPixels::new(counters);
    validate()?;
    if observer.snapshot().release_failed {
        return Err(NativeMarkdownPagedTextViewError::Raster(
            "校准分配失败后原像素页未归还".into(),
        ));
    }
    let pixels = match pixels {
        Ok(value) => value,
        Err(PixelError::BudgetExceeded) => return Ok(None),
        Err(error) => return Err(error.into()),
    }; // 只有额外页预算不足是优化miss，真实Native/context/映射故障仍拒绝原候选。
    let graph = Graph::new(validate, weaks)?;
    graph.install(request, blend, &pixels, validate)?;
    native(validate, || graph.second.setOpacity(0.0))?;
    native(validate, || {
        graph.container.renderInContext(pixels.context())
    })?;
    native(validate, || CGContext::flush(Some(pixels.context())))?;
    let single = pixels.first_row(); // 有限复制已结束，不能把输出slice借用跨过下一次Native。
    native(validate, || graph.second.setOpacity(1.0))?;
    native(validate, || {
        CGContext::clear_rect(
            Some(pixels.context()),
            NSRect::new(NSPoint::ZERO, NSSize::new(256.0, 256.0)),
        )
    })?;
    native(validate, || {
        graph.container.renderInContext(pixels.context())
    })?;
    native(validate, || CGContext::flush(Some(pixels.context())))?;
    drop(graph); // 先实际清mask/contents/颜色/子关系，后归还provider/context；无RefCell借用。
    validate()?;
    let table = pixels.finish(request, blend, &single);
    validate()?;
    if observer.snapshot().release_failed {
        return Err(NativeMarkdownPagedTextViewError::Raster(
            "校准provider或context的真实归还失败".into(),
        ));
    }
    Ok(Some(table?))
}

/// 与实际发布相同的五角色：container局部背景，两个按模式选色的前景各持独立A8mask。
struct Graph {
    container: Retained<CALayer>,
    first: Retained<CALayer>,
    second: Retained<CALayer>,
    first_mask: Retained<CALayer>,
    second_mask: Retained<CALayer>,
}
impl Graph {
    fn new(
        validate: &impl Fn() -> Result<()>,
        weaks: &mut Vec<NativeWeak<AnyObject>>,
    ) -> Result<Self> {
        let mut create = || -> Result<Retained<CALayer>> {
            validate()?;
            let layer = CALayer::new();
            let object: &AnyObject = &layer;
            weaks.push(NativeWeak::from(object));
            validate()?; // 创建已经发生就先登记Weak，后置撤权也必须观察实际归还。
            Ok(layer)
        };
        Ok(Self {
            container: create()?,
            first: create()?,
            second: create()?,
            first_mask: create()?,
            second_mask: create()?,
        })
    }
    fn install(
        &self,
        request: &MaskPaletteRequest,
        blend: MaskPairBlend,
        pixels: &MaskCalibrationPixels,
        validate: &impl Fn() -> Result<()>,
    ) -> Result<()> {
        let rect = NSRect::new(NSPoint::ZERO, NSSize::new(256.0, 256.0));
        for (layer, mask) in [
            (&self.container, false),
            (&self.first, false),
            (&self.second, false),
            (&self.first_mask, true),
            (&self.second_mask, true),
        ] {
            native(validate, || layer.setBounds(rect))?;
            native(validate, || layer.setFrame(rect))?;
            native(validate, || layer.setContentsScale(1.0))?;
            // SAFETY: 两个公开QuartzCore格式常量静态有效；只有实际A8mask采用Gray8存储hint。
            let format = unsafe {
                if mask {
                    objc2_quartz_core::kCAContentsFormatGray8Uint
                } else {
                    objc2_quartz_core::kCAContentsFormatRGBA8Uint
                }
            };
            native(validate, || {
                // 标准setter只接收上方已取得的公开格式值，不改变原图像像素。
                layer.setContentsFormat(format);
            })?;
            let actual = native(validate, || layer.contentsFormat())?;
            if !native(validate, || actual.isEqualToString(format))? {
                return Err(NativeMarkdownPagedTextViewError::Raster(
                    "双遮罩校准图的实际存储格式不一致".into(),
                )); // setter成功不代表实际属性相同；拒绝此校准，不能拿旧RGBA格式的表给Gray签票。
            }
            native(validate, || {
                // SAFETY: 原A8mask为256²，layer同尺寸；固定resize不丢像素或改变原glyph。
                unsafe { layer.setContentsGravity(objc2_quartz_core::kCAGravityResize) };
            })?;
        }
        let color = |value: [u8; 3]| -> Result<CFRetained<CGColor>> {
            native(validate, || {
                CGColor::new_srgb(
                    f64::from(value[0]) / 255.0,
                    f64::from(value[1]) / 255.0,
                    f64::from(value[2]) / 255.0,
                    1.0,
                )
            })
        };
        let background = color(request.background)?;
        let foreground = color(request.foreground)?;
        native(validate, || {
            self.container.setBackgroundColor(Some(&background))
        })?;
        native(validate, || {
            self.first.setBackgroundColor(Some(&foreground))
        })?;
        let second = match blend {
            MaskPairBlend::ForegroundThenForeground => &foreground,
            MaskPairBlend::ForegroundThenBackground => &background,
        }; // 第二层只选原端点，不引入新颜色或不透明度；资格仍由实际完整RGB表决定。
        native(validate, || self.second.setBackgroundColor(Some(second)))?;
        let (first, second) = pixels.images();
        native(validate, || {
            // SAFETY: 两个CGImage各持独立真实只读provider，局部pixels保持活跃；不向CF发送ObjC消息。
            unsafe {
                self.first_mask
                    .setContents(Some(&*std::ptr::from_ref(first).cast::<AnyObject>()))
            };
        })?;
        native(validate, || {
            // SAFETY: 第二mask同样来自独立原provider，不能用第一份allocation的一次callback共享授权。
            unsafe {
                self.second_mask
                    .setContents(Some(&*std::ptr::from_ref(second).cast::<AnyObject>()))
            };
        })?;
        native(validate, || {
            // SAFETY: 固定原mask由本局部图独占；不持正文、View、delegate或业务回调。
            unsafe { self.first.setMask(Some(&self.first_mask)) };
        })?;
        native(validate, || {
            // SAFETY: 第二前景只使用自己的原第二mask，主线程下建立有限关系。
            unsafe { self.second.setMask(Some(&self.second_mask)) };
        })?;
        let children = native(validate, || {
            NSArray::from_retained_slice(&[self.first.clone(), self.second.clone()])
        })?;
        native(validate, || {
            // SAFETY: 数组仅含本图两个固定前景，未挂到产品视图，不发布部分画面。
            unsafe { self.container.setSublayers(Some(&children)) };
        })?;
        Ok(())
    }
}
impl Drop for Graph {
    fn drop(&mut self) {
        // 清理只作用本局部从未发布/暴露的独占图；即使current已撤销也必须归还自己的载荷。
        // SAFETY: 所有对象均在主线程，关系只来自此实例；不清用户视图或后继Owner的图。
        unsafe {
            self.first.setMask(None);
            self.second.setMask(None);
            self.first_mask.setContents(None);
            self.second_mask.setContents(None);
            self.container.setSublayers(None);
        }
        self.container.setBackgroundColor(None);
        self.first.setBackgroundColor(None);
        self.second.setBackgroundColor(None);
    }
}

fn native<T>(validate: &impl Fn() -> Result<()>, call: impl FnOnce() -> T) -> Result<T> {
    validate()?;
    let result = call();
    validate()?;
    Ok(result)
}
