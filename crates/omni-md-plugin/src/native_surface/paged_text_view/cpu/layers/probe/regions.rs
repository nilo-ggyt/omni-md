//! 非整齐二维分区的真实Native差分、复用与外方重挂清理；不把页数节省当作产品峰值证据。

use std::collections::HashSet;

use super::super::super::calibration::Calibrations;
use super::*;

const REGION_WIDTH: usize = 513;
const REGION_HEIGHT: usize = 259;
const FIRST_HEIGHT: usize = 129;
// 原结构层与每次真实发布的角色分别登记；不存在的瓦片没有预建空图层。

fn invalid() -> NativeMarkdownPagedTextViewError {
    NativeMarkdownPagedTextViewError::InvalidGeometry
}

fn verify_page_costs() -> Result<()> {
    // SAFETY: sysconf只查询本进程机器的固定页大小，不借用Native对象或任何像素地址。
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page != 16_384 {
        return Err(invalid());
    } // 本正式机器是16KiB页；换机器不能把此独立预期静默解释成另一组成本。
    let aligned = |length: usize| length.div_ceil(16_384) * 16_384;
    let rgb24 = aligned(512 * 3 * 259);
    let two = 2 * (aligned(512 * 129) + aligned(512 * 130));
    let four = 2 * (aligned(512 * 64) + 3 * aligned(512 * 65));
    if rgb24 != 409_600 || two != 327_680 || four != 360_448 || !(two < four && four < rgb24) {
        return Err(invalid());
    }
    println!(
        "paged_region_slots actual_page_bytes={page} independent_rgb24_page_cost={rgb24} independent_two_region_worst_pair_cost={two} independent_four_region_worst_pair_cost={four} native_process_peak_claim=false formal=false"
    );
    Ok(())
}

/// 只保两条实际sRGB输出的256像素ramp，不按数学公式假定CG混色会产生哪些RGB字节。
fn ramp(background: [u8; 3], foreground: [u8; 3]) -> Result<Canvas> {
    let canvas = Canvas::sized(256, 1)?;
    CGContext::set_rgb_fill_color(
        Some(&canvas.context),
        f64::from(background[0]) / 255.0,
        f64::from(background[1]) / 255.0,
        f64::from(background[2]) / 255.0,
        1.0,
    );
    CGContext::fill_rect(
        Some(&canvas.context),
        NSRect::new(NSPoint::ZERO, NSSize::new(256.0, 1.0)),
    );
    for x in 0..256 {
        CGContext::set_rgb_fill_color(
            Some(&canvas.context),
            f64::from(foreground[0]) / 255.0,
            f64::from(foreground[1]) / 255.0,
            f64::from(foreground[2]) / 255.0,
            x as f64 / 255.0,
        );
        CGContext::fill_rect(
            Some(&canvas.context),
            NSRect::new(NSPoint::new(x as f64, 0.0), NSSize::new(1.0, 1.0)),
        );
    }
    CGContext::flush(Some(&canvas.context));
    Ok(canvas)
}

fn draw_source(source: &mut Canvas) -> Result<(usize, usize, usize)> {
    let ramps = [
        ramp([19, 21, 28], [219, 224, 235])?,
        ramp([31, 61, 93], [245, 173, 41])?,
    ];
    let mut palettes = [HashSet::with_capacity(256), HashSet::with_capacity(256)];
    let mut combined = HashSet::with_capacity(512);
    for y in 0..REGION_HEIGHT {
        let group = usize::from(y >= FIRST_HEIGHT);
        for x in 0..512 {
            // 每行覆盖全部256种coverage且随y移位；顶底颠倒、区边界差一行或行stride错位
            // 不能被恒色行掩盖。直接采用原ramp实际RGBA，不授予任何mask校准证书。
            let at = ((x + y * 7) & 255) * 4;
            let pixel: [u8; 4] = ramps[group].pixels[at..at + 4]
                .try_into()
                .map_err(|_| invalid())?;
            if pixel[3] != 255 {
                return Err(invalid());
            } // 原工作格必须真正opaque；不得为触发分区而抹掉透明数据。
            palettes[group].insert(pixel);
            combined.insert(pixel);
            let target = y * source.row_bytes + x * 4;
            source.pixels[target..target + 4].copy_from_slice(&pixel);
        }
        // 最右1px尾格全高都有实际有色像素；最后一行和每个行padding也不会被裁成512宽。
        let tail = y * source.row_bytes + 512 * 4;
        source.pixels[tail..tail + 4].copy_from_slice(&[7, (y & 255) as u8, 203, 255]);
    }
    CGContext::flush(Some(&source.context));
    let counts = (palettes[0].len(), palettes[1].len(), combined.len());
    if counts.0 <= 1 || counts.0 > 256 || counts.1 <= 1 || counts.1 > 256 || counts.2 <= 256 {
        return Err(invalid());
    } // 用实际颜色基数证明原整块RGB24、两个局部有限palette；不能只根据端点名称猜资格。
    Ok(counts)
}

