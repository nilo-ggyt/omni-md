//! 真实NSTextLocation/NSTextRange机制控制：只创建固定位置元数据，无NSApplication或正文排版图。

use objc2::{
    AnyThread,
    rc::{Weak, autoreleasepool},
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{NSTextLocation, NSTextRange};
use objc2_foundation::{MainThreadMarker, NSComparisonResult, NSObjectProtocol};
use omni_markdown_core::{MarkdownDocumentBuffer, MarkdownLayoutTextSource};
use omni_md_plugin::native_surface::paged_locations::{
    NativeMarkdownPagedLocationError, NativeMarkdownPagedTextLocation as Location,
};

fn location(value: &Location) -> &ProtocolObject<dyn NSTextLocation> {
    ProtocolObject::from_ref(value)
}

fn main() {
    assert!(
        std::env::args_os().nth(1).is_none(),
        "位置控制不接受未知参数"
    );
    let mtm = MainThreadMarker::new().expect("Native位置控制必须在实际主线程");
    let weak_objects = autoreleasepool(|_| {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("positions.md");
        std::fs::write(&path, "A🙂אב中").unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 4096).unwrap();
        let mapping = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let source_observer = source.resource_observer();
        let another = MarkdownLayoutTextSource::capture(&document);
        let start = Location::new(&source, 0, mtm, &|| true).unwrap();
        let at = Location::new(&source, 3, mtm, &|| true).unwrap();
        let same = Location::new(&source, 3, mtm, &|| true).unwrap();
        let end = Location::new(&source, source.utf16_len(), mtm, &|| true).unwrap();
        let foreign = Location::new(&another, 3, mtm, &|| true).unwrap();
        let at_object: &AnyObject = &at;
        let same_object: &AnyObject = &same;
        assert!(at.isEqual(Some(same_object)));
        assert!(same.isEqual(Some(at_object)));
        assert_eq!(at.hash(), same.hash());
        assert_eq!(
            location(&start).compare(location(&at)),
            NSComparisonResult::Ascending
        );
        assert_eq!(
            location(&at).compare(location(&same)),
            NSComparisonResult::Same
        );
        assert_eq!(
            location(&end).compare(location(&at)),
            NSComparisonResult::Descending
        );
        assert!(!foreign.isEqual(Some(at_object)));
        assert_ne!(
            location(&foreign).compare(location(&at)),
            NSComparisonResult::Same
        );
        assert_eq!(Location::utf16_for_source(location(&at), &source), Some(3));
        assert_eq!(
            Location::utf16_for_source(location(&foreign), &source),
            None
        );
        assert!(matches!(
            Location::new(&source, source.utf16_len() + 1, mtm, &|| true),
            Err(NativeMarkdownPagedLocationError::OutsideSource)
        ));
        assert!(matches!(
            Location::new(&source, 0, mtm, &|| false),
            Err(NativeMarkdownPagedLocationError::Revoked)
        ));
        let range = NSTextRange::initWithLocation_endLocation(
            NSTextRange::alloc(),
            location(&start),
            Some(location(&end)),
        )
        .expect("原source的真实完整范围");
        assert!(!range.isEmpty());
        assert!(range.containsLocation(location(&at)));
        let range_start = range.location();
        let range_end = range.endLocation();
        assert_eq!(Location::utf16_for_source(&range_start, &source), Some(0));
        assert_eq!(
            Location::utf16_for_source(&range_end, &source),
            Some(source.utf16_len())
        );
        drop(range_start);
        drop(range_end);
        let objects: [&AnyObject; 6] = [&start, &at, &same, &end, &foreign, &range];
        let weak = objects.map(Weak::from);
        // 即使Native范围和位置尚存，只有弱来源键，不应继续保活Doc或源映射。
        drop(document);
        drop(source);
        drop(another);
        assert!(source_observer.payload_released());
        assert!(mapping.unmap_succeeded());
        assert_eq!(mapping.mapped_bytes(), 0);
        assert!(!mapping.failed());
        drop(range);
        drop(start);
        drop(at);
        drop(same);
        drop(end);
        drop(foreign);
        weak
    });
    for weak in &weak_objects {
        assert!(
            weak.load().is_none(),
            "Native位置与范围都必须真实weak nil，不按类型豁免"
        );
    }
    println!(
        "native_paged_locations source_identity=true logical_utf16=true canonical_mapping_unmapped_while_positions_live=true native_weak_nil=true whole_product=false"
    );
}
