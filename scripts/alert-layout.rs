//! Native layout regression probe. Creates hidden AppKit objects only: no
//! activation, modal loop, services, user files, or interaction with the app.
//! Run: cargo run --release -p erlkonig-mac --example alert-layout

#[cfg(target_os = "macos")]
#[path = "../crates/erlkonig-mac/src/alert.rs"]
mod alert;

#[cfg(target_os = "macos")]
use objc2::MainThreadMarker;
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSApplication, NSPasteboard, NSPasteboardTypeString, NSScrollView, NSTextView,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSRange, NSSize};

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("alert-layout: the AppKit probe requires macOS");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    let mtm = MainThreadMarker::new().expect("native probe runs on main thread");
    let _app = NSApplication::sharedApplication(mtm);
    let report = format!(
        "{}\n{}\nEND — vollständige Diagnose 🦉",
        "Failure: synthetic diagnostic with selectable details\n".repeat(2000),
        "long-unbroken-diagnostic-".repeat(500),
    );
    for screen in [
        NSSize::new(1440.0, 900.0),
        NSSize::new(1024.0, 768.0),
        NSSize::new(800.0, 600.0),
        NSSize::new(640.0, 480.0),
    ] {
        let dialog = alert::message_alert(mtm, "Attention needed", &report, &["OK"], screen);
        let window = dialog.window();
        assert!(!window.isVisible(), "probe must not show a window");
        assert!(window.frame().size.height <= screen.height - 40.0);
        assert!(window.frame().size.width <= screen.width - 40.0);
        let scroll = dialog
            .accessoryView()
            .unwrap()
            .downcast::<NSScrollView>()
            .unwrap();
        let body = scroll
            .documentView()
            .unwrap()
            .downcast::<NSTextView>()
            .unwrap();
        assert!(body.isSelectable());
        assert!(!body.isEditable());
        assert_eq!(body.string().to_string(), report, "no lost diagnostics");
        assert!(body.frame().size.height > scroll.contentSize().height);
        assert!(body.frame().size.width <= scroll.contentSize().width + 1.0);
        let length = body.string().length();
        body.setSelectedRange(NSRange::new(0, length));
        assert_eq!(body.selectedRange(), NSRange::new(0, length));
        // Exercise native copying without reading or modifying JP's clipboard.
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        let plain_text = unsafe { NSPasteboardTypeString };
        assert!(
            body.writeSelectionToPasteboard_types(&pasteboard, &body.writablePasteboardTypes())
        );
        assert_eq!(
            pasteboard.stringForType(plain_text).unwrap().to_string(),
            report
        );
        pasteboard.clearContents();
        body.scrollRangeToVisible(NSRange::new(length - 1, 1));
        assert!(
            scroll.documentVisibleRect().origin.y > 0.0,
            "tail is scrollable"
        );
        let button = dialog.buttons().firstObject().unwrap();
        assert_eq!(button.keyEquivalent().to_string(), "\r");
        let frame = button.convertRect_toView(button.bounds(), window.contentView().as_deref());
        let content = window.contentView().unwrap().bounds();
        assert!(frame.origin.y >= 0.0);
        assert!(frame.origin.y + frame.size.height <= content.size.height);
        println!(
            "PASS {:?}: window {:?}, full {} UTF-16 units selected/copied, scrollable tail, visible button bounds, Return default",
            screen,
            window.frame().size,
            length
        );
    }
    let short = alert::message_alert(
        mtm,
        "Erlkonig",
        "Short message",
        &["OK", "Cancel"],
        NSSize::new(800.0, 600.0),
    );
    assert!(short.accessoryView().is_none());
    assert_eq!(short.informativeText().to_string(), "Short message");
    assert_eq!(short.buttons().count(), 2);
    println!("PASS short alert retains native informative text and buttons");
}
