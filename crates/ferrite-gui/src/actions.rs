use crate::app::{
    ActiveModal, ActivePage, ActivePopover, AppWindowGlobal, FerriteApp, SettingsTab,
};
use crate::helpers::{
    macos_bring_all_to_front, macos_restore_windows_and_activate, macos_unhide_all_applications,
};
use crate::text_input::{InputCopy, InputPaste, InputSelectAll};
use ferrite_core::{DownloadId, StateFilter};
use gpui::{App, Context, Div, KeyBinding, actions, prelude::*};
use std::sync::Arc;

actions!(
    ferrite,
    [
        QuitApp,
        AboutAppAction,
        HideAppAction,
        HideOtherAppsAction,
        ShowAllAppsAction,
        CloseWindowAction,
        AddUrlAction,
        BatchAddAction,
        PasteUrlAction,
        OpenSettingsAction,
        OpenPluginsAction,
        OpenQrAction,
        FocusSearchAction,
        ToggleSelectedAction,
        SelectPrevAction,
        SelectNextAction,
        ActivateSelectedAction,
        EscapeModalAction,
        DeleteSelectedAction,
        ZoomInAction,
        ZoomOutAction,
        ZoomResetAction,
        ToggleThemeAction,
        ToggleInspectorAction,
        ToggleFullscreenAction,
        MinimizeWindowAction,
        ZoomWindowAction,
        StartAllAction,
        PauseAllAction,
        SelectAllDownloadsAction,
        ShowTroubleshootingAction,
        FilterAllAction,
        FilterActiveAction,
        FilterDownloadingAction,
        FilterFinishedAction,
        FilterStoppedAction,
        OpenExportSettingsAction,
        OpenImportSettingsAction,
        CheckForUpdatesAction,
        BringAllToFrontAction,
    ]
);

pub(crate) fn app_key_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("secondary-q", QuitApp, None),
        KeyBinding::new("secondary-w", CloseWindowAction, None),
        KeyBinding::new("secondary-n", AddUrlAction, None),
        KeyBinding::new("secondary-b", BatchAddAction, None),
        KeyBinding::new("secondary-shift-v", PasteUrlAction, None),
        KeyBinding::new(
            "secondary-v",
            PasteUrlAction,
            Some("FerriteList && !FerriteInput"),
        ),
        KeyBinding::new(
            "secondary-c",
            InputCopy,
            Some("FerriteList && !FerriteInput"),
        ),
        KeyBinding::new(
            "secondary-a",
            InputSelectAll,
            Some("FerriteList && !FerriteInput"),
        ),
        KeyBinding::new("secondary-,", OpenSettingsAction, None),
        KeyBinding::new("secondary-f", FocusSearchAction, None),
        KeyBinding::new("secondary-=", ZoomInAction, None),
        KeyBinding::new("secondary-+", ZoomInAction, None),
        KeyBinding::new("secondary--", ZoomOutAction, None),
        KeyBinding::new("secondary-0", ZoomResetAction, None),
        KeyBinding::new("secondary-i", ToggleInspectorAction, None),
        KeyBinding::new("secondary-shift-a", SelectAllDownloadsAction, None),
        KeyBinding::new("escape", EscapeModalAction, None),
        KeyBinding::new(
            "space",
            ToggleSelectedAction,
            Some("FerriteList && !FerriteInput"),
        ),
        KeyBinding::new("up", SelectPrevAction, Some("FerriteList && !FerriteInput")),
        KeyBinding::new(
            "down",
            SelectNextAction,
            Some("FerriteList && !FerriteInput"),
        ),
        KeyBinding::new(
            "enter",
            ActivateSelectedAction,
            Some("FerriteList && !FerriteInput"),
        ),
        KeyBinding::new(
            "backspace",
            DeleteSelectedAction,
            Some("FerriteList && !FerriteInput"),
        ),
        KeyBinding::new(
            "delete",
            DeleteSelectedAction,
            Some("FerriteList && !FerriteInput"),
        ),
    ];

    #[cfg(target_os = "macos")]
    bindings.extend([
        KeyBinding::new("cmd-h", HideAppAction, None),
        KeyBinding::new("alt-cmd-h", HideOtherAppsAction, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullscreenAction, None),
        KeyBinding::new("cmd-m", MinimizeWindowAction, None),
    ]);

    #[cfg(not(target_os = "macos"))]
    bindings.extend([
        KeyBinding::new("f11", ToggleFullscreenAction, None),
        KeyBinding::new(
            "ctrl-insert",
            InputCopy,
            Some("FerriteList && !FerriteInput"),
        ),
        KeyBinding::new(
            "shift-insert",
            PasteUrlAction,
            Some("FerriteList && !FerriteInput"),
        ),
        KeyBinding::new("copy", InputCopy, Some("FerriteList && !FerriteInput")),
        KeyBinding::new(
            "paste",
            PasteUrlAction,
            Some("FerriteList && !FerriteInput"),
        ),
    ]);

    bindings
}

