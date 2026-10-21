//! Bounded, selectable local diagnostics; the native buttons never scroll.

use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{NSAlert, NSAutoresizingMaskOptions, NSFont, NSScrollView, NSTextView};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize, NSString};

pub(crate) fn message_alert(
    mtm: MainThreadMarker,
    title: &str,
    text: &str,
    buttons: &[&str],
    visible_size: NSSize,
) -> Retained<NSAlert> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    for button in buttons {
        alert.addButtonWithTitle(&NSString::from_str(button));
    }

    // Keep short alerts' native sizing. Switch to a scrollable body only when
    // AppKit's actual layout exceeds the comfortable, screen-bounded height.
    alert.setInformativeText(&NSString::from_str(text));
    alert.layout();
    let max_height = (visible_size.height - 40.0).min(600.0).max(1.0);
    let max_width = (visible_size.width - 40.0).max(1.0);
    if alert.window().frame().size.height <= max_height
        && alert.window().frame().size.width <= max_width
    {
        return alert;
    }

    alert.setInformativeText(&NSString::from_str(""));
    let size = NSSize::new((max_width - 120.0).clamp(1.0, 560.0), 320.0);
    let scroll =
        NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(NSPoint::ZERO, size));
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(false);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    let body = NSTextView::initWithFrame(
        NSTextView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, scroll.contentSize()),
    );
    body.setEditable(false);
    body.setSelectable(true);
    body.setRichText(false);
    body.setDrawsBackground(false);
    body.setFont(Some(&NSFont::systemFontOfSize(
        NSFont::smallSystemFontSize(),
    )));
    body.setMinSize(NSSize::ZERO);
    body.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
    body.setVerticallyResizable(true);
    body.setHorizontallyResizable(false);
    body.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    // The initializer owns the text system; configure wrapping, not truncation.
    if let Some(container) = unsafe { body.textContainer() } {
        container.setContainerSize(NSSize::new(scroll.contentSize().width, f64::MAX));
        container.setWidthTracksTextView(true);
    }
    body.setString(&NSString::from_str(text));
    scroll.setDocumentView(Some(&body));
    body.sizeToFit();
    scroll.setFrameSize(NSSize::new(
        size.width,
        body.frame().size.height.min(size.height),
    ));
    alert.setAccessoryView(Some(&scroll));
    alert.layout();

    // Account for actual AppKit chrome and button layout, rather than assuming
    // a fixed title/button height on every display or OS version.
    let overflow = (alert.window().frame().size.height - max_height).max(0.0);
    if overflow > 0.0 {
        scroll.setFrameSize(NSSize::new(
            size.width,
            (scroll.frame().size.height - overflow).max(1.0),
        ));
        alert.layout();
    }
    body.scrollRangeToVisible(NSRange::new(0, 0));
    // Retain NSAlert's first-button Return equivalent and initial button focus.
    // The read-only body remains selectable/copyable when the user focuses it.
    if let Some(button) = alert.buttons().firstObject() {
        alert.window().setInitialFirstResponder(Some(&button));
    }
    alert
}
