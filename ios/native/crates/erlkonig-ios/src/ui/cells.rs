use super::{
    Phone,
    art::{collection_color, other_used, rect, rgb},
    model::{Model, Screen},
};
use crate::collections::Row;
use block2::RcBlock;
use objc2::{DefinedClass, MainThreadOnly, rc::Retained, sel};
use objc2::{MainThreadMarker, runtime::Bool};
use objc2_foundation::{NSArray, NSIndexPath, NSString};
use objc2_ui_kit::*;
use std::ptr::NonNull;

pub(super) fn bytes(n: u64) -> String {
    let (unit, scale) = if n >= 1_000_000_000_000 {
        ("TB", 1e12)
    } else if n >= 1_000_000_000 {
        ("GB", 1e9)
    } else if n >= 1_000_000 {
        ("MB", 1e6)
    } else if n >= 1000 {
        ("kB", 1e3)
    } else {
        return format!("{n} B");
    };
    format!("{:.1} {unit}", n as f64 / scale)
}
fn rate(v: Option<f64>) -> String {
    match v.filter(|v| v.is_finite() && *v >= 0.) {
        None => "—".into(),
        Some(v) if v >= 1e6 => format!("{:.1} MB/s", v / 1e6),
        Some(v) if v >= 1e3 => format!("{:.1} kB/s", v / 1e3),
        Some(v) => format!("{v:.0} B/s"),
    }
}
fn abbrev(s: &str) -> String {
    if s.len() > 24 && s.is_ascii() {
        format!("{}…{}", &s[..12], &s[s.len() - 8..])
    } else {
        s.into()
    }
}
pub(super) enum Item {
    Text(String, String),
    Copy(String, String, String),
    Choice(Row),
    Volume(String, String, u32),
    Loading,
}
impl Model {
    fn preface(&self) -> Vec<Item> {
        let mut items = vec![];
        match self.screen {
            Screen::Collections | Screen::Storage => {
                if let Some(e) = self.error() {
                    items.push(Item::Copy(e.into(), "Copy error".into(), e.into()));
                }
                if self.screen == Screen::Storage {
                    items.push(Item::Text("Local data".into(),"Resident content can belong to several collections. Record counts describe direct blobs.".into()));
                }
            }
            _ => {}
        }
        items
    }
    fn footer(&self) -> Vec<Item> {
        let mut items = vec![];
        if self.list_loaded && self.total == 0 && self.list_status().replay_complete {
            items.push(Item::Text(
                if self.screen == Screen::Collections && self.list_status().listening {
                    "Listening for collection access"
                } else {
                    "No available collections"
                }
                .into(),
                String::new(),
            ));
        } else if self.total == 0
            && (!self.list_loaded || !self.list_status().replay_complete)
            && self.error().is_none()
        {
            items.push(Item::Loading);
        }
        if self.screen == Screen::Collections
            && self.access_pending > 0
            && self.list_status().replay_complete
        {
            items.push(Item::Text(
                "Some access metadata is unavailable".into(),
                String::new(),
            ));
        }
        if self.screen == Screen::Storage {
            if let Some((total, free)) = self.capacity {
                match self
                    .list_status()
                    .composition
                    .as_ref()
                    .filter(|_| self.list_status().replay_complete)
                    .and_then(|c| other_used(total, free, c).map(|other| (c.shared_bytes, other)))
                {
                    Some((shared, other)) => {
                        if shared > 0 {
                            items.push(Item::Volume("Shared".into(), bytes(shared), 0x888888))
                        }
                        if other > 0 {
                            items.push(Item::Volume("Other used".into(), bytes(other), 0x484848))
                        }
                    }
                    None if total > free => {
                        items.push(Item::Volume("Used".into(), bytes(total - free), 0x888888))
                    }
                    _ => {}
                }
            }
        }
        items
    }
    pub fn number_of_rows(&self, section: isize) -> isize {
        let count = if self.screen == Screen::Sync {
            if section == 1 && self.acknowledged() && self.foreground && self.status.error.is_none()
            {
                self.status
                    .network
                    .as_ref()
                    .map_or(0, |n| n.connections.len())
            } else {
                0
            }
        } else if self.screen.list() {
            match section {
                0 => self.preface().len(),
                1 => self.total,
                2 => self.footer().len(),
                _ => 0,
            }
        } else {
            0
        };
        count.try_into().unwrap_or(isize::MAX)
    }
    pub fn item(&self, section: isize, index: usize) -> Item {
        if section == 0 {
            return self
                .preface()
                .into_iter()
                .nth(index)
                .unwrap_or(Item::Loading);
        }
        if section == 2 {
            return self
                .footer()
                .into_iter()
                .nth(index)
                .unwrap_or(Item::Loading);
        }
        if self.screen == Screen::Sync {
            let Some(c) = self
                .status
                .network
                .as_ref()
                .and_then(|n| n.connections.get(index))
            else {
                return Item::Loading;
            };
            let fresh = self.fresh_network();
            let rx = rate(c.receive_bytes_per_second.filter(|_| fresh));
            let tx = rate(c.send_bytes_per_second.filter(|_| fresh));
            let rtt = c
                .rtt_ms
                .filter(|_| fresh)
                .map_or_else(String::new, |v| format!("\nRTT ≈ {v:.0} ms"));
            return Item::Copy(
                abbrev(&c.peer),
                format!("↓ {rx}     ↑ {tx}{rtt}"),
                c.peer.clone(),
            );
        }
        let Some(row) = self.row(index) else {
            return Item::Loading;
        };
        if self.screen != Screen::Storage {
            return Item::Choice(row.clone());
        }
        let sample = self
            .list_status()
            .storage
            .iter()
            .find(|s| s.root == row.root)
            .filter(|_| self.list_status().replay_complete);
        let resident = sample
            .and_then(|s| s.resident_bytes)
            .map(bytes)
            .unwrap_or_else(|| {
                if sample.is_some_and(|s| s.pending) {
                    "Measuring…"
                } else {
                    "—"
                }
                .into()
            });
        let updating = if sample.is_some_and(|s| s.pending && s.resident_bytes.is_some()) {
            " · Updating"
        } else {
            ""
        };
        let records = row.counts.as_ref().map_or_else(
            || "Record counts unavailable".into(),
            |n| format!("{}/{} records", n.direct_present, n.records),
        );
        let detail = sample
            .and_then(|s| s.resident_error.as_deref())
            .or(row.count_error.as_deref())
            .map_or_else(String::new, |e| format!("\n{e}"));
        Item::Volume(
            row.name.clone(),
            format!("{resident}{updating}   ·   {records}{detail}"),
            collection_color(&row.root),
        )
    }
}
fn copy_action(view: &UIView, root: &str, mtm: MainThreadMarker) {
    let id = root.to_owned();
    let handler = RcBlock::new(move |_: NonNull<UIAccessibilityCustomAction>| {
        super::copy(&id);
        Bool::YES
    });
    let action = unsafe {
        UIAccessibilityCustomAction::initWithName_actionHandler(
            UIAccessibilityCustomAction::alloc(mtm),
            &NSString::from_str("Copy collection ID"),
            RcBlock::as_ptr(&handler),
        )
    };
    view.setAccessibilityCustomActions(Some(&NSArray::from_retained_slice(&[action])), mtm);
}
impl Phone {
    pub(super) fn cell(
        &self,
        table: &UITableView,
        path: &NSIndexPath,
    ) -> Retained<UITableViewCell> {
        let mtm = self.mtm();
        let index = path.row() as usize;
        let key = NSString::from_str("replica-row");
        let cell = table
            .dequeueReusableCellWithIdentifier(&key)
            .unwrap_or_else(|| {
                UITableViewCell::initWithStyle_reuseIdentifier(
                    UITableViewCell::alloc(mtm),
                    UITableViewCellStyle::Subtitle,
                    Some(&key),
                )
            });
        cell.setAccessoryView(None);
        cell.setAccessibilityCustomActions(None, mtm);
        cell.setSelectionStyle(UITableViewCellSelectionStyle::None);
        cell.setBackgroundColor(Some(&rgb(0x121212)));
        cell.setIsAccessibilityElement(true, mtm);
        cell.setAccessibilityLabel(None, mtm);
        cell.setAccessibilityValue(None, mtm);
        cell.setAccessibilityHint(None, mtm);
        let section = path.section();
        let item = {
            let mut model = self.ivars().model.borrow_mut();
            if section == 1 && model.screen.list() && model.row(index).is_none() {
                model.demand(index);
            }
            model.item(section, index)
        };
        let (title, detail) = match item {
            Item::Loading => ("Loading collection…".into(), String::new()),
            Item::Text(name, detail) => (name, detail),
            Item::Copy(name, detail, value) => {
                cell.setSelectionStyle(UITableViewCellSelectionStyle::Default);
                let hint = if self.ivars().model.borrow().screen == Screen::Sync {
                    format!(
                        "Copy node ID {value}. Network traffic includes overhead and retransmissions."
                    )
                } else {
                    "Copy error".into()
                };
                cell.setAccessibilityHint(Some(&NSString::from_str(&hint)), mtm);
                (name, detail)
            }
            Item::Volume(name, detail, color) => {
                let swatch = UIView::initWithFrame(UIView::alloc(mtm), rect(0., 0., 12., 12.));
                swatch.setBackgroundColor(Some(&rgb(color)));
                swatch.setIsAccessibilityElement(false, mtm);
                cell.setAccessoryView(Some(&swatch));
                (name, detail)
            }
            Item::Choice(row) => {
                let model = self.ivars().model.borrow();
                let on = model.desired(&row);
                let change = model.accepts_intent();
                let mut detail = abbrev(&row.root);
                copy_action(&cell, &row.root, mtm);
                if model.list_status().replay_complete && !row.readable {
                    detail += "\nRead access unavailable";
                }
                let hint = format!(
                    "Collection {}. Tap the row to copy its ID. {}",
                    row.root,
                    if model.list_status().replay_complete {
                        "Turning sync off keeps saved data."
                    } else {
                        "Changes require complete replay."
                    }
                );
                cell.setAccessibilityHint(Some(&NSString::from_str(&hint)), mtm);
                cell.setSelectionStyle(UITableViewCellSelectionStyle::Default);
                if row.state == "unknown" {
                    detail += "\nSelection unknown in this snapshot";
                } else if row.conflicted {
                    detail += "\nConflicting choices · sync is off";
                    let stack = UIStackView::initWithFrame(
                        UIStackView::alloc(mtm),
                        rect(0., 0., 126., 44.),
                    );
                    stack.setSpacing(8.);
                    for (title, enabled) in [("Sync", true), ("Keep off", false)] {
                        let b = UIButton::buttonWithType(UIButtonType::System, mtm);
                        b.setTitle_forState(
                            Some(&NSString::from_str(title)),
                            UIControlState::Normal,
                        );
                        b.setEnabled(change && (!enabled || row.readable));
                        b.setTag(enabled as isize);
                        copy_action(&b, &row.root, mtm);
                        b.setAccessibilityIdentifier(Some(&NSString::from_str(&row.root)));
                        b.setAccessibilityLabel(
                            Some(&NSString::from_str(&format!("{title} {}", row.name))),
                            mtm,
                        );
                        unsafe {
                            b.addTarget_action_forControlEvents(
                                Some(self),
                                sel!(resolve:),
                                UIControlEvents::TouchUpInside,
                            )
                        };
                        stack.addArrangedSubview(&b);
                    }
                    cell.setIsAccessibilityElement(false, mtm);
                    cell.setAccessoryView(Some(&stack));
                } else {
                    let toggle = UISwitch::new(mtm);
                    toggle.setOn(on);
                    copy_action(&toggle, &row.root, mtm);
                    toggle.setEnabled(change && (row.readable || on));
                    toggle.setAccessibilityIdentifier(Some(&NSString::from_str(&row.root)));
                    toggle.setAccessibilityLabel(
                        Some(&NSString::from_str(&format!("Sync {}", row.name))),
                        mtm,
                    );
                    toggle.setAccessibilityValue(
                        Some(&NSString::from_str(if row.state == "unset" {
                            "Unset in this snapshot"
                        } else if on {
                            "On"
                        } else {
                            "Off"
                        })),
                        mtm,
                    );
                    toggle.setAccessibilityHint(Some(&NSString::from_str(&hint)), mtm);
                    unsafe {
                        toggle.addTarget_action_forControlEvents(
                            Some(self),
                            sel!(toggle:),
                            UIControlEvents::ValueChanged,
                        )
                    };
                    cell.setIsAccessibilityElement(false, mtm);
                    cell.setAccessoryView(Some(&toggle));
                }
                (row.name, detail)
            }
        };
        #[allow(deprecated)]
        if let Some(label) = cell.textLabel() {
            label.setText(Some(&NSString::from_str(&title)));
            super::set_font(&label, true);
        }
        #[allow(deprecated)]
        if let Some(label) = cell.detailTextLabel() {
            label.setText(Some(&NSString::from_str(&detail)));
            super::set_font(&label, false);
        }
        cell
    }
    pub(super) fn select(&self, path: &NSIndexPath) {
        let m = self.ivars().model.borrow();
        let value = match m.item(path.section(), path.row() as usize) {
            Item::Choice(row) => Some(row.root),
            Item::Copy(_, _, s) => Some(s),
            _ => None,
        };
        if let Some(value) = value {
            super::copy(&value);
        }
    }
}
