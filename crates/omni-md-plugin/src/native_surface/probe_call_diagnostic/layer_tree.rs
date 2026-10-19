//! 显式合成验收的已有图层树快照；不创建layer或读取其contents。

use std::sync::atomic::{AtomicU8, Ordering};

use objc2::Message;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_app_kit::NSView;
use objc2_quartz_core::{
    CALayer, kCAContentsFormatGray8Uint, kCAContentsFormatRGBA8Uint, kCAContentsFormatRGBA16Float,
};

use super::{DiagnosticScope, TASK_VM_INFO_REV3_COUNT, capture_memory_classes};

const MAX_NODES: usize = 128;
const MAX_EDGES: usize = MAX_NODES * 2;
static PHASES: AtomicU8 = AtomicU8::new(0);

#[derive(Clone)]
enum Node {
    View(Retained<NSView>),
    Layer(Retained<CALayer>),
}

impl Node {
    fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::View(left), Self::View(right)) => std::ptr::eq(&**left, &**right),
            (Self::Layer(left), Self::Layer(right)) => std::ptr::eq(&**left, &**right),
            _ => false, // 不跨原生类型把同一数值地址认作另一个节点。
        }
    }
}

struct Tree {
    nodes: [Option<Node>; MAX_NODES],
    used: usize,
    edges: usize,
    complete: bool,
}

impl Tree {
    fn new() -> Self {
        Self {
            nodes: std::array::from_fn(|_| None),
            used: 0,
            edges: 0,
            complete: true,
        }
    }

    fn add(&mut self, node: Node) -> Option<usize> {
        if let Some(index) = self.nodes[..self.used].iter().position(|existing| {
            existing
                .as_ref()
                .is_some_and(|value| value.same_identity(&node))
        }) {
            return Some(index); // 同一已有layer可能同时由view和父layer找到；只观察一次。
        }
        if self.used == MAX_NODES {
            self.complete = false;
            return None; // 固定栈表满后明确不完整，不扩容或另起递归遍历。
        }
        let index = self.used;
        self.nodes[index] = Some(node);
        self.used += 1;
        Some(index)
    }

    fn edge(&mut self) -> bool {
        if self.edges == MAX_EDGES {
            self.complete = false;
            return false; // 重复边同样有界，避免异常共享图在未增加节点时无界访问。
        }
        self.edges += 1;
        true
    }
}

/// 运行时class名只标识实现类型；从不调用对象description、layer.name或窗口标题。
fn class_name(value: &AnyObject) -> &str {
    let bytes = value.class().name().to_bytes();
    std::str::from_utf8(&bytes[..bytes.len().min(96)]).unwrap_or("<class-unavailable>")
}

/// 只识别公开常量；自定义format值也不作为任意字符串写入日志。
fn contents_format(layer: &CALayer) -> &'static str {
    let format = layer.contentsFormat();
    // SAFETY: 三个公开QuartzCore常量自macOS10.12可用，当前调用只同步比较NSString值。
    let (rgba8, rgba16, gray8) = unsafe {
        (
            kCAContentsFormatRGBA8Uint,
            kCAContentsFormatRGBA16Float,
            kCAContentsFormatGray8Uint,
        )
    };
    if format.isEqualToString(rgba8) {
        "rgba8"
    } else if format.isEqualToString(rgba16) {
        "rgba16float"
    } else if format.isEqualToString(gray8) {
        "gray8"
    } else {
        "other" // 未知值不能据此猜测真实像素格式或申请的图形资源容量。
    }
}

