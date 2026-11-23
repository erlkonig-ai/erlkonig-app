//! UIKit owns the process and scene lifecycle. Each connected scene retains its
//! native controller and gives the replica time to acknowledge a background pause.
use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyObject;
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send,
};
use objc2_foundation::{
    NSDictionary, NSFileManager, NSFileProtectionCompleteUntilFirstUserAuthentication,
    NSFileProtectionKey, NSNumber, NSObject, NSObjectProtocol, NSSearchPathDirectory,
    NSSearchPathDomainMask, NSString, NSTimer, NSURL, NSURLIsExcludedFromBackupKey,
    NSURLResourceKey, NSURLVolumeAvailableCapacityKey, NSURLVolumeTotalCapacityKey, ns_string,
};
use objc2_ui_kit::{
    UIApplication, UIApplicationDelegate, UIBackgroundTaskIdentifier, UIBackgroundTaskInvalid,
    UIScene, UISceneConnectionOptions, UISceneDelegate, UISceneSession, UIViewController, UIWindow,
    UIWindowScene, UIWindowSceneDelegate,
};

pub(crate) enum ModelSource {
    Live(PathBuf),
    Error(String),
    #[cfg(feature = "synthetic-proof")]
    Visual(Vec<String>),
    #[cfg(feature = "synthetic-proof")]
    Proof {
        reader: std::sync::mpsc::Receiver<Result<PathBuf, String>>,
    },
}

fn prepare_root() -> Result<PathBuf, String> {
    let manager = NSFileManager::defaultManager();
    let support = manager
        .URLForDirectory_inDomain_appropriateForURL_create_error(
            NSSearchPathDirectory::ApplicationSupportDirectory,
            NSSearchPathDomainMask::UserDomainMask,
            None,
            true,
        )
        .map_err(|error| error.to_string())?;
    let root = support
        .URLByAppendingPathComponent_isDirectory(ns_string!("ErlkonigReplica"), true)
        .ok_or("Application Support URL has no replica directory")?;
    // Foundation defines these keys and values as constant NSString objects.
    unsafe {
        let attributes = NSDictionary::from_slices(
            &[NSFileProtectionKey],
            &[NSFileProtectionCompleteUntilFirstUserAuthentication.as_ref()],
        );
        manager
            .createDirectoryAtURL_withIntermediateDirectories_attributes_error(
                &root,
                true,
                Some(&attributes),
            )
            .map_err(|error| error.to_string())?;
        root.setResourceValue_forKey_error(
            Some(&NSNumber::numberWithBool(true)),
            NSURLIsExcludedFromBackupKey,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(PathBuf::from(
        root.path()
            .ok_or("Replica URL has no file path")?
            .to_string(),
    ))
}

fn model_source() -> ModelSource {
    #[cfg(feature = "synthetic-proof")]
    let arguments: Vec<_> = std::env::args().collect();
    #[cfg(feature = "synthetic-proof")]
    if arguments
        .iter()
        .any(|arg| arg.starts_with("--crown-fixture=") || arg.starts_with("--collections-fixture"))
    {
        return ModelSource::Visual(arguments);
    }
    let root = match prepare_root() {
        Ok(path) => path,
        Err(error) => return ModelSource::Error(error),
    };
    #[cfg(feature = "synthetic-proof")]
    {
        let reopening = arguments
            .iter()
            .any(|arg| arg == "--reopen-synthetic-proof");
        if reopening || arguments.iter().any(|arg| arg == "--synthetic-proof") {
            let fixture = root.join("synthetic-proof");
            let (tx, reader) = std::sync::mpsc::channel();
            let launch = std::thread::Builder::new()
                .name("ios-proof-start".into())
                .spawn(move || {
                    let result = (|| -> anyhow::Result<PathBuf> {
                        if !reopening {
                            std::fs::create_dir(&fixture)?;
                        }
                        crate::proof::run_owned(fixture.clone(), reopening)?;
                        eprintln!("Synthetic replica proof passed (reopen={reopening})");
                        Ok(fixture.join("reader"))
                    })();
                    let _ = tx.send(result.map_err(|error| format!("{error:#}")));
                });
            return match launch {
                Ok(_) => ModelSource::Proof { reader },
                Err(error) => ModelSource::Error(error.to_string()),
            };
        }
    }
    ModelSource::Live(root)
}

/// Ordinary volume capacity for the app's own filesystem. The caller samples
/// off the main thread. A fresh file URL avoids Foundation's resource cache.
pub(crate) fn volume_capacity(path: &Path) -> Result<(u64, u64), String> {
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath_isDirectory(
            &NSString::from_str(path.to_str().ok_or("Non-UTF-8 volume path")?),
            true,
        );
        let capacity = |key: &NSURLResourceKey| -> Result<u64, String> {
            let mut value: Option<Retained<AnyObject>> = None;
            unsafe { url.getResourceValue_forKey_error(&mut value, key) }
                .map_err(|error| error.to_string())?;
            let number = value
                .and_then(|v| v.downcast::<NSNumber>().ok())
                .ok_or("Volume capacity unavailable")?;
            number
                .longLongValue()
                .try_into()
                .map_err(|_| "Invalid volume capacity".into())
        };
        let total = capacity(unsafe { NSURLVolumeTotalCapacityKey })?;
        let free = capacity(unsafe { NSURLVolumeAvailableCapacityKey })?;
        if total == 0 || free > total {
            return Err("Invalid volume capacity".into());
        }
        Ok((total, free))
    })
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "ErlkonigApplicationDelegate"]
    #[thread_kind = MainThreadOnly]
    struct ApplicationDelegate;
    unsafe impl NSObjectProtocol for ApplicationDelegate {}
    unsafe impl UIApplicationDelegate for ApplicationDelegate {}
);

