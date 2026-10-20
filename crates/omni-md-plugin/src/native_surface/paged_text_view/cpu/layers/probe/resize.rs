//! 缩窗后保留当前完整画面，同时验证上一帧真实角色已经销毁，不能把它们变成空闲池。

use super::*;

pub(super) fn run() -> Result<()> {
    let counters = Arc::new(NativeMarkdownViewportPixels::default());
    let observer = counters.observer();
    let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
        let mut source = Canvas::new()?;
        let wide = fixture(&mut source, Arc::clone(&counters))?;
        let wide_bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH as f64, HEIGHT as f64));
        let backing = CALayer::new();
        backing.setFrame(wide_bounds);
        backing.setBounds(wide_bounds);
        let pool = LayerPool::new(&|| true)?;
        backing.addSublayer(pool.mount());
        let initially_empty = pool
            .groups
            .iter()
            .all(|group| group.slots.borrow().is_empty());
        let mut weaks = pool.weaks();
        weaks.push(layer_weak(&backing));
        let (old_roles, correct_roles) = autoreleasepool(|_| -> Result<_> {
            let attempt = pool.attempt()?;
            let published = pool.publish(&backing, &wide, &attempt, wide_bounds, 1.0, &|| true)?;
            pool.validate(&backing, &published, &|| true)?;
            let slots = pool.groups[0].slots.borrow().clone();
            let mut old_roles = Vec::new();
            for slot in &slots {
                slot.add_weaks(&mut old_roles);
            }
            // 完整 Native 验证已证明第一块单遮罩与第二块半透明图的实际挂载关系。
            // 逐个比对真正分配对象的 Weak 身份，额外预建但未挂载的角色也不能混入此集合。
            let [container, foreground, mask] = slots[0].diagnostic_layers();
            let expected = [&*container, &*foreground, &*mask, &**slots[1].container()];
            let matches = |weak: &Weak<AnyObject>, expected: &CALayer| {
                weak.load().as_deref().is_some_and(|actual| {
                    std::ptr::from_ref(actual).cast::<()>()
                        == std::ptr::from_ref(expected).cast::<()>()
                })
            };
            let correct_roles = old_roles
                .iter()
                .all(|weak| expected.iter().any(|layer| matches(weak, layer)))
                && expected
                    .iter()
                    .all(|layer| old_roles.iter().any(|weak| matches(weak, layer)));
            Ok((old_roles, correct_roles))
        })?;
        weaks.extend(old_roles.iter().cloned());

        // 缩窄后只绘制原图的左侧真实像素；没有缩放、降采样或删除颜色来制造内存下降。
        let narrow_bounds = NSRect::new(NSPoint::ZERO, NSSize::new(512.0, HEIGHT as f64));
        let mut narrow_bitmap = TiledBitmap::new(512, HEIGHT, Arc::clone(&counters))?;
        let tile = narrow_bitmap
            .next_tile()
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        let writable = narrow_bitmap.begin_tile(tile)?;
        let row_bytes = CGBitmapContextGetBytesPerRow(Some(writable.context()));
        let address = CGBitmapContextGetData(Some(writable.context()));
        if address.is_null() || row_bytes < 512 * 4 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        for row in 0..HEIGHT {
            // SAFETY: 唯一可写 context 覆盖 row_bytes*HEIGHT；每次只复制当前有限瓦片的一行。
            unsafe {
                std::ptr::copy_nonoverlapping(
                    source.pixels.as_ptr().add(row * source.row_bytes),
                    address.cast::<u8>().add(row * row_bytes),
                    512 * 4,
                );
            }
        }
        narrow_bitmap.finish_tile(writable, None)?;
        let narrow = narrow_bitmap.finish()?;
        autoreleasepool(|_| -> Result<()> {
            backing.setFrame(narrow_bounds);
            backing.setBounds(narrow_bounds);
            let attempt = pool.attempt()?;
            let published =
                pool.publish(&backing, &narrow, &attempt, narrow_bounds, 1.0, &|| true)?;
            pool.validate(&backing, &published, &|| true)?;
            weaks.extend(pool.weaks());
            Ok(())
        })?;
        drop(wide); // 新帧已经原子提交后才归还调用方的旧像素 claim。
        CATransaction::flush();
        let old_roles_returned = old_roles.iter().all(|weak| weak.load().is_none());
        let retired_group_empty = pool.groups[0].slots.borrow().is_empty();
        // 再次清理已经分离的旧组，模拟下一次复用；实际 Weak 已归零后，其列表分配也须归还。
        // 当前窄画面仍保持挂载，后面的逐像素对照继续确认此资源释放没有破坏活帧。
        let retired_storage_returned =
            pool.groups[0].clear() && pool.groups[0].retired.borrow().capacity() == 0;

        let rendered = Canvas::sized(512, HEIGHT)?;
        rendered.blue(narrow_bounds);
        pool.mount().renderInContext(&rendered.context);
        CGContext::flush(Some(&rendered.context));
        let exact = (0..HEIGHT).all(|row| {
            rendered.pixels[row * rendered.row_bytes..row * rendered.row_bytes + 512 * 4]
                == source.pixels[row * source.row_bytes..row * source.row_bytes + 512 * 4]
        });
        let detached = pool.detach(&backing);
        drop((narrow, pool, backing, source, rendered));
        CATransaction::flush();
        println!(
            "paged_lazy_layer_resize initially_empty={initially_empty} actual_representation_roles={correct_roles} actual_old_role_weak_nil={old_roles_returned} retired_group_empty={retired_group_empty} retired_weak_storage_returned={retired_storage_returned} exact_narrow_rgba={exact} actual_detach={detached} whole_product=false formal=false"
        );
        Ok((
            initially_empty
                && correct_roles
                && old_roles_returned
                && retired_group_empty
                && retired_storage_returned
                && exact
                && detached,
            weaks,
        ))
    })?;
    CATransaction::flush();
    let native_returned = weaks.iter().all(|weak| weak.load().is_none());
    if !passed || !native_returned || !observer.is_released() {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(())
}