pub(crate) fn register_global_action_handlers(cx: &mut App) {
    cx.on_action(|_: &QuitApp, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                if g.window
                    .update(cx, |view, _window, cx| {
                        view.request_quit(cx);
                    })
                    .is_err()
                {
                    cx.quit();
                }
            });
        } else {
            cx.quit();
        }
    });
    cx.on_action(|_: &AboutAppAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, _window, cx| {
                    view.open_about_modal(cx);
                });
            });
        }
    });
    cx.on_action(|_: &HideAppAction, cx| {
        cx.hide();
    });
    cx.on_action(|_: &HideOtherAppsAction, cx| {
        cx.hide_other_apps();
    });
    cx.on_action(|_: &ShowAllAppsAction, cx| {
        cx.unhide_other_apps();
        macos_unhide_all_applications();
    });
    cx.on_action(|_: &CloseWindowAction, cx| {
        cx.defer(|cx| {
            if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
                if !g.window.is_active(cx).unwrap_or(false) {
                    if let Some(aw) = cx.active_window() {
                        let _ = aw.update(cx, |_, w, _| w.remove_window());
                    }
                    return;
                }
                let _ = g.window.update(cx, |view, window, cx| {
                    view.handle_close_window_request(window, cx);
                });
            } else if let Some(aw) = cx.active_window() {
                let _ = aw.update(cx, |_, w, _| w.remove_window());
            }
        });
    });
    cx.on_action(|_: &AddUrlAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, window, cx| {
                    view.open_add_modal("", window, cx);
                });
            });
        }
    });
    cx.on_action(|_: &BatchAddAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, window, cx| {
                    view.open_batch_modal(window, cx);
                });
            });
        }
    });
    cx.on_action(|_: &PasteUrlAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, window, cx| {
                    view.paste_from_clipboard_add(window, cx);
                });
            });
        }
    });
    cx.on_action(|_: &OpenSettingsAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, _window, cx| {
                    view.open_settings_page(SettingsTab::General, cx);
                });
            });
        }
    });
    cx.on_action(|_: &CheckForUpdatesAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.entity.update(cx, |view, cx| {
                    view.check_for_app_updates_action(true, cx);
                });
            });
        }
    });
    cx.on_action(|_: &OpenPluginsAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, _window, cx| {
                    view.open_plugins_page(cx);
                });
            });
        }
    });
    cx.on_action(|_: &OpenImportSettingsAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, _window, cx| {
                    view.open_settings_page(SettingsTab::Advanced, cx);
                });
            });
        }
    });
    cx.on_action(|_: &OpenExportSettingsAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, _window, cx| {
                    view.open_settings_page(SettingsTab::Advanced, cx);
                });
            });
        }
    });
    cx.on_action(|_: &ShowTroubleshootingAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, _window, cx| {
                    view.show_troubleshooting = true;
                    view.open_settings_page(SettingsTab::Troubleshooting, cx);
                });
            });
        }
    });
    cx.on_action(|_: &OpenQrAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, _window, cx| {
                    view.open_remote_qr_modal(cx);
                });
            });
        }
    });
    cx.on_action(|_: &FocusSearchAction, cx| {
        macos_restore_windows_and_activate();
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, window, cx| {
                    view.set_active_page(ActivePage::Downloads, cx);
                    let fh = view.search_input.read(cx).focus_handle.clone();
                    window.focus(&fh);
                });
            });
        }
    });
    cx.on_action(|_: &ZoomInAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, window, cx| {
                    view.adjust_zoom(Some(10), window, cx);
                });
            });
        }
    });
    cx.on_action(|_: &ZoomOutAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, window, cx| {
                    view.adjust_zoom(Some(-10), window, cx);
                });
            });
        }
    });
    cx.on_action(|_: &ZoomResetAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, window, cx| {
                    view.adjust_zoom(None, window, cx);
                });
            });
        }
    });
    cx.on_action(|_: &ToggleThemeAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, _window, cx| {
                    view.toggle_theme(cx);
                });
            });
        }
    });
    cx.on_action(|_: &ToggleInspectorAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, _window, cx| {
                    view.set_active_page(ActivePage::Downloads, cx);
                    let next = !view.inspector_expanded;
                    view.set_inspector_expanded(next, cx);
                });
            });
        }
    });
    cx.on_action(|_: &ToggleFullscreenAction, cx| {
        cx.defer(|cx| {
            if let Some(aw) = cx.active_window() {
                let _ = aw.update(cx, |_, window, _| window.toggle_fullscreen());
            }
        });
    });
    cx.on_action(|_: &MinimizeWindowAction, cx| {
        cx.defer(|cx| {
            if let Some(aw) = cx.active_window() {
                let _ = aw.update(cx, |_, window, _| window.minimize_window());
            }
        });
    });
    cx.on_action(|_: &ZoomWindowAction, cx| {
        cx.defer(|cx| {
            if let Some(aw) = cx.active_window() {
                let _ = aw.update(cx, |_, window, _| window.zoom_window());
            }
        });
    });
    cx.on_action(|_: &BringAllToFrontAction, cx| {
        cx.defer(|_| {
            macos_bring_all_to_front();
        });
    });
    cx.on_action(|_: &StartAllAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.entity.update(cx, |view, cx| {
                    let eng = Arc::clone(&view.engine);
                    view.rt.spawn(async move {
                        let _ = eng.start_all().await;
                    });
                    cx.notify();
                });
            });
        }
    });
    cx.on_action(|_: &PauseAllAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.entity.update(cx, |view, cx| {
                    let eng = Arc::clone(&view.engine);
                    view.rt.spawn(async move {
                        let _ = eng.pause_all().await;
                    });
                    cx.notify();
                });
            });
        }
    });
    cx.on_action(|_: &SelectAllDownloadsAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.entity.update(cx, |view, cx| {
                    view.set_active_page(ActivePage::Downloads, cx);
                    view.toggle_select_all_visible(cx);
                });
            });
        }
    });
    for (filter_action_id, state_f) in [
        (0usize, StateFilter::All),
        (1, StateFilter::Running),
        (2, StateFilter::Downloading),
        (3, StateFilter::Finished),
        (4, StateFilter::Stopped),
    ] {
        match filter_action_id {
            0 => cx.on_action(move |_: &FilterAllAction, cx| {
                if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
                    cx.defer(move |cx| {
                        let _ = g.entity.update(cx, |view, cx| {
                            view.apply_state_filter_shortcut(state_f, cx);
                        });
                    });
                }
            }),
            1 => cx.on_action(move |_: &FilterActiveAction, cx| {
                if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
                    cx.defer(move |cx| {
                        let _ = g.entity.update(cx, |view, cx| {
                            view.apply_state_filter_shortcut(state_f, cx);
                        });
                    });
                }
            }),
            2 => cx.on_action(move |_: &FilterDownloadingAction, cx| {
                if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
                    cx.defer(move |cx| {
                        let _ = g.entity.update(cx, |view, cx| {
                            view.apply_state_filter_shortcut(state_f, cx);
                        });
                    });
                }
            }),
            3 => cx.on_action(move |_: &FilterFinishedAction, cx| {
                if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
                    cx.defer(move |cx| {
                        let _ = g.entity.update(cx, |view, cx| {
                            view.apply_state_filter_shortcut(state_f, cx);
                        });
                    });
                }
            }),
            _ => cx.on_action(move |_: &FilterStoppedAction, cx| {
                if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
                    cx.defer(move |cx| {
                        let _ = g.entity.update(cx, |view, cx| {
                            view.apply_state_filter_shortcut(state_f, cx);
                        });
                    });
                }
            }),
        }
    }
    cx.on_action(|_: &ToggleSelectedAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.entity.update(cx, |view, cx| {
                    view.toggle_selected_download(cx);
                });
            });
        }
    });
    cx.on_action(|_: &SelectPrevAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.entity.update(cx, |view, cx| {
                    view.select_adjacent_download(-1, cx);
                });
            });
        }
    });
    cx.on_action(|_: &SelectNextAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.entity.update(cx, |view, cx| {
                    view.select_adjacent_download(1, cx);
                });
            });
        }
    });
    cx.on_action(|_: &ActivateSelectedAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.entity.update(cx, |view, cx| {
                    view.activate_selected_download(cx);
                });
            });
        }
    });
    cx.on_action(|_: &DeleteSelectedAction, cx| {
        if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
            cx.defer(move |cx| {
                let _ = g.window.update(cx, |view, window, cx| {
                    if view.active_page == ActivePage::Downloads
                        && view.active_modal == ActiveModal::None
                    {
                        let ids: Vec<DownloadId> = if !view.checked_ids.is_empty() {
                            view.checked_ids.iter().copied().collect()
                        } else if let Some(id) = view.selected_id {
                            vec![id]
                        } else {
                            Vec::new()
                        };
                        if !ids.is_empty() {
                            view.request_delete_downloads(ids, window, cx);
                        }
                    }
                });
            });
        }
    });
    cx.on_action(|_: &EscapeModalAction, cx| {
        cx.defer(|cx| {
            if let Some(g) = cx.try_global::<AppWindowGlobal>().cloned() {
                if !g.window.is_active(cx).unwrap_or(false) {
                    if let Some(aw) = cx.active_window() {
                        let _ = aw.update(cx, |_, w, _| w.remove_window());
                    }
                    return;
                }
                let _ = g.window.update(cx, |view, window, cx| {
                    view.handle_escape(window, cx);
                });
            } else if let Some(aw) = cx.active_window() {
                let _ = aw.update(cx, |_, w, _| w.remove_window());
            }
        });
    });
}