#[derive(Default)]
struct SceneState {
    window: RefCell<Option<Retained<UIWindow>>>,
    grant: Cell<Option<UIBackgroundTaskIdentifier>>,
    generation: Cell<u64>,
    pause_timer: RefCell<Option<Retained<NSTimer>>>,
}

// This app supports one scene at a time. UIKit may nevertheless disconnect
// and replace it without ending the process. The controller owns the replica
// and must survive that handoff, including a pause that is still settling.
thread_local! {
    static ROOT: OnceCell<Retained<UIViewController>> = const { OnceCell::new() };
    static OWNER: RefCell<Option<Retained<SceneDelegate>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "ErlkonigSceneDelegate"]
    #[thread_kind = MainThreadOnly]
    #[ivars = SceneState]
    struct SceneDelegate;
    impl SceneDelegate {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(SceneState::default());
            unsafe { msg_send![super(this), init] }
        }
    }
    unsafe impl NSObjectProtocol for SceneDelegate {}
    unsafe impl UIWindowSceneDelegate for SceneDelegate {
        #[unsafe(method_id(window))]
        fn window(&self) -> Option<Retained<UIWindow>> { self.ivars().window.borrow().clone() }
    }
    unsafe impl UISceneDelegate for SceneDelegate {
        #[unsafe(method(scene:willConnectToSession:options:))]
        fn connect(&self, scene: &UIScene, _session: &UISceneSession, _options: &UISceneConnectionOptions) {
            let Some(scene) = scene.downcast_ref::<UIWindowScene>() else { return };
            let window = UIWindow::initWithWindowScene(UIWindow::alloc(self.mtm()), scene);
            let root = ROOT.with(|slot| slot.get_or_init(|| crate::ui::make_root(self.mtm(), model_source())).clone());
            let previous = OWNER.with(|owner| owner.replace(Some(self.retain())));
            if let Some(previous) = previous {
                previous.end_grant();
                previous.detach_window();
            }
            window.setRootViewController(Some(&root));
            self.ivars().window.replace(Some(window.clone()));
            window.makeKeyAndVisible();
        }
        #[unsafe(method(sceneDidBecomeActive:))]
        fn active(&self, _scene: &UIScene) {
            if !self.owns_root() { return; }
            if let Some(root) = self.root_controller() {
                crate::ui::set_foreground(&root, true);
            }
            self.end_grant();
        }
        #[unsafe(method(sceneDidEnterBackground:))]
        fn background(&self, _scene: &UIScene) {
            self.pause();
        }
        #[unsafe(method(sceneDidDisconnect:))]
        fn disconnected(&self, _scene: &UIScene) {
            self.pause();
            self.detach_window();
        }
    }
);

impl SceneDelegate {
    fn owns_root(&self) -> bool {
        OWNER.with(|owner| {
            owner
                .borrow()
                .as_deref()
                .is_some_and(|current| std::ptr::eq(current, self))
        })
    }

    fn detach_window(&self) {
        if let Some(window) = self.ivars().window.take() {
            window.setRootViewController(None);
        }
    }

    fn pause(&self) {
        if !self.owns_root() || self.ivars().grant.get().is_some() {
            return;
        }
        let Some(root) = self.root_controller() else {
            return;
        };
        let generation = self.ivars().generation.get().wrapping_add(1);
        self.ivars().generation.set(generation);
        let delegate = self.retain();
        let expiration = RcBlock::new(move || {
            if delegate.ivars().generation.get() == generation {
                delegate.end_grant();
            }
        });
        let app = UIApplication::sharedApplication(self.mtm());
        let id = app.beginBackgroundTaskWithName_expirationHandler(
            Some(ns_string!("Save replica")),
            Some(&expiration),
        );
        crate::ui::set_foreground(&root, false);
        if id == unsafe { UIBackgroundTaskInvalid } {
            return;
        }
        self.ivars().grant.set(Some(id));
        let delegate = self.retain();
        let tick = RcBlock::new(move |_timer| {
            if delegate.ivars().generation.get() == generation && crate::ui::pause_settled(&root) {
                delegate.end_grant();
            }
        });
        // UIKit invokes scene callbacks, expiration handlers and this
        // main-run-loop timer on the main thread.
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.25, true, &tick) };
        self.ivars().pause_timer.replace(Some(timer));
    }
    fn root_controller(&self) -> Option<Retained<objc2_ui_kit::UIViewController>> {
        self.ivars()
            .window
            .borrow()
            .as_ref()
            .and_then(|window| window.rootViewController())
    }

    fn end_grant(&self) {
        if let Some(timer) = self.ivars().pause_timer.take() {
            timer.invalidate();
        }
        if let Some(id) = self.ivars().grant.take() {
            UIApplication::sharedApplication(self.mtm()).endBackgroundTask(id);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn erl_ios_application_main() -> i32 {
    let mtm = MainThreadMarker::new().expect("UIApplicationMain requires the main thread");
    // UIApplicationMain resolves both delegate classes by their Objective-C names.
    let _ = (ApplicationDelegate::class(), SceneDelegate::class());
    UIApplication::main(None, Some(ns_string!("ErlkonigApplicationDelegate")), mtm)
}
