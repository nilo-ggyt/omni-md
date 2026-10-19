//! 本轮原关闭收据的真实资源事件交付；不以窗口关闭、队列轮数或时间流逝授权成功。

use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedCloseReceipt;
use std::{cell::Cell, rc::Rc};

pub fn wait(receipt: &Rc<NativeMarkdownPagedCloseReceipt>) {
    let arrived = Rc::new(Cell::new(false));
    let observed = Rc::clone(&arrived);
    let run_loop = CFRunLoop::current().expect("关闭资源事件只在原主runloop交付");
    receipt.when_released(move || {
        observed.set(true);
        run_loop.stop(); // 只有这份原收据的真实资源relay能结束本次交付，不取Native或业务强边。
    });
    // SAFETY: 原main runloop仅交付一次有限3秒；超时明确失败，不循环重试或更换关闭收据。
    unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 3.0, false) };
    assert!(arrived.get(), "原关闭收据在3秒内没有真实资源完成事件");
}
