#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ShowFerrite,
    AddDownload,
    StartAll,
    PauseAll,
    ToggleSnailMode,
    Quit,
}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)]
mod imp {
    use super::TrayAction;
    use crate::helpers::{compact_speed_label, macos_restore_windows_and_activate};
    use cocoa::appkit::{NSMenu, NSMenuItem, NSStatusBar, NSVariableStatusItemLength};
    use cocoa::base::{NO, id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::{Mutex, Once, OnceLock};

    fn action_queue() -> &'static Mutex<Vec<TrayAction>> {
        static QUEUE: OnceLock<Mutex<Vec<TrayAction>>> = OnceLock::new();
        QUEUE.get_or_init(|| Mutex::new(Vec::new()))
    }

    fn push_action(action: TrayAction) {
        if let Ok(mut q) = action_queue().lock() {
            q.push(action);
        }
    }

    pub fn has_pending_tray_actions() -> bool {
        action_queue()
            .lock()
            .map(|q| !q.is_empty())
            .unwrap_or(false)
    }

    pub fn drain_tray_actions() -> Vec<TrayAction> {
        let Ok(mut q) = action_queue().lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *q)
    }

    extern "C" fn on_show_ferrite(_this: &Object, _cmd: Sel, _sender: id) {
        macos_restore_windows_and_activate();
        push_action(TrayAction::ShowFerrite);
    }

    extern "C" fn on_add_download(_this: &Object, _cmd: Sel, _sender: id) {
        macos_restore_windows_and_activate();
        push_action(TrayAction::AddDownload);
    }

    extern "C" fn on_start_all(_this: &Object, _cmd: Sel, _sender: id) {
        push_action(TrayAction::StartAll);
    }

    extern "C" fn on_pause_all(_this: &Object, _cmd: Sel, _sender: id) {
        push_action(TrayAction::PauseAll);
    }

    extern "C" fn on_toggle_snail(_this: &Object, _cmd: Sel, _sender: id) {
        push_action(TrayAction::ToggleSnailMode);
    }

    extern "C" fn on_quit(_this: &Object, _cmd: Sel, _sender: id) {
        push_action(TrayAction::Quit);
    }

    fn tray_target_class() -> &'static Class {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let mut decl = ClassDecl::new("FerriteTrayTarget", class!(NSObject))
                .expect("Failed to declare FerriteTrayTarget class");
            unsafe {
                decl.add_method(
                    sel!(onShowFerrite:),
                    on_show_ferrite as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(onAddDownload:),
                    on_add_download as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(onStartAll:),
                    on_start_all as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(onPauseAll:),
                    on_pause_all as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(onToggleSnail:),
                    on_toggle_snail as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(sel!(onQuit:), on_quit as extern "C" fn(&Object, Sel, id));
            }
            decl.register();
        });
        Class::get("FerriteTrayTarget").expect("FerriteTrayTarget class missing")
    }

    #[allow(deprecated, unexpected_cfgs)]
    unsafe fn make_menu_item(title: &str, action: Sel, target: id) -> id {
        unsafe {
            let ns_title = NSString::alloc(nil).init_str(title).autorelease();
            let ns_key = NSString::alloc(nil).init_str("").autorelease();
            let item = NSMenuItem::alloc(nil)
                .initWithTitle_action_keyEquivalent_(ns_title, action, ns_key)
                .autorelease();
            let _: () = msg_send![item, setTarget: target];
            item
        }
    }

    pub struct MacosTrayController {
        status_item: id,
        snail_item: id,
        target: id,
        last_title: String,
        last_snail: Option<bool>,
    }

    impl MacosTrayController {
        #[allow(deprecated, unexpected_cfgs)]
        pub fn new() -> Self {
            unsafe {
                let pool = NSAutoreleasePool::new(nil);
                let target: id = msg_send![tray_target_class(), new];

                let status_bar = NSStatusBar::systemStatusBar(nil);
                let status_item: id =
                    msg_send![status_bar, statusItemWithLength: NSVariableStatusItemLength];
                let _: id = msg_send![status_item, retain];

                let button: id = msg_send![status_item, button];
                if button != nil {
                    let init_title = NSString::alloc(nil).init_str("⬇").autorelease();
                    let _: () = msg_send![button, setTitle: init_title];
                    let tip = NSString::alloc(nil).init_str("Ferrite").autorelease();
                    let _: () = msg_send![button, setToolTip: tip];
                }

                let menu = NSMenu::new(nil).autorelease();
                let _: () = msg_send![menu, setAutoenablesItems: NO];

                let show_item = make_menu_item("Show Ferrite", sel!(onShowFerrite:), target);
                let add_item = make_menu_item("Add Download…", sel!(onAddDownload:), target);
                let start_item = make_menu_item("Start All", sel!(onStartAll:), target);
                let pause_item = make_menu_item("Pause All", sel!(onPauseAll:), target);
                let snail_item = make_menu_item("Snail Mode", sel!(onToggleSnail:), target);
                let quit_item = make_menu_item("Quit Ferrite", sel!(onQuit:), target);

                let _: () = msg_send![snail_item, retain];

                menu.addItem_(show_item);
                menu.addItem_(NSMenuItem::separatorItem(nil));
                menu.addItem_(add_item);
                menu.addItem_(start_item);
                menu.addItem_(pause_item);
                menu.addItem_(snail_item);
                menu.addItem_(NSMenuItem::separatorItem(nil));
                menu.addItem_(quit_item);

                let _: () = msg_send![status_item, setMenu: menu];
                pool.drain();

                Self {
                    status_item,
                    snail_item,
                    target,
                    last_title: "⬇".to_string(),
                    last_snail: None,
                }
            }
        }

        #[allow(deprecated, unexpected_cfgs)]
        pub fn update(&mut self, show_speeds: bool, dl_bps: u64, ul_bps: u64, snail_mode: bool) {
            let next_title = if show_speeds && (dl_bps > 0 || ul_bps > 0) {
                format!(
                    "↓{} ↑{}",
                    compact_speed_label(dl_bps),
                    compact_speed_label(ul_bps)
                )
            } else {
                "⬇".to_string()
            };

            unsafe {
                let pool = NSAutoreleasePool::new(nil);
                if next_title != self.last_title {
                    let button: id = msg_send![self.status_item, button];
                    if button != nil {
                        let ns_title = NSString::alloc(nil).init_str(&next_title).autorelease();
                        let _: () = msg_send![button, setTitle: ns_title];
                    }
                    self.last_title = next_title;
                }

                if self.last_snail != Some(snail_mode) {
                    let state: isize = if snail_mode { 1 } else { 0 };
                    let _: () = msg_send![self.snail_item, setState: state];
                    self.last_snail = Some(snail_mode);
                }
                pool.drain();
            }
        }
    }

    impl Drop for MacosTrayController {
        #[allow(deprecated, unexpected_cfgs)]
        fn drop(&mut self) {
            unsafe {
                if self.status_item != nil {
                    let status_bar = NSStatusBar::systemStatusBar(nil);
                    let _: () = msg_send![status_bar, removeStatusItem: self.status_item];
                    let _: () = msg_send![self.status_item, release];
                    self.status_item = nil;
                }
                if self.snail_item != nil {
                    let _: () = msg_send![self.snail_item, release];
                    self.snail_item = nil;
                }
                if self.target != nil {
                    let _: () = msg_send![self.target, release];
                    self.target = nil;
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::TrayAction;

    pub fn has_pending_tray_actions() -> bool {
        false
    }

    pub fn drain_tray_actions() -> Vec<TrayAction> {
        Vec::new()
    }

    pub struct MacosTrayController;

    impl MacosTrayController {
        pub fn new() -> Self {
            Self
        }

        pub fn update(
            &mut self,
            _show_speeds: bool,
            _dl_bps: u64,
            _ul_bps: u64,
            _snail_mode: bool,
        ) {
        }
    }
}

pub use imp::{MacosTrayController, drain_tray_actions, has_pending_tray_actions};
