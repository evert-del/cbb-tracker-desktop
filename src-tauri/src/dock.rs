//! macOS: the Dock icon's menu (right-click or hold), for the project timer
//! (hourly.rs): the running timer ("Logo refresh · 1h 05m"), Stop, Switch
//! to the recent projects, "Just start, file it later" and Open My Hours.
//!
//! Tauri has no Dock menu, so AppKit's `applicationDockMenu:` is added to
//! tao's application delegate class. AppKit asks it on the main thread each
//! time the menu opens; it is built then from the latest model, as a menu
//! of the menu crate Tauri already uses (muda), so its clicks arrive with
//! the tray menu's (lib.rs) and go to `hourly::menu_action`. Without a
//! timer the Dock menu is the system's alone.

use std::cell::RefCell;

use muda::{ContextMenu, Menu, MenuItem, PredefinedMenuItem, Submenu};
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel};

thread_local! {
    /// The menu last handed to AppKit, kept alive until the next one.
    static SHOWN: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

fn build(model: &crate::hourly::MenuModel) -> Option<Menu> {
    let menu = Menu::new();
    menu.append(&MenuItem::with_id("hourly-title", &model.title, false, None)).ok()?;
    if model.running {
        menu.append(&MenuItem::with_id(crate::hourly::MENU_STOP, "Stop", true, None)).ok()?;
    }
    if !model.projects.is_empty() {
        let projects = Submenu::new(if model.running { "Switch to" } else { "Start" }, true);
        for (id, label) in &model.projects {
            projects.append(&MenuItem::with_id(id.as_str(), label, true, None)).ok()?;
        }
        menu.append(&projects).ok()?;
    }
    menu.append(&MenuItem::with_id(crate::hourly::MENU_LATER, "Just start, file it later", true, None)).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    menu.append(&MenuItem::with_id(crate::hourly::MENU_HOURS, "Open My Hours", true, None)).ok()?;
    Some(menu)
}

/// `- (NSMenu *)applicationDockMenu:(NSApplication *)sender`.
unsafe extern "C-unwind" fn dock_menu(_this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> *mut AnyObject {
    let Some(menu) = crate::hourly::dock_model().as_ref().and_then(build) else {
        return std::ptr::null_mut();
    };
    let ns_menu = menu.ns_menu() as *mut AnyObject;
    SHOWN.with(|shown| *shown.borrow_mut() = Some(menu));
    ns_menu
}

/// Teach the app delegate `applicationDockMenu:`. Call once, on the main
/// thread, after launch (Tauri's setup).
pub(crate) fn install() {
    let Some(class) = AnyClass::get(c"TaoAppDelegateParent") else { return };
    let Some(app_class) = AnyClass::get(c"NSApplication") else { return };
    // SAFETY: the function matches the selector's type encoding ("@@:@":
    // returns an object; self, _cmd, one object argument). class_addMethod
    // leaves an existing method alone. The delegate is set again so AppKit
    // looks up the methods it answers afresh; tao keeps it alive.
    unsafe {
        let imp: Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> *mut AnyObject,
            Imp,
        >(dock_menu);
        let class = class as *const AnyClass as *mut AnyClass;
        if !objc2::ffi::class_addMethod(class, sel!(applicationDockMenu:), imp, c"@@:@".as_ptr()).as_bool() {
            return;
        }
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return;
        }
        let delegate: *mut AnyObject = msg_send![app, delegate];
        if !delegate.is_null() {
            let _: () = msg_send![app, setDelegate: std::ptr::null_mut::<AnyObject>()];
            let _: () = msg_send![app, setDelegate: delegate];
        }
    }
}