fn encode(
    source: &Canvas,
    counters: Arc<NativeMarkdownViewportPixels>,
    prior: Option<&TileFrame>,
) -> Result<TileFrame> {
    let mut bitmap = TiledBitmap::new(REGION_WIDTH, REGION_HEIGHT, counters.clone())?;
    let mut calibrations = Calibrations::default();
    while let Some(tile) = bitmap.next_tile() {
        let writable = bitmap.begin_tile(tile)?;
        let row = CGBitmapContextGetBytesPerRow(Some(writable.context()));
        let data = CGBitmapContextGetData(Some(writable.context()));
        if data.is_null() || row < tile.width * 4 {
            return Err(invalid());
        }
        for y in 0..tile.height {
            // SAFETY: 原WritableTile唯一context覆盖真实stride*height；只写本行activeRGBA，
            // slice在prepare/校准/Native封存之前结束，源Canvas的513宽padding另按row_bytes跳过。
            let target = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * row), tile.width * 4)
            };
            let source_at = (tile.y + y) * source.row_bytes + tile.x * 4;
            target.copy_from_slice(&source.pixels[source_at..source_at + tile.width * 4]);
        }
        let prepared = bitmap.prepare_tile(writable)?;
        let plan = bitmap.plan_encoding(prepared)?;
        let count = plan.region_count();
        let expected = if tile.x == 0 { 2 } else { 1 };
        if count != expected {
            return Err(invalid());
        } // 16KiB页下首格2区严格优于整块/4区，尾格仍占完整一个slot，不能被截掉。
        let mut pairs = Vec::with_capacity(count);
        for index in 0..count {
            let certificate = match plan.mask_request(index)? {
                Some(request) => calibrations.pairs(&request, counters.clone(), &|| Ok(()))?,
                None => None,
            }; // 校准可合法miss；局部Indexed/RGB24仍必须完整精确，不能假定必有双mask。
            pairs.push(certificate);
        }
        let borrowed = pairs.iter().map(Option::as_ref).collect::<Vec<_>>();
        bitmap.finish_encoding(plan, &borrowed, prior)?;
    }
    drop(calibrations); // 不让有限校准表跨候选存活，真实临时页不计作零成本。
    let frame = bitmap.finish()?;
    let expected = [
        (0, 0, 512, 129, 130),
        (0, 129, 512, 130, 0),
        (512, 0, 1, 259, 0),
    ];
    if frame.width != REGION_WIDTH
        || frame.height != REGION_HEIGHT
        || frame.tiles.len() != expected.len()
        || frame.tiles.iter().zip(expected).any(|(sealed, expected)| {
            let tile = sealed.tile;
            (tile.x, tile.y, tile.width, tile.height, tile.bottom) != expected
        })
    {
        return Err(invalid());
    } // bottom必须是parent.bottom+parent.height-end；两个边界行和1px全高尾格都属于原帧。
    Ok(frame)
}

fn same_images(left: &TileFrame, right: &TileFrame, index: usize) -> bool {
    let (Some(left), Some(right)) = (left.tiles.get(index), right.tiles.get(index)) else {
        return false; // 缺少真实slot不能靠zip截短后空集合的all通过。
    };
    left.tile == right.tile
        && left.presentation == right.presentation
        && std::ptr::eq::<CGImage>(&*left.image, &*right.image)
        && match (&left.secondary_image, &right.secondary_image) {
            (None, None) => true, // 无配对的原Image仍须上面的真实primary身份完全相同。
            (Some(left), Some(right)) => std::ptr::eq::<CGImage>(&**left, &**right),
            _ => false, // 原校准的Some/None类别也属于复用资格，不能丢掉一张真实mask图。
        }
}

fn changed_images(left: &TileFrame, right: &TileFrame, index: usize) -> bool {
    let (Some(left), Some(right)) = (left.tiles.get(index), right.tiles.get(index)) else {
        return false;
    };
    left.tile == right.tile
        && !std::ptr::eq::<CGImage>(&*left.image, &*right.image)
        && match (&left.secondary_image, &right.secondary_image) {
            (None, None) => true, // 非配对表示只需要真实替换原primary图。
            (Some(left), Some(right)) => !std::ptr::eq::<CGImage>(&**left, &**right),
            _ => false, // 同色表单像素变更不应凭空改变是否持第二图；不把类型变更冒充局部复用。
        } // 原配对是一组资格；不能只换其中一图再把另一平面按字节偶合借为旧对。
}

