//! Phone presentation. UIKit owns layout, scrolling, assistive controls and event delivery.
//! The replica owns its observations. A scene reads their immutable snapshots and asks a
//! background reader only for the bounded collection pages its table actually displays.
mod art;
mod cells;
#[cfg(feature = "synthetic-proof")]
mod fixture;
mod model;

use crate::application::{ModelSource, volume_capacity};
use art::{Canvas, Drawing, rect};
use model::{Model, Screen};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::ProtocolObject, sel,
};
use objc2_core_foundation::{CGRect, CGSize};
use objc2_foundation::{NSArray, NSIndexPath, NSObjectProtocol, NSString, NSTimer};
use objc2_ui_kit::*;
use std::{
    cell::{Cell, RefCell},
    sync::Arc,
    time::{Duration, Instant},
};

fn copy(text: &str) {
    unsafe { UIPasteboard::generalPasteboard().setString(Some(&NSString::from_str(text))) }
}
fn set_font(label: &UILabel, body: bool) {
    unsafe {
        label.setFont(Some(&UIFont::preferredFontForTextStyle(if body {
            UIFontTextStyleBody
        } else {
            UIFontTextStyleCaption1
        })))
    }
    label.setAdjustsFontForContentSizeCategory(true);
    label.setNumberOfLines(0);
}
fn label(mtm: MainThreadMarker) -> Retained<UILabel> {
    let l = UILabel::new(mtm);
    set_font(&l, true);
    l
}
fn button(mtm: MainThreadMarker) -> Retained<UIButton> {
    UIButton::buttonWithType(UIButtonType::System, mtm)
}
fn title(b: &UIButton, s: &str) {
    b.setTitle_forState(Some(&NSString::from_str(s)), UIControlState::Normal);
}
fn text(l: &UILabel, s: &str) {
    if !l.text().is_some_and(|v| v.to_string() == s) {
        l.setText(Some(&NSString::from_str(s)));
    }
}

struct Controls {
    heading: Retained<UILabel>,
    header: Retained<UILabel>,
    action: Retained<UIButton>,
    notice: Retained<UIButton>,
    table: Retained<UITableView>,
    crown: Retained<Canvas>,
    chart: Retained<Canvas>,
    progress: Retained<Canvas>,
    fixture: Retained<UILabel>,
}
impl Controls {
    fn new(mtm: MainThreadMarker) -> Self {
        let heading = label(mtm);
        unsafe {
            heading.setFont(Some(&UIFont::preferredFontForTextStyle(
                UIFontTextStyleTitle2,
            )));
            heading.setAccessibilityTraits(UIAccessibilityTraitHeader, mtm);
        }
        let crown = Canvas::create(
            mtm,
            Drawing::Crown {
                origin: Instant::now(),
                activity: None,
                reduced: false,
                attention: false,
            },
            "Erlkonig crown",
        );
        let chart = Canvas::create(mtm, Drawing::Storage(vec![]), "Phone storage");
        let progress = Canvas::create(mtm, Drawing::Progress(0.), "Local pile replay");
        let table = UITableView::initWithFrame_style(
            UITableView::alloc(mtm),
            CGRect::ZERO,
            UITableViewStyle::Plain,
        );
        table.setBackgroundColor(Some(&UIColor::blackColor()));
        table.setEstimatedRowHeight(72.);
        unsafe {
            table.setRowHeight(UITableViewAutomaticDimension);
        }
        let fixture = label(mtm);
        set_font(&fixture, false);
        fixture.setTextAlignment(NSTextAlignment::Center);
        unsafe {
            fixture.setTextColor(Some(&UIColor::colorWithWhite_alpha(1., 0.65)));
        }
        text(&fixture, "Visual check · no replica");
        Self {
            heading,
            header: label(mtm),
            action: button(mtm),
            notice: button(mtm),
            table,
            crown,
            chart,
            progress,
            fixture,
        }
    }
}
struct State {
    model: RefCell<Model>,
    ui: Controls,
    status_timer: RefCell<Option<Retained<NSTimer>>>,
    animation_timer: RefCell<Option<Retained<NSTimer>>>,
    painted: Cell<Option<(u64, bool)>>,
    chart_key: Cell<Option<(Option<(u64, u64)>, usize, bool)>>,
    progress_key: Cell<Option<(bool, u64, u64)>>,
    crown_key: Cell<Option<(Option<erlkonig_crown::WorkActivity>, bool, bool)>>,
}