pub(crate) fn attach_root_actions(root: Div, cx: &mut Context<FerriteApp>) -> Div {
    root.on_action(cx.listener(|this, _: &QuitApp, _, cx| {
        this.request_quit(cx);
    }))
    .on_action(cx.listener(|this, _: &AboutAppAction, _, cx| {
        this.open_about_modal(cx);
    }))
    .on_action(cx.listener(|_, _: &HideAppAction, _, cx| {
        cx.hide();
    }))
    .on_action(cx.listener(|_, _: &HideOtherAppsAction, _, cx| {
        cx.hide_other_apps();
    }))
    .on_action(cx.listener(|_, _: &ShowAllAppsAction, _, cx| {
        cx.unhide_other_apps();
        macos_unhide_all_applications();
    }))
    .on_action(cx.listener(|this, _: &CloseWindowAction, w, cx| {
        this.handle_close_window_request(w, cx);
    }))
    .on_action(cx.listener(|this, _: &AddUrlAction, w, cx| {
        this.open_add_modal("", w, cx);
    }))
    .on_action(cx.listener(|this, _: &BatchAddAction, w, cx| {
        this.open_batch_modal(w, cx);
    }))
    .on_action(cx.listener(|this, _: &PasteUrlAction, w, cx| {
        this.paste_from_clipboard_add(w, cx);
    }))
    .on_action(cx.listener(|this, _: &OpenSettingsAction, _, cx| {
        this.open_settings_page(SettingsTab::General, cx);
    }))
    .on_action(cx.listener(|this, _: &OpenPluginsAction, _, cx| {
        this.open_plugins_page(cx);
    }))
    .on_action(cx.listener(|this, _: &OpenQrAction, _, cx| {
        this.open_remote_qr_modal(cx);
    }))
    .on_action(cx.listener(|this, _: &OpenImportSettingsAction, _, cx| {
        this.open_settings_page(SettingsTab::Advanced, cx);
    }))
    .on_action(cx.listener(|this, _: &OpenExportSettingsAction, _, cx| {
        this.open_settings_page(SettingsTab::Advanced, cx);
    }))
    .on_action(cx.listener(|this, _: &ShowTroubleshootingAction, _, cx| {
        this.show_troubleshooting = true;
        this.open_settings_page(SettingsTab::Troubleshooting, cx);
    }))
    .on_action(cx.listener(|this, _: &CheckForUpdatesAction, _, cx| {
        this.check_for_app_updates_action(true, cx);
    }))
    .on_action(cx.listener(|this, _: &FocusSearchAction, w, cx| {
        this.set_active_page(ActivePage::Downloads, cx);
        let fh = this.search_input.read(cx).focus_handle.clone();
        w.focus(&fh);
    }))
    .on_action(cx.listener(|this, _: &EscapeModalAction, w, cx| {
        this.handle_escape(w, cx);
    }))
    .on_action(cx.listener(|this, _: &ZoomInAction, w, cx| {
        this.adjust_zoom(Some(10), w, cx);
    }))
    .on_action(cx.listener(|this, _: &ZoomOutAction, w, cx| {
        this.adjust_zoom(Some(-10), w, cx);
    }))
    .on_action(cx.listener(|this, _: &ZoomResetAction, w, cx| {
        this.adjust_zoom(None, w, cx);
    }))
    .on_action(cx.listener(|this, _: &ToggleThemeAction, _, cx| {
        this.toggle_theme(cx);
    }))
    .on_action(cx.listener(|this, _: &ToggleInspectorAction, _, cx| {
        this.set_active_page(ActivePage::Downloads, cx);
        let next = !this.inspector_expanded;
        this.set_inspector_expanded(next, cx);
    }))
    .on_action(cx.listener(|_, _: &ToggleFullscreenAction, w, _| {
        w.toggle_fullscreen();
    }))
    .on_action(cx.listener(|_, _: &MinimizeWindowAction, w, _| {
        w.minimize_window();
    }))
    .on_action(cx.listener(|_, _: &ZoomWindowAction, w, _| {
        w.zoom_window();
    }))
    .on_action(cx.listener(|_, _: &BringAllToFrontAction, _, _| {
        macos_bring_all_to_front();
    }))
    .on_action(cx.listener(|this, _: &StartAllAction, _, cx| {
        let eng = Arc::clone(&this.engine);
        this.rt.spawn(async move {
            let _ = eng.start_all().await;
        });
        cx.notify();
    }))
    .on_action(cx.listener(|this, _: &PauseAllAction, _, cx| {
        let eng = Arc::clone(&this.engine);
        this.rt.spawn(async move {
            let _ = eng.pause_all().await;
        });
        cx.notify();
    }))
    .on_action(cx.listener(|this, _: &FilterAllAction, _, cx| {
        this.apply_state_filter_shortcut(StateFilter::All, cx);
    }))
    .on_action(cx.listener(|this, _: &FilterActiveAction, _, cx| {
        this.apply_state_filter_shortcut(StateFilter::Running, cx);
    }))
    .on_action(cx.listener(|this, _: &FilterDownloadingAction, _, cx| {
        this.apply_state_filter_shortcut(StateFilter::Downloading, cx);
    }))
    .on_action(cx.listener(|this, _: &FilterFinishedAction, _, cx| {
        this.apply_state_filter_shortcut(StateFilter::Finished, cx);
    }))
    .on_action(cx.listener(|this, _: &FilterStoppedAction, _, cx| {
        this.apply_state_filter_shortcut(StateFilter::Stopped, cx);
    }))
    .on_action(cx.listener(|this, _: &ToggleSelectedAction, _, cx| {
        if this.active_page == ActivePage::Downloads && this.active_modal == ActiveModal::None {
            this.toggle_selected_download(cx);
        }
    }))
    .on_action(cx.listener(|this, _: &SelectPrevAction, _, cx| {
        if this.active_page == ActivePage::Downloads && this.active_modal == ActiveModal::None {
            this.select_adjacent_download(-1, cx);
        }
    }))
    .on_action(cx.listener(|this, _: &SelectNextAction, _, cx| {
        if this.active_page == ActivePage::Downloads && this.active_modal == ActiveModal::None {
            this.select_adjacent_download(1, cx);
        }
    }))
    .on_action(cx.listener(|this, _: &ActivateSelectedAction, _, cx| {
        if this.active_page == ActivePage::Downloads && this.active_modal == ActiveModal::None {
            this.activate_selected_download(cx);
        }
    }))
    .on_action(cx.listener(|this, _: &SelectAllDownloadsAction, _, cx| {
        if this.active_page == ActivePage::Downloads && this.active_modal == ActiveModal::None {
            this.toggle_select_all_visible(cx);
        }
    }))
    .on_action(cx.listener(|this, _: &DeleteSelectedAction, w, cx| {
        if this.active_page == ActivePage::Downloads && this.active_modal == ActiveModal::None {
            let ids: Vec<DownloadId> = if !this.checked_ids.is_empty() {
                this.checked_ids.iter().copied().collect()
            } else if let Some(id) = this.selected_id {
                vec![id]
            } else {
                Vec::new()
            };
            if !ids.is_empty() {
                this.request_delete_downloads(ids, w, cx);
            }
        }
    }))
    .on_action(cx.listener(|this, _: &InputCopy, _, cx| {
        if this.active_page == ActivePage::Downloads
            && this.active_modal == ActiveModal::None
            && this.active_popover == ActivePopover::None
            && let Some(id) = this.selected_id
            && let Some(rec) = this.all_downloads.iter().find(|d| d.id == id).cloned()
        {
            this.copy_to_clipboard_from_keyboard(rec.url, "Copied download URL", cx);
        }
    }))
    .on_action(cx.listener(|this, _: &InputPaste, w, cx| {
        this.paste_from_clipboard_add(w, cx);
    }))
    .on_action(cx.listener(|this, _: &InputSelectAll, _, cx| {
        if this.active_page == ActivePage::Downloads && this.active_modal == ActiveModal::None {
            this.toggle_select_all_visible(cx);
        }
    }))
}