fn render_exact(
    pool: &LayerPool,
    backing: &CALayer,
    frame: &TileFrame,
    source: &Canvas,
    bounds: NSRect,
    previous: Option<&Publication>,
) -> Result<Publication> {
    let reference = CGBitmapContextCreateImage(Some(&source.context)).ok_or_else(invalid)?;
    let baseline = Canvas::sized(REGION_WIDTH, REGION_HEIGHT)?;
    baseline.blue(bounds);
    CGContext::draw_image(Some(&baseline.context), bounds, Some(&reference));
    CGContext::flush(Some(&baseline.context));
    let rendered = Canvas::sized(REGION_WIDTH, REGION_HEIGHT)?;
    rendered.blue(bounds);
    // 分区边界和尾列也沿同一生产准入；只有真实相同槽才保留 Native 图，
    // 单像素变化仍交生产部分发布，完整颜色结果继续由独立 RGBA 图决定。
    let reused = previous
        .map(|previous| pool.reuse_publication(backing, frame, previous, bounds, 1.0, &|| true))
        .transpose()?
        .flatten();
    let publication = match reused {
        Some(publication) => publication,
        None => {
            let mut attempt = pool.attempt()?;
            let partial = previous
                .map(|previous| {
                    pool.publish_partial(backing, frame, previous, &mut attempt, &|| true)
                })
                .transpose()?
                .flatten();
            match partial {
                Some(publication) => publication,
                None => pool.publish(backing, frame, &attempt, bounds, 1.0, &|| true)?,
            }
        }
    };
    pool.validate(backing, &publication, &|| true)?;
    pool.mount().renderInContext(&rendered.context);
    CGContext::flush(Some(&rendered.context));
    if baseline.width != rendered.width || baseline.height != rendered.height {
        return Err(invalid());
    }
    for y in 0..REGION_HEIGHT {
        let before = y * baseline.row_bytes;
        let after = y * rendered.row_bytes;
        let active = REGION_WIDTH * 4;
        if baseline.pixels[before..before + active] != rendered.pixels[after..after + active] {
            let byte = baseline.pixels[before..before + active]
                .iter()
                .zip(&rendered.pixels[after..after + active])
                .position(|(expected, actual)| expected != actual)
                .ok_or_else(invalid)?;
            let x = byte / 4;
            println!(
                "paged_region_slots first_active_rgba_mismatch_x={x} first_active_rgba_mismatch_y={y} expected={:?} actual={:?} exact_required=true formal=false",
                &baseline.pixels[before + x * 4..before + x * 4 + 4],
                &rendered.pixels[after + x * 4..after + x * 4 + 4],
            );
            return Err(invalid());
        } // 逐行精确比较全部513个activeRGBA，包括分界129/130、尾列和第259行，无像素容差。
    }
    Ok(publication)
}