define_class!(
    #[unsafe(super(UIViewController))]
    #[name = "ErlkonigPhoneController"]
    #[thread_kind = MainThreadOnly]
    #[ivars = State]
    struct Phone;
    unsafe impl NSObjectProtocol for Phone {}
    unsafe impl UIScrollViewDelegate for Phone {}
    unsafe impl UITableViewDelegate for Phone {
        #[unsafe(method(tableView:didSelectRowAtIndexPath:))]
        fn tableView_didSelectRowAtIndexPath(&self, table: &UITableView, path: &NSIndexPath) {
            self.select(path);
            table.deselectRowAtIndexPath_animated(path, true);
        }
    }
    unsafe impl UITableViewDataSource for Phone {
        #[unsafe(method(numberOfSectionsInTableView:))]
        fn numberOfSectionsInTableView(&self, _table: &UITableView) -> isize {
            3
        }
        #[unsafe(method(tableView:numberOfRowsInSection:))]
        fn tableView_numberOfRowsInSection(&self, _table: &UITableView, section: isize) -> isize {
            self.ivars().model.borrow().number_of_rows(section)
        }
        #[unsafe(method_id(tableView:cellForRowAtIndexPath:))]
        fn tableView_cellForRowAtIndexPath(
            &self,
            table: &UITableView,
            path: &NSIndexPath,
        ) -> Retained<UITableViewCell> {
            self.cell(table, path)
        }
    }
    impl Phone {
        #[unsafe(method(viewDidLoad))]
        fn loaded(&self) {
            let _: () = unsafe { msg_send![super(self), viewDidLoad] };
            let root = self.view().unwrap();
            let u = &self.ivars().ui;
            root.setBackgroundColor(Some(&UIColor::blackColor()));
            self.setOverrideUserInterfaceStyle(UIUserInterfaceStyle::Dark);
            for v in [
                &*u.crown as &UIView,
                &u.table,
                &u.header,
                &u.chart,
                &u.notice,
                &u.action,
                &u.fixture,
                &u.heading,
                &u.progress,
            ] {
                root.addSubview(v);
            }
            u.table.setDataSource(Some(ProtocolObject::from_ref(self)));
            unsafe {
                u.table.setDelegate(Some(ProtocolObject::from_ref(self)));

                u.action.addTarget_action_forControlEvents(
                    Some(self),
                    sel!(action:),
                    UIControlEvents::TouchUpInside,
                );
                u.notice.addTarget_action_forControlEvents(
                    Some(self),
                    sel!(copyError:),
                    UIControlEvents::TouchUpInside,
                );
            }
            let actions: Vec<_> = [
                ("Show crown", sel!(showCrown:)),
                ("Show collections", sel!(showCollections:)),
                ("Show peers", sel!(showPeers:)),
            ]
            .into_iter()
            .map(|(name, selector)| unsafe {
                UIAccessibilityCustomAction::initWithName_target_selector(
                    UIAccessibilityCustomAction::alloc(self.mtm()),
                    &NSString::from_str(name),
                    Some(self),
                    selector,
                )
            })
            .collect();
            let actions = NSArray::from_retained_slice(&actions);
            for view in [&*u.crown as &UIView, &u.heading, &root] {
                view.setAccessibilityCustomActions(Some(&actions), self.mtm());
            }
            for dir in [
                UISwipeGestureRecognizerDirection::Left,
                UISwipeGestureRecognizerDirection::Right,
            ] {
                let gesture = unsafe {
                    UISwipeGestureRecognizer::initWithTarget_action(
                        UISwipeGestureRecognizer::alloc(self.mtm()),
                        Some(self),
                        Some(sel!(swipe:)),
                    )
                };
                gesture.setDirection(dir);
                root.addGestureRecognizer(&gesture);
                // A table's pan must not consume a horizontal page change. Vertical
                // motion fails each horizontal swipe recognizer and scrolls normally.
                u.table.panGestureRecognizer().requireGestureRecognizerToFail(&gesture);
            }
            self.poll();
        }
        #[unsafe(method(viewDidAppear:))]
        fn appeared(&self, animated: bool) {
            let _: () = unsafe { msg_send![super(self),viewDidAppear:animated] };
            self.start_timer();
            self.poll();
        }
        #[unsafe(method(viewDidDisappear:))]
        fn disappeared(&self, animated: bool) {
            let _: () = unsafe { msg_send![super(self),viewDidDisappear:animated] };
            if let Some(t) = self.ivars().status_timer.take() {
                t.invalidate();
            }
            self.animate(false);
        }
        #[unsafe(method(viewDidLayoutSubviews))]
        fn laid_out(&self) {
            let _: () = unsafe { msg_send![super(self), viewDidLayoutSubviews] };
            self.layout();
        }
        #[unsafe(method(tick:))]
        fn status_tick(&self, _timer: &NSTimer) {
            self.poll();
        }
        #[unsafe(method(drawCrown:))]
        fn crown_tick(&self, _timer: &NSTimer) {
            let m = self.ivars().model.borrow();
            let animate = m.foreground
                && m.screen == Screen::Crown
                && !m.reduced()
                && !UIAccessibilityIsReduceMotionEnabled()
                && erlkonig_crown::frame(
                    m.began.elapsed(),
                    m.activity,
                    false,
                    m.status.error.is_some(),
                )
                .animating;
            self.ivars().ui.crown.setNeedsDisplay();
            if !animate {
                self.animate(false);
            }
        }
        #[unsafe(method(showCrown:))]
        fn show_crown(&self, _: &UIAccessibilityCustomAction) -> bool {
            self.show(0);
            true
        }
        #[unsafe(method(showCollections:))]
        fn show_collections(&self, _: &UIAccessibilityCustomAction) -> bool {
            self.show(1);
            true
        }
        #[unsafe(method(showPeers:))]
        fn show_peers(&self, _: &UIAccessibilityCustomAction) -> bool {
            self.show(2);
            true
        }
        #[unsafe(method(accessibilityScroll:))]
        fn voiceover_scroll(&self, direction: UIAccessibilityScrollDirection) -> bool {
            match direction {
                UIAccessibilityScrollDirection::Left | UIAccessibilityScrollDirection::Next => {
                    self.step(1)
                }
                UIAccessibilityScrollDirection::Right | UIAccessibilityScrollDirection::Previous => {
                    self.step(-1)
                }
                _ => false,
            }
        }
        #[unsafe(method(swipe:))]
        fn swipe(&self, gesture: &UISwipeGestureRecognizer) {
            self.step(
                if gesture.direction() == UISwipeGestureRecognizerDirection::Left {
                    1
                } else {
                    -1
                },
            );
        }
        #[unsafe(method(action:))]
        fn action(&self, _button: &UIButton) {
            let mut m = self.ivars().model.borrow_mut();
            match m.screen {
                Screen::Crown if !m.status.node_id.is_empty() => {
                    copy(&m.status.node_id);
                    m.copied = Some(Instant::now());
                }
                Screen::Collections => m.discover(),
                _ => {}
            }
            drop(m);
            self.refresh();
        }
        #[unsafe(method(copyError:))]
        fn copy_error(&self, _b: &UIButton) {
            if let Some(error) = self.ivars().model.borrow().error() {
                copy(error);
            }
        }
        #[unsafe(method(toggle:))]
        fn toggle(&self, s: &UISwitch) {
            self.change(s.accessibilityIdentifier(), s.isOn());
        }
        #[unsafe(method(resolve:))]
        fn resolve(&self, b: &UIButton) {
            self.change(b.accessibilityIdentifier(), b.tag() != 0);
        }
    }
);
impl Phone {
    fn change(&self, id: Option<Retained<NSString>>, enabled: bool) {
        let Some(id) = id else { return };
        let root = id.to_string();
        let mut m = self.ivars().model.borrow_mut();
        let selected = m.row_for_root(&root).cloned();
        if let Some(row) = selected {
            m.choose(&row, enabled);
        }
        drop(m);
        self.refresh();
    }
    fn start_timer(&self) {
        let timer = &self.ivars().status_timer;
        if timer.borrow().is_none() {
            let t = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    0.25,
                    self,
                    sel!(tick:),
                    None,
                    true,
                )
            };
            *timer.borrow_mut() = Some(t);
        }
    }
    fn animate(&self, active: bool) {
        let timer = &self.ivars().animation_timer;
        if active && timer.borrow().is_none() {
            let t = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    1. / 30.,
                    self,
                    sel!(drawCrown:),
                    None,
                    true,
                )
            };
            *timer.borrow_mut() = Some(t);
        } else if !active {
            if let Some(t) = timer.take() {
                t.invalidate();
            }
        }
    }
    fn step(&self, delta: isize) -> bool {
        let index = self.ivars().model.borrow().screen as isize;
        let next = index + delta;
        if !(0..3).contains(&next) {
            return false;
        }
        self.show(next);
        true
    }
    fn show(&self, index: isize) {
        let screen = Screen::from_index(index);
        if self.ivars().model.borrow().screen == screen {
            return;
        }

        self.ivars().model.borrow_mut().set_screen(screen);
        self.ivars()
            .ui
            .table
            .setContentOffset_animated(objc2_core_foundation::CGPoint::ZERO, false);
        self.poll();
        if UIAccessibilityIsVoiceOverRunning() {
            let u = &self.ivars().ui;
            let target: &UIView = if screen == Screen::Crown {
                &u.crown
            } else {
                &u.heading
            };
            unsafe {
                UIAccessibilityPostNotification(
                    UIAccessibilityScreenChangedNotification,
                    Some(target),
                );
            }
        }
    }
    fn poll(&self) {
        self.ivars().model.borrow_mut().tick();
        self.refresh();
        if self.ivars().model.borrow().screen.list() {
            let paths = self.ivars().ui.table.indexPathsForVisibleRows();
            let indices: Vec<_> = paths
                .iter()
                .flat_map(|p| p.iter())
                .filter(|p| p.section() == 1)
                .map(|p| p.row() as usize)
                .collect();
            let mut model = self.ivars().model.borrow_mut();
            if model.foreground {
                model.visible_pages(indices.iter().copied());
                let roots = indices.into_iter().filter_map(|n| model.row(n).map(|r| r.root.clone())).collect();
                model.storage_roots(roots);
            }
        }
    }
    fn refresh(&self) {
        if !self.isViewLoaded() {
            return;
        }
        let st = self.ivars();
        let u = &st.ui;
        let m = st.model.borrow();
        let screen = m.screen;
        u.heading.setHidden(screen == Screen::Crown);
        text(
            &u.heading,
            match screen {
                Screen::Crown => "",
                Screen::Collections => "Collections",
                Screen::Peers => "Peers",
            },
        );
        u.crown.setHidden(screen != Screen::Crown);
        u.table.setHidden(screen == Screen::Crown);
        u.chart.setHidden(screen != Screen::Collections);
        u.fixture.setHidden(!m.visual());
        u.header
            .setHidden(!matches!(screen, Screen::Collections | Screen::Peers));
        if screen == Screen::Peers {
            let phase = m.phase();
            if !m.status.replay_complete && m.foreground && m.status.error.is_none() {
                text(&u.header, &format!("Peer connections unavailable during startup\n{phase}"));
            } else {
                text(&u.header, &phase);
            }
        }
        if screen == Screen::Collections {
            let summary = m
                .capacity
                .map(|(t, f)| {
                    format!(
                        "iPhone     {} of {} used\n{} available",
                        model::bytes(t - f),
                        model::bytes(t),
                        model::bytes(f)
                    )
                })
                .unwrap_or_else(|| "iPhone     Capacity unavailable".into());
            if !m.status.replay_complete && m.foreground && m.status.error.is_none() {
                text(&u.header, &format!("{summary}\n{}", m.phase()));
            } else {
                text(&u.header, &summary);
            }
            let key = (
                m.capacity,
                m.status
                    .composition
                    .as_ref()
                    .map_or(0, |c| Arc::as_ptr(c) as usize),
                m.list_status().replay_complete,
            );
            if st.chart_key.replace(Some(key)) != Some(key) {
                let mut parts = vec![];
                let mut value = "Unavailable".into();
                if let Some((total, free)) = m.capacity {
                    value = format!(
                        "{} bytes used, {} bytes available, {} bytes total",
                        total - free,
                        free,
                        total
                    );
                    if let Some((c, other)) = m
                        .list_status()
                        .composition
                        .as_ref()
                        .filter(|_| m.list_status().replay_complete)
                        .and_then(|c| art::other_used(total, free, c).map(|o| (c, o)))
                    {
                        for s in &c.segments {
                            if s.bytes > 0 {
                                parts.push((
                                    s.bytes as f64 / total as f64,
                                    art::collection_color(&s.root),
                                ));
                            }
                        }
                        parts.push((c.shared_bytes as f64 / total as f64, 0x888888));
                        parts.push((other as f64 / total as f64, 0x484848));
                    } else {
                        parts.push(((total - free) as f64 / total as f64, 0x888888));
                    }
                    parts.push((free as f64 / total as f64, 0x242424));
                }
                u.chart.update(Drawing::Storage(parts));
                u.chart
                    .setAccessibilityValue(Some(&NSString::from_str(&value)), self.mtm());
                u.chart.setAccessibilityHint(Some(&NSString::from_str(m.volume_error.as_deref().unwrap_or("Known local content. Shared counts overlapping inventories. Other used includes system and unmeasured data."))),self.mtm());
            }
        }
        let discover = screen == Screen::Collections
            && m.list_status().replay_complete
            && !m.list_status().listening
            && m.list_status().selected == 0;
        u.action
            .setHidden(!((screen == Screen::Crown && !m.visual()) || discover));
        u.notice
            .setHidden(screen != Screen::Crown || m.error().is_none());
        if screen == Screen::Crown {
            if let Some(e) = m.error() {
                u.notice
                    .setTitleColor_forState(Some(&art::rgb(0xff9f0a)), UIControlState::Normal);
                title(&u.notice, e);
                u.notice
                    .setAccessibilityHint(Some(&NSString::from_str("Copy error")), self.mtm());
            }
            if !m.visual() {
                let id = &m.status.node_id;
                let displayed = if m
                    .copied
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
                {
                    "Copied".into()
                } else if id.is_empty() {
                    "Preparing node ID…".into()
                } else if id.len() == 64 && id.is_ascii() {
                    format!("{}\n{}", &id[..32], &id[32..])
                } else {
                    id.clone()
                };
                u.action.setTitleColor_forState(
                    Some(&UIColor::colorWithWhite_alpha(1., 0.75)),
                    UIControlState::Normal,
                );
                u.action.setEnabled(!id.is_empty());
                title(&u.action, &displayed);
                if let Some(label) = u.action.titleLabel() {
                    label.setNumberOfLines(2);
                    label.setTextAlignment(NSTextAlignment::Center);
                    unsafe {
                        let size =
                            UIFont::preferredFontForTextStyle(UIFontTextStyleCaption1).pointSize();
                        label.setFont(Some(&UIFont::monospacedSystemFontOfSize_weight(
                            size,
                            UIFontWeightRegular,
                        )));
                    }
                }
                u.action
                    .setAccessibilityLabel(Some(&NSString::from_str("Copy node ID")), self.mtm());
            }
            let crown_key = (
                m.activity,
                m.reduced() || UIAccessibilityIsReduceMotionEnabled(),
                m.status.error.is_some(),
            );
            if st.crown_key.replace(Some(crown_key)) != Some(crown_key) {
                u.crown.update(Drawing::Crown {
                    origin: m.began,
                    activity: m.activity,
                    reduced: m.reduced(),
                    attention: m.status.error.is_some(),
                });
            }
        } else if discover {
            u.action
                .setTitleColor_forState(None, UIControlState::Normal);
            title(&u.action, "Discover collections");
            u.action.setAccessibilityLabel(
                Some(&NSString::from_str("Discover collections")),
                self.mtm(),
            );
            u.action.setHidden(false);
            u.action.setEnabled(m.can_choose());
        }
        let can_animate = screen == Screen::Crown
            && m.foreground
            && m.activity.is_some()
            && !m.reduced()
            && !UIAccessibilityIsReduceMotionEnabled();
        self.animate(
            can_animate
                && erlkonig_crown::frame(
                    m.began.elapsed(),
                    m.activity,
                    false,
                    m.status.error.is_some(),
                )
                .animating,
        );
        let progress = m.progress().map(|(a, b)| (m.replaying(), a, b));
        if st.progress_key.replace(progress) != progress {
            if let Some((replay, a, b)) = progress {
                u.progress.update(Drawing::Progress(if b > 0 {
                    a as f64 / b as f64
                } else {
                    0.
                }));
                u.progress.setAccessibilityLabel(
                    Some(&NSString::from_str(if replay {
                        "Local pile replay"
                    } else {
                        "Collection record synchronization"
                    })),
                    self.mtm(),
                );
                u.progress.setAccessibilityValue(
                    Some(&NSString::from_str(&format!(
                        "{a} of {b} {}",
                        if replay {
                            "bytes"
                        } else {
                            "known collection records"
                        }
                    ))),
                    self.mtm(),
                );
            }
        }
        u.progress.setHidden(progress.is_none());
        let key = (m.table_epoch, screen != Screen::Crown && m.fresh_network());
        let reload = st.painted.replace(Some(key)) != Some(key) && screen != Screen::Crown;
        drop(m);
        if reload {
            u.table.reloadData();
        }
        self.layout();
    }
    fn layout(&self) {
        let Some(root) = self.view() else { return };
        let u = &self.ivars().ui;
        let bounds = root.bounds();
        let i = root.safeAreaInsets();
        let margin = if self.ivars().model.borrow().screen.list() {
            20.
        } else {
            16.
        };
        let x = i.left + margin;
        let w = (bounds.size.width - x - i.right - margin).max(0.);
        let mut y = i.top + 6.;
        let mut bottom = (bounds.size.height - i.bottom).max(y);
        if !u.heading.isHidden() {
            y += 8.;
            let h = u
                .heading
                .sizeThatFits(CGSize::new(w, f64::MAX))
                .height
                .max(28.);
            u.heading.setFrame(rect(x, y, w, h));
            y += h + 12.;
        }
        if !u.fixture.isHidden() {
            bottom -= 30.;
            u.fixture.setFrame(rect(x, bottom, w, 30.));
        }
        if !u.header.isHidden() {
            let h = u
                .header
                .sizeThatFits(CGSize::new(w, f64::MAX))
                .height
                .max(26.);
            u.header.setFrame(rect(x, y, w, h));
            y += h + 8.;
        }
        if !u.chart.isHidden() {
            u.chart.setFrame(rect(x, y, w, 24.));
            y += 36.;
        }
        if !u.action.isHidden() {
            if !u.crown.isHidden() {
                bottom -= 54.;
                u.action.setFrame(rect(x, bottom, w, 54.));
            } else {
                u.action.setFrame(rect(x, y, w, 44.));
                y += 46.;
            }
        }
        if !u.notice.isHidden() {
            bottom -= 50.;
            u.notice.setFrame(rect(x, bottom, w, 50.));
        }
        let body = rect(x, y, w, bottom - y);
        u.table.setFrame(body);
        u.crown.setFrame(rect(i.left, y, bounds.size.width - i.left - i.right, bottom - y));
        u.progress.setFrame(bounds);
    }
}
pub(crate) fn make_root(
    mtm: MainThreadMarker,
    source: crate::application::ModelSource,
) -> Retained<UIViewController> {
    let this = Phone::alloc(mtm).set_ivars(State {
        model: RefCell::new(Model::new(source)),
        ui: Controls::new(mtm),
        status_timer: Default::default(),
        animation_timer: Default::default(),
        painted: Cell::new(None),
        chart_key: Cell::new(None),
        progress_key: Cell::new(None),
        crown_key: Cell::new(None),
    });
    let this: Retained<Phone> = unsafe { msg_send![super(this), init] };
    Retained::into_super(this)
}
pub(crate) fn set_foreground(root: &UIViewController, active: bool) {
    if let Some(phone) = root.downcast_ref::<Phone>() {
        phone.ivars().model.borrow_mut().set_active(active);
        phone.poll();
    }
}
pub(crate) fn pause_settled(root: &UIViewController) -> bool {
    root.downcast_ref::<Phone>().is_none_or(|p| {
        let mut m = p.ivars().model.borrow_mut();
        m.tick();
        m.acknowledged() || m.status.error.is_some() || m.visual()
    })
}