/// 两个原产品阶段各一次，只取已存在对象；所有强claim在同步autoreleasepool结束前归还。
pub(crate) fn layer_audit_point(phase: &'static str, body: &NSView) {
    let bit = match phase {
        "table_110_request_return" => 1,
        "table_executor_pending_begin" => 2,
        _ => return,
    };
    // SAFETY: 固定NUL结束环境键只查询存在性，不持有环境指针或读取其他环境内容。
    if unsafe { libc::getenv(c"OMNI_NATIVE_PROBE_ALLOCATION_AUDIT".as_ptr()) }.is_null() {
        return;
    }
    if PHASES.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return; // 不按重排、窗口或第二次激活重置预算。
    }
    let Some(_scope) = DiagnosticScope::enter() else {
        return; // 同一诊断递归/并发时不重试，不影响原请求及其验收门禁。
    };
    autoreleasepool(|_| {
        let mut tree = Tree::new();
        let window = body.window();
        let content = window.as_ref().and_then(|value| value.contentView());
        let backing_scale = window.as_ref().map(|value| value.backingScaleFactor());
        if let Some(content) = content {
            tree.add(Node::View(content));
        } else {
            tree.complete = false; // 没有原窗口根时仍可记录body，但不能声称整窗树完整。
        }
        tree.add(Node::View(body.retain()));
        eprintln!(
            "native_layer_tree phase={phase} event=begin window_present={} backing_scale={backing_scale:?} node_cap={MAX_NODES} edge_cap={MAX_EDGES} only_existing=true contents_unread=true formal=false",
            window.is_some()
        );
        let mut index = 0;
        while index < tree.used {
            // 留在固定数组内的原对象claim保证去重身份不会因本轮局部drop被地址复用。
            let node = tree.nodes[index].as_ref().expect("已登记的树节点").clone();
            match node {
                Node::View(view) => {
                    let frame = view.frame();
                    let bounds = view.bounds();
                    let visible = view.visibleRect();
                    let wants_layer = view.wantsLayer();
                    let update_layer = view.wantsUpdateLayer();
                    let opaque = view.isOpaque();
                    let layer = view.layer(); // 只取关联值，不调用makeBackingLayer或任何setter。
                    let layer_index = layer.and_then(|layer| tree.add(Node::Layer(layer)));
                    let children = view.subviews();
                    let count = children.count();
                    eprintln!(
                        "native_layer_tree phase={phase} event=view node={index} class={} body={} frame={frame:?} bounds={bounds:?} visible={visible:?} wants_layer={wants_layer} update_layer={update_layer} opaque={opaque} layer_node={layer_index:?} child_count={count} formal=false",
                        class_name(&view),
                        std::ptr::eq(&*view, body)
                    );
                    for child in 0..count {
                        if tree.used == MAX_NODES || !tree.edge() || children.count() != count {
                            tree.complete = false;
                            break; // 不完整、预算满或原数组改变均停止当前分支，不补签另一个图。
                        }
                        let child_index = tree.add(Node::View(children.objectAtIndex(child)));
                        eprintln!(
                            "native_layer_tree phase={phase} event=view_edge parent={index} child={child_index:?} formal=false"
                        );
                    }
                }
                Node::Layer(layer) => {
                    let bounds = layer.bounds();
                    let position = layer.position();
                    let scale = layer.contentsScale();
                    let format = contents_format(&layer);
                    let rasterized = layer.shouldRasterize();
                    let rasterization_scale = layer.rasterizationScale();
                    let asynchronous = layer.drawsAsynchronously();
                    // SAFETY: 原layer是主线程仍存活的真实CALayer；只同步读取公开子层数组，
                    // 不写层树、不把数组交给其他线程，且所有claim在本次pool内结束。
                    let children = unsafe { layer.sublayers() };
                    let count = children.as_ref().map_or(0, |values| values.count());
                    eprintln!(
                        "native_layer_tree phase={phase} event=layer node={index} class={} bounds={bounds:?} position={position:?} contents_scale={scale} contents_format={format} rasterized={rasterized} rasterization_scale={rasterization_scale} asynchronous={asynchronous} child_count={count} formal=false",
                        class_name(&layer)
                    );
                    if let Some(children) = children {
                        for child in 0..count {
                            if tree.used == MAX_NODES || !tree.edge() || children.count() != count {
                                tree.complete = false;
                                break;
                            }
                            let child_index = tree.add(Node::Layer(children.objectAtIndex(child)));
                            eprintln!(
                                "native_layer_tree phase={phase} event=layer_edge parent={index} child={child_index:?} formal=false"
                            );
                        }
                    }
                }
            }
            index += 1;
        }
        eprintln!(
            "native_layer_tree phase={phase} event=end nodes={} edges={} complete={} pixel_capacity_not_measured=true formal=false",
            tree.used, tree.edges, tree.complete
        );
    }); // 先销毁全部诊断Native claim，再观察诊断自身是否伴随新的graphics记账。
    let after = capture_memory_classes();
    if after.vm_status == libc::KERN_SUCCESS && after.vm_count == TASK_VM_INFO_REV3_COUNT {
        let footprint = after.vm.phys_footprint;
        let graphics = after.vm.ledger_tag_graphics_footprint;
        eprintln!(
            "native_layer_tree phase={phase} event=after_release vm_available=true footprint_bytes={footprint} graphics_footprint_bytes={graphics} heap_in_use_bytes={} formal=false",
            after.malloc.size_in_use
        );
    } else {
        eprintln!(
            "native_layer_tree phase={phase} event=after_release vm_available=false vm_status={} vm_count={} formal=false",
            after.vm_status, after.vm_count
        );
    }
}
