//! Session-bus plumbing: one instance per session, the tray host probe, the
//! colour-scheme setting and the desktop's URI and file-manager services.

use super::UserEvent;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use winit::event_loop::EventLoopProxy;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

pub(crate) const NAME: &str = "ai.erlkonig.app";
const PATH: &str = "/ai/erlkonig/app";
const INTERFACE: &str = "ai.erlkonig.App";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";
const FILE_MANAGER: &str = "org.freedesktop.FileManager1";
const FILE_MANAGER_PATH: &str = "/org/freedesktop/FileManager1";

/// The running instance's activation object: another launch asks it to show
/// its window instead of starting a second owner.
struct Activation(Mutex<EventLoopProxy<UserEvent>>);

#[zbus::interface(name = "ai.erlkonig.App")]
impl Activation {
    fn show(&self) {
        if let Ok(proxy) = self.0.lock() {
            let _ = proxy.send_event(UserEvent::Show);
        }
    }
}

pub(crate) enum Claim {
    /// This process owns the name; keep the connection for the session.
    Primary(Connection),
    /// Another instance owns the name and was asked to show its window.
    Shown,
    /// No usable session bus; run without single-instance activation.
    Unavailable(zbus::Error),
}

pub(crate) fn claim(proxy: EventLoopProxy<UserEvent>) -> Claim {
    // zbus requests names with AllowReplacement | ReplaceExisting by default,
    // so a second launch would take the name over and start a second owner.
    // Neither: the holder keeps the name and a later launch gets NameTaken.
    let built = zbus::blocking::connection::Builder::session()
        .and_then(|builder| builder.serve_at(PATH, Activation(Mutex::new(proxy))))
        .and_then(|builder| builder.name(NAME))
        .map(|builder| {
            builder
                .allow_name_replacements(false)
                .replace_existing_names(false)
        })
        .and_then(|builder| builder.build());
    match built {
        Ok(connection) => Claim::Primary(connection),
        Err(zbus::Error::NameTaken) => match Connection::session().and_then(|connection| {
            connection.call_method(Some(NAME), PATH, Some(INTERFACE), "Show", &())
        }) {
            Ok(_) => Claim::Shown,
            Err(error) => Claim::Unavailable(error),
        },
        Err(error) => Claim::Unavailable(error),
    }
}

/// Whether a StatusNotifierItem host can show the tray icon right now.
pub(crate) fn has_tray_host(connection: &Connection) -> bool {
    zbus::blocking::fdo::DBusProxy::new(connection)
        .and_then(|bus| {
            bus.name_has_owner(WATCHER.try_into().expect("valid bus name"))
                .map_err(zbus::Error::from)
        })
        .unwrap_or(false)
}

/// `Read` returns the value inside one more variant layer than `ReadOne`.
pub(crate) fn scheme_value(value: &Value<'_>) -> Option<u32> {
    match value {
        Value::U32(scheme) => Some(*scheme),
        Value::Value(inner) => scheme_value(inner),
        _ => None,
    }
}

/// The desktop's `color-scheme` preference, if a settings portal answers.
pub(crate) fn color_scheme(connection: &Connection) -> Option<u32> {
    let reply = connection
        .call_method(
            Some(PORTAL),
            PORTAL_PATH,
            Some(SETTINGS),
            "Read",
            &(APPEARANCE, COLOR_SCHEME),
        )
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    scheme_value(&value)
}

/// Deliver every later change of the preference on a small thread.
pub(crate) fn watch_color_scheme(
    connection: &Connection,
    on_change: impl Fn(Option<u32>) + Send + 'static,
) -> zbus::Result<()> {
    let proxy = zbus::blocking::Proxy::new(connection, PORTAL, PORTAL_PATH, SETTINGS)?;
    let signals = proxy.receive_signal("SettingChanged")?;
    std::thread::Builder::new()
        .name("color-scheme".into())
        .spawn(move || {
            let _proxy = proxy;
            for message in signals {
                let Ok((namespace, key, value)) =
                    message.body().deserialize::<(String, String, OwnedValue)>()
                else {
                    continue;
                };
                if namespace == APPEARANCE && key == COLOR_SCHEME {
                    on_change(scheme_value(&value));
                }
            }
        })
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    Ok(())
}

/// A `file://` URI with every byte outside the unreserved set percent-encoded.
pub(crate) fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

pub(crate) fn open_uri(connection: &Connection, uri: &str) -> zbus::Result<()> {
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    connection.call_method(
        Some(PORTAL),
        PORTAL_PATH,
        Some("org.freedesktop.portal.OpenURI"),
        "OpenURI",
        &("", uri, options),
    )?;
    Ok(())
}

/// `ShowItems` selects files in their folder; `ShowFolders` opens folders.
pub(crate) fn file_manager(connection: &Connection, method: &str, path: &Path) -> zbus::Result<()> {
    connection.call_method(
        Some(FILE_MANAGER),
        FILE_MANAGER_PATH,
        Some(FILE_MANAGER),
        method,
        &(vec![file_uri(path)], ""),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_escape_everything_outside_the_unreserved_set() {
        assert_eq!(
            file_uri(Path::new("/home/someone/.local/share/erlkonig")),
            "file:///home/someone/.local/share/erlkonig"
        );
        assert_eq!(
            file_uri(Path::new("/home/someone/My Files/100%/ä#?")),
            "file:///home/someone/My%20Files/100%25/%C3%A4%23%3F"
        );
    }

    #[test]
    fn read_and_read_one_replies_both_yield_the_scheme() {
        assert_eq!(scheme_value(&Value::U32(1)), Some(1));
        assert_eq!(
            scheme_value(&Value::Value(Box::new(Value::U32(2)))),
            Some(2)
        );
        assert_eq!(scheme_value(&Value::from("dark")), None);
    }
}