pub(super) fn run(_mtm: MainThreadMarker) -> Result<()> {
    verify_page_costs()?;
    let counters = Arc::new(NativeMarkdownViewportPixels::default());
    let observer = counters.observer();
    let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
        let successor = CALayer::new();
        let foreign_owner = layer_weak(&successor);
        let (intact, weaks) = autoreleasepool(|_| -> Result<_> {
            let bounds = NSRect::new(
                NSPoint::ZERO,
                NSSize::new(REGION_WIDTH as f64, REGION_HEIGHT as f64),
            );
            let mut source = Canvas::sized(REGION_WIDTH, REGION_HEIGHT)?;
            let colors = draw_source(&mut source)?;
            let backing = CALayer::new();
            backing.setFrame(bounds);
            backing.setBounds(bounds);
            let pool = LayerPool::new(&|| true)?;
            backing.addSublayer(pool.mount());
            let mut weaks = pool.weaks();
            weaks.push(layer_weak(&backing));
            let mut frames = Vec::with_capacity(3);
            let mut publication = None;
            let result = (|| -> Result<usize> {
                frames.push(encode(&source, counters.clone(), None)?);
                publication = Some(render_exact(
                    &pool, &backing, &frames[0], &source, bounds, None,
                )?);
                // 只登记该次真实发布的瓦片；后继缩窗/换组的旧角色仍须按原 Weak 验证已归还。
                for group in &pool.groups {
                    let slots = group.slots.borrow().clone();
                    for slot in slots {
                        slot.add_weaks(&mut weaks);
                    }
                }
                let same = encode(&source, counters.clone(), Some(&frames[0]))?;
                let same_reused = same.reused_images == 3
                    && (0..3).all(|index| same_images(&frames[0], &same, index));
                frames.push(same);
                publication = Some(render_exact(
                    &pool,
                    &backing,
                    &frames[1],
                    &source,
                    bounds,
                    publication.as_ref(),
                )?);
                // 只登记该次真实发布的瓦片；后继缩窗/换组的旧角色仍须按原 Weak 验证已归还。
                for group in &pool.groups {
                    let slots = group.slots.borrow().clone();
                    for slot in slots {
                        slot.add_weaks(&mut weaks);
                    }
                }
                // 原色表内仅改一个真实像素；其它行、局部palette端点、几何及两后继子tile均不变。
                let first: [u8; 4] = source.pixels[..4].try_into().map_err(|_| invalid())?;
                let other = source.pixels[4..512 * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .find(|pixel| **pixel != first)
                    .copied()
                    .ok_or_else(invalid)?;
                source.pixels[..4].copy_from_slice(&other);
                CGContext::flush(Some(&source.context));
                let changed = encode(&source, counters.clone(), Some(&frames[0]))?;
                let changed_reused = changed.reused_images == 2
                    && changed_images(&frames[0], &changed, 0)
                    && (1..3).all(|index| same_images(&frames[0], &changed, index));
                frames.push(changed);
                publication = Some(render_exact(
                    &pool,
                    &backing,
                    &frames[2],
                    &source,
                    bounds,
                    publication.as_ref(),
                )?);
                // 只登记该次真实发布的瓦片；后继缩窗/换组的旧角色仍须按原 Weak 验证已归还。
                for group in &pool.groups {
                    let slots = group.slots.borrow().clone();
                    for slot in slots {
                        slot.add_weaks(&mut weaks);
                    }
                }
                let secondary = frames[0]
                    .tiles
                    .iter()
                    .filter(|tile| tile.secondary_image.is_some())
                    .count();
                println!(
                    "paged_region_slots actual_width=513 actual_height=259 actual_source_colors={colors:?} actual_slots=3 actual_same_primary_images=3 actual_same_secondary_images={secondary} actual_changed_tile_reuses=2 original_slot_image_pairs_verified={same_reused}/{changed_reused} exact_active_rgba=true rowstride={} assumed_mask_certificates=false formal=false whole_product=false",
                    source.row_bytes,
                );
                if !same_reused || !changed_reused {
                    return Err(invalid());
                }
                Ok(secondary)
            })();
            successor.addSublayer(pool.mount()); // 真实提交后迁给外方，旧owner清理不能夺走这条parent关系。
            let rejected = publication.as_ref().is_some_and(|publication| {
                matches!(
                    pool.validate(&backing, publication, &|| true),
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                )
            });
            let detached = pool.detach(&backing);
            let foreign_preserved = pool
                .mount()
                .superlayer()
                .as_deref()
                .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &*successor));
            drop((frames, publication)); // 外方仍持原mount；先归还调用方全部frame，不借先drop foreign通过资源门。
            CATransaction::flush();
            println!(
                "paged_region_slots actual_result={result:?} actual_detach={detached} foreign_parent_preserved={foreign_preserved} actual_rejected={rejected} formal=false"
            );
            drop((pool, backing, source));
            CATransaction::flush();
            Ok((
                result.is_ok() && rejected && !detached && foreign_preserved,
                weaks,
            ))
        })?; // 先排空本owner及Native临时数组；外层外方successor仍真实持活，不能用缓存时序放宽Weak门。
        CATransaction::flush();
        let pixels_released_with_foreign = observer.is_released();
        let foreign_mount_alive = weaks[0].load().is_some();
        let foreign_owner_alive = foreign_owner.load().is_some();
        let other_owned_released = weaks[1..].iter().all(|weak| weak.load().is_none());
        println!(
            "paged_region_slots original_provider_maps_zero_with_foreign_alive={pixels_released_with_foreign} foreign_mount_still_alive={foreign_mount_alive} foreign_owner_still_alive={foreign_owner_alive} other_pool_and_backing_weak_nil={other_owned_released} actual_pixels={:?} formal=false",
            observer.snapshot(),
        );
        // 外方在核真实provider/mmap及其它自有角色后自行归还mount；这不算原owner的关闭动作。
        // SAFETY: successor只属于本有限控制，None仅移除它自己后挂的真实mount。
        unsafe { successor.setSublayers(None) };
        drop(successor);
        CATransaction::flush();
        Ok((
            intact
                && pixels_released_with_foreign
                && foreign_mount_alive
                && foreign_owner_alive
                && other_owned_released,
            weaks,
        ))
    })?;
    let native_released = weaks.iter().all(|weak| weak.load().is_none());
    println!(
        "paged_region_slots actual_native_roles={} actual_all_weak_nil={native_released} actual_pixels_zero={} actual_pixels={:?} whole_product=false formal=false",
        weaks.len(),
        observer.is_released(),
        observer.snapshot(),
    );
    if !passed || !native_released || !observer.is_released() {
        return Err(invalid());
    }
    Ok(())
}
