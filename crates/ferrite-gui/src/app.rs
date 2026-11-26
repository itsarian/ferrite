use crate::actions::{
    AboutAppAction, AddUrlAction, BatchAddAction, CheckForUpdatesAction, CloseWindowAction,
    FilterActiveAction, FilterAllAction, FilterDownloadingAction, FilterFinishedAction,
    FilterStoppedAction, FocusSearchAction, OpenExportSettingsAction, OpenImportSettingsAction,
    OpenPluginsAction, OpenQrAction, OpenSettingsAction, PasteUrlAction, PauseAllAction, QuitApp,
    SelectAllDownloadsAction, ShowTroubleshootingAction, StartAllAction, ToggleInspectorAction,
    ToggleThemeAction, ZoomInAction, ZoomOutAction, ZoomResetAction, app_key_bindings,
    attach_root_actions, register_global_action_handlers,
};
#[cfg(target_os = "macos")]
use crate::actions::{
    BringAllToFrontAction, HideAppAction, HideOtherAppsAction, ShowAllAppsAction,
};
use crate::bottom_panel::render_bottom_panel;
use crate::helpers::{
    LaunchOptions, UiAsyncMsg, UiMsgQueue, format_kb_limit, format_minute_hhmm,
    format_platform_shortcuts, set_macos_hide_dock_icon, zoom_rem_size_px,
};
use crate::i18n;
use crate::macos_tray::MacosTrayController;
use crate::menus::render_popover;
use crate::modals::render_active_modal;
use crate::plugins_page::render_plugins_page;
use crate::settings_page::render_settings_page;
use crate::standalone::open_standalone_add_window;
use crate::text_input::{
    InputCopy, InputCut, InputPaste, InputRedo, InputSelectAll, InputUndo, TextInput,
};
use crate::theme::ThemePalette;
use crate::ui_widgets::ghost_icon_btn;
use ferrite_cli::{AddProbeOutcome, DownloadEngine, EngineStateSummary};
use ferrite_core::os_integration::is_default_torrent_client;
use ferrite_core::{
    DownloadId, DownloadPriority, DownloadRecord, DownloadState, EngineEvent, FerriteConfig,
    HashAlgorithm, KnownWebBrowser, NotificationEvent, ProxySettings, SortField, SslException,
    StateFilter, Tag, TrafficUsageMode,
};
use ferrite_ipc::RemotePairingInfo;
use ferrite_media::JsPluginInfo;
#[cfg(target_os = "macos")]
use gpui::SystemMenuType;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, Entity, FocusHandle, Focusable, Global,
    Menu, MenuItem, MouseButton, OsAction, Pixels, Point, Render, SharedString,
    UniformListScrollHandle, Window, div, ease_out_quint, prelude::*, px,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct AppWindowGlobal {
    pub entity: gpui::WeakEntity<FerriteApp>,
    pub window: gpui::WindowHandle<FerriteApp>,
}

impl Global for AppWindowGlobal {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePage {
    Downloads,
    Settings(SettingsTab),
    Plugins,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    General,
    Details,
    Files,
    Progress,
    Connections,
    SpeedGraph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Browser,
    Network,
    Traffic,
    Antivirus,
    BitTorrent,
    RemoteAccess,
    Advanced,
    Troubleshooting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivePopover {
    None,
    SortMenu,
    FilterMenu,
    MainMenu,
    TumMenu,
    RowContextMenu(DownloadId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveModal {
    None,
    AddDownload,
    BatchAdd,
    FileConflict { id: DownloadId, path: PathBuf },
    HashVerify { id: DownloadId },
    MediaConvert { id: DownloadId },
    RemoteQr,
    EditTag { id: Option<i64> },
    ChangeSource { id: DownloadId },
    MoveDir { ids: Vec<DownloadId> },
    RenameDownload { id: DownloadId },
    DownloadSchedule { id: DownloadId },
    DeleteConfirm { ids: Vec<DownloadId> },
    QuitConfirm,
    About,
    CreatePortable,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_proxy_inputs_to_settings(
    proxy: &mut ProxySettings,
    p_host: String,
    p_port: u16,
    s5_host: String,
    s5_port: u16,
    p_user: String,
    p_pass: String,
) {
    proxy.http.host = p_host.clone();
    proxy.http.port = p_port;
    proxy.http.username = p_user.clone();
    proxy.http.password = p_pass.clone();
    proxy.https.host = p_host;
    proxy.https.port = p_port;
    proxy.https.username = p_user.clone();
    proxy.https.password = p_pass.clone();

    let s5_active = !s5_host.is_empty();
    proxy.socks5.host = s5_host;
    proxy.socks5.port = s5_port;
    if s5_active {
        proxy.socks5.username = p_user;
        proxy.socks5.password = p_pass;
    } else {
        proxy.socks5.username.clear();
        proxy.socks5.password.clear();
    }
}

pub fn bind_app_keys_and_menus(cx: &mut App) {
    cx.bind_keys(app_key_bindings());

    let app_menu_items = {
        #[allow(unused_mut)]
        let mut items = vec![
            MenuItem::action("About Ferrite", AboutAppAction),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettingsAction),
            MenuItem::action("Check for Updates…", CheckForUpdatesAction),
            MenuItem::action("Add-ons…", OpenPluginsAction),
            MenuItem::separator(),
        ];
        #[cfg(target_os = "macos")]
        items.extend([
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Ferrite", HideAppAction),
            MenuItem::action("Hide Others", HideOtherAppsAction),
            MenuItem::action("Show All", ShowAllAppsAction),
            MenuItem::separator(),
        ]);
        items.push(MenuItem::action("Quit Ferrite", QuitApp));
        items
    };

    let mut menus = vec![
        Menu {
            name: "Ferrite".into(),
            items: app_menu_items,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Add URL / Torrent…", AddUrlAction),
                MenuItem::action("Batch Download…", BatchAddAction),
                MenuItem::separator(),
                MenuItem::action("Import Downloads / Settings…", OpenImportSettingsAction),
                MenuItem::action("Export Downloads / Settings…", OpenExportSettingsAction),
                MenuItem::separator(),
                MenuItem::action("Close Window", CloseWindowAction),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", InputUndo, OsAction::Undo),
                MenuItem::os_action("Redo", InputRedo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", InputCut, OsAction::Cut),
                MenuItem::os_action("Copy", InputCopy, OsAction::Copy),
                MenuItem::os_action("Paste", InputPaste, OsAction::Paste),
                MenuItem::os_action("Select All", InputSelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action("Find…", FocusSearchAction),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Details Inspector", ToggleInspectorAction),
                MenuItem::separator(),
                MenuItem::action("Zoom In", ZoomInAction),
                MenuItem::action("Zoom Out", ZoomOutAction),
                MenuItem::action("Actual Size", ZoomResetAction),
                MenuItem::separator(),
                MenuItem::action("Toggle Light / Dark Theme", ToggleThemeAction),
                MenuItem::submenu(Menu {
                    name: "Filter Downloads".into(),
                    items: vec![
                        MenuItem::action("All Downloads", FilterAllAction),
                        MenuItem::action("Active", FilterActiveAction),
                        MenuItem::action("Downloading", FilterDownloadingAction),
                        MenuItem::action("Completed", FilterFinishedAction),
                        MenuItem::action("Paused / Stopped", FilterStoppedAction),
                    ],
                }),
            ],
        },
        Menu {
            name: "Downloads".into(),
            items: vec![
                MenuItem::action("Add Download URL…", AddUrlAction),
                MenuItem::action("Paste URL from Clipboard…", PasteUrlAction),
                MenuItem::action("Batch Download…", BatchAddAction),
                MenuItem::separator(),
                MenuItem::action("Start All Downloads", StartAllAction),
                MenuItem::action("Pause All Downloads", PauseAllAction),
                MenuItem::action("Select / Deselect All Visible", SelectAllDownloadsAction),
                MenuItem::separator(),
                MenuItem::action("Remote Control QR…", OpenQrAction),
            ],
        },
    ];

    #[cfg(target_os = "macos")]
    menus.push(Menu {
        name: "Window".into(),
        items: vec![
            MenuItem::action("Minimize", crate::actions::MinimizeWindowAction),
            MenuItem::action("Zoom", crate::actions::ZoomWindowAction),
            MenuItem::separator(),
            MenuItem::action("Bring All to Front", BringAllToFrontAction),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindowAction),
        ],
    });

    menus.push(Menu {
        name: "Help".into(),
        items: vec![
            MenuItem::action("Check for Updates…", CheckForUpdatesAction),
            MenuItem::separator(),
            MenuItem::action("Troubleshooting & Diagnostics…", ShowTroubleshootingAction),
            MenuItem::separator(),
            MenuItem::action("About Ferrite", AboutAppAction),
        ],
    });

    cx.set_menus(menus);

    #[cfg(target_os = "macos")]
    cx.set_dock_menu(vec![
        MenuItem::action("Add Download URL…", AddUrlAction),
        MenuItem::action("Paste URL from Clipboard…", PasteUrlAction),
        MenuItem::action("Batch Download…", BatchAddAction),
        MenuItem::separator(),
        MenuItem::action("Start All Downloads", StartAllAction),
        MenuItem::action("Pause All Downloads", PauseAllAction),
        MenuItem::separator(),
        MenuItem::action("Settings…", OpenSettingsAction),
    ]);

    register_global_action_handlers(cx);
}

pub struct FerriteApp {
    pub(crate) rt: Arc<tokio::runtime::Runtime>,
    pub(crate) engine: Arc<DownloadEngine>,
    pub(crate) msgs: UiMsgQueue,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) theme: ThemePalette,

    pub(crate) active_page: ActivePage,
    pub(crate) summary: EngineStateSummary,
    pub(crate) all_downloads: Vec<DownloadRecord>,
    pub(crate) visible_downloads: Vec<DownloadRecord>,
    pub(crate) tags: Vec<Tag>,
    pub(crate) selected_id: Option<DownloadId>,
    pub(crate) parent_folder_id: Option<DownloadId>,
    pub(crate) hovered_row_id: Option<DownloadId>,
    pub(crate) checked_ids: HashSet<DownloadId>,
    pub(crate) active_filter: StateFilter,
    pub(crate) active_tag_id: Option<i64>,
    pub(crate) sort_by: SortField,
    pub(crate) sort_ascending: bool,
    pub(crate) inspector_tab: InspectorTab,
    pub(crate) inspector_expanded: bool,
    pub(crate) last_non_snail_tum: TrafficUsageMode,
    pub(crate) active_popover: ActivePopover,
    pub(crate) shutdown_submenu_open: bool,
    pub(crate) active_modal: ActiveModal,
    pub(crate) delete_remember_choice: bool,
    pub(crate) toast: Option<(String, bool, Instant)>,

    // One-shot animation epochs (0% idle CPU) & OS window tracking
    pub(crate) modal_anim_epoch: u64,
    pub(crate) popover_anim_epoch: u64,
    pub(crate) plugins_gear_anim_epoch: u64,
    pub(crate) inspector_anim_epoch: u64,
    pub(crate) inspector_closing_since: Option<Instant>,
    pub(crate) toast_anim_epoch: u64,
    pub(crate) page_anim_epoch: u64,
    pub(crate) settings_tab_anim_epoch: u64,
    pub(crate) inspector_tab_anim_epoch: u64,
    pub(crate) adv_panel_anim_epoch: u64,
    pub(crate) probe_anim_epoch: u64,
    pub(crate) shutdown_sub_anim_epoch: u64,
    pub(crate) proxy_box_anim_epoch: u64,
    pub(crate) last_dock_badge: Option<String>,
    pub(crate) window_active: bool,
    pub(crate) manual_torrent_check_pending: bool,

    pub(crate) downloads_scroll_handle: UniformListScrollHandle,
    pub(crate) context_menu_pos: Option<Point<Pixels>>,
    pub(crate) cached_add_disk_free: Option<(String, Option<u64>)>,
    pub(crate) cached_native_host_installed: HashMap<KnownWebBrowser, bool>,
    pub(crate) cached_antivirus_presets:
        Vec<(&'static str, &'static str, &'static str, &'static str, bool)>,
    pub(crate) macos_tray: MacosTrayController,

    pub(crate) search_input: Entity<TextInput>,

    // Add / Tune & Add modal
    pub(crate) add_url_input: Entity<TextInput>,
    pub(crate) add_dir_input: Entity<TextInput>,
    pub(crate) add_name_input: Entity<TextInput>,
    pub(crate) add_ref_input: Entity<TextInput>,
    pub(crate) add_user_input: Entity<TextInput>,
    pub(crate) add_pass_input: Entity<TextInput>,
    pub(crate) add_show_pass: bool,
    pub(crate) add_priority: DownloadPriority,
    pub(crate) add_start_paused: bool,
    pub(crate) add_play_asap: bool,
    pub(crate) add_apply_schedule: bool,
    pub(crate) add_show_advanced: bool,
    pub(crate) add_probe_busy: bool,
    pub(crate) add_probe_outcome: Option<AddProbeOutcome>,
    pub(crate) add_selected_files: HashSet<usize>,
    pub(crate) add_selected_variant: usize,

    // Batch modal
    pub(crate) batch_pattern_input: Entity<TextInput>,
    pub(crate) batch_group_title_input: Entity<TextInput>,
    pub(crate) batch_dir_input: Entity<TextInput>,
    pub(crate) batch_start_paused: bool,
    pub(crate) batch_as_group: bool,

    // Hash verification modal
    pub(crate) hash_algo: HashAlgorithm,
    pub(crate) hash_expected_input: Entity<TextInput>,
    pub(crate) hash_computed: Option<String>,
    pub(crate) hash_matched: Option<bool>,

    // Media convert modal
    pub(crate) convert_mode_mp3: bool,
    pub(crate) convert_mp3_bitrate: u32,
    pub(crate) convert_use_vbr: bool,
    pub(crate) convert_vbr_quality: u8,
    pub(crate) convert_stream_copy: bool,
    pub(crate) convert_out_input: Entity<TextInput>,
    pub(crate) convert_busy: bool,
    pub(crate) convert_status: Option<String>,

    // Remote QR modal
    pub(crate) remote_pairing: Option<RemotePairingInfo>,
    pub(crate) remote_loading: bool,
    pub(crate) remote_error: Option<String>,

    // New / Edit tag modal
    pub(crate) new_tag_name_input: Entity<TextInput>,
    pub(crate) new_tag_color_hex: String,
    pub(crate) tag_exts_input: Entity<TextInput>,
    pub(crate) tag_dir_input: Entity<TextInput>,

    // Change URL / mirrors modal
    pub(crate) source_url_input: Entity<TextInput>,
    pub(crate) source_mirrors_input: Entity<TextInput>,

    // Move dir modal
    pub(crate) move_dir_input: Entity<TextInput>,

    // Rename output file modal
    pub(crate) rename_file_input: Entity<TextInput>,

    // Portable bundle modal
    pub(crate) portable_dir_input: Entity<TextInput>,

    // Per-download schedule modal
    pub(crate) sched_enabled: bool,
    pub(crate) sched_days_mask: u8,
    pub(crate) sched_start_input: Entity<TextInput>,
    pub(crate) sched_end_input: Entity<TextInput>,

    // Bottom panel BT tracker input
    pub(crate) bt_add_tracker_input: Entity<TextInput>,

    // Add-ons page state
    pub(crate) plugins_title_clicks: u8,
    pub(crate) show_plugin_dev: bool,
    pub(crate) plugins_gear_menu_open: bool,
    pub(crate) plugin_install_input: Entity<TextInput>,

    // Settings page draft + state + inputs
    pub(crate) settings_cfg: FerriteConfig,
    pub(crate) settings_tum_tab: TrafficUsageMode,
    pub(crate) settings_dirty: bool,
    pub(crate) settings_plugins: Vec<JsPluginInfo>,
    pub(crate) settings_ssl_exceptions: Vec<SslException>,
    pub(crate) prefs_title_clicks: u8,
    pub(crate) show_troubleshooting: bool,
    pub(crate) show_proxy_pass: bool,
    pub(crate) show_remote_pass: bool,

    pub(crate) set_default_dir: Entity<TextInput>,
    pub(crate) set_partial_suffix: Entity<TextInput>,
    pub(crate) set_keep_days: Entity<TextInput>,
    pub(crate) set_max_retries: Entity<TextInput>,
    pub(crate) set_slow_speed_kb: Entity<TextInput>,
    pub(crate) set_slow_interval_s: Entity<TextInput>,
    pub(crate) set_max_query_dl: Entity<TextInput>,
    pub(crate) set_max_small_dl: Entity<TextInput>,
    pub(crate) set_max_slow_dl: Entity<TextInput>,
    pub(crate) set_tum_dl_kb: Entity<TextInput>,
    pub(crate) set_tum_ul_kb: Entity<TextInput>,
    pub(crate) set_tum_conns: Entity<TextInput>,
    pub(crate) set_tum_per_srv: Entity<TextInput>,
    pub(crate) set_tum_concurrent: Entity<TextInput>,
    pub(crate) set_tum_ratio: Entity<TextInput>,
    pub(crate) set_proxy_host: Entity<TextInput>,
    pub(crate) set_proxy_port: Entity<TextInput>,
    pub(crate) set_socks5_host: Entity<TextInput>,
    pub(crate) set_socks5_port: Entity<TextInput>,
    pub(crate) set_proxy_user: Entity<TextInput>,
    pub(crate) set_proxy_pass: Entity<TextInput>,
    pub(crate) set_bt_port: Entity<TextInput>,
    pub(crate) set_bt_watch_dir: Entity<TextInput>,
    pub(crate) set_bt_trackers: Entity<TextInput>,
    pub(crate) set_sched_start: Entity<TextInput>,
    pub(crate) set_sched_end: Entity<TextInput>,
    pub(crate) set_battery_min: Entity<TextInput>,
    pub(crate) set_wb_small_kb: Entity<TextInput>,
    pub(crate) set_wb_skip_ext: Entity<TextInput>,
    pub(crate) set_wb_only_ext: Entity<TextInput>,
    pub(crate) set_wb_skip_hosts: Entity<TextInput>,
    pub(crate) set_ffmpeg_path: Entity<TextInput>,
    pub(crate) set_ytdlp_path: Entity<TextInput>,
    pub(crate) set_av_path: Entity<TextInput>,
    pub(crate) set_av_args: Entity<TextInput>,
    pub(crate) set_ext_path: Entity<TextInput>,
    pub(crate) set_ext_args: Entity<TextInput>,
    pub(crate) set_remote_bind: Entity<TextInput>,
    pub(crate) set_remote_port: Entity<TextInput>,
    pub(crate) set_remote_pass: Entity<TextInput>,
    pub(crate) set_notify_sound_path: Entity<TextInput>,
    pub(crate) set_low_disk_mb: Entity<TextInput>,
    pub(crate) set_low_disk_pct: Entity<TextInput>,
    pub(crate) set_backup_interval_m: Entity<TextInput>,
    pub(crate) set_batch_max_urls: Entity<TextInput>,
    pub(crate) set_data_io_path: Entity<TextInput>,
    pub(crate) set_diag_zip_path: Entity<TextInput>,
}

impl Focusable for FerriteApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl FerriteApp {
    pub fn new(
        rt: Arc<tokio::runtime::Runtime>,
        engine: Arc<DownloadEngine>,
        launch: LaunchOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let init_remote_pw = engine.ensure_remote_password();
        let cfg = engine.get_config();
        let theme = ThemePalette::from_config(
            cfg.app.theme_mode,
            cfg.app.ui_style,
            cfg.app.overall_zoom_pct,
            cfg.app.fonts_zoom_pct,
        );
        window.set_rem_size(px(zoom_rem_size_px(cfg.app.overall_zoom_pct)));
        let msgs = UiMsgQueue::default();
        let init_ssl = engine.list_ssl_exceptions().unwrap_or_default();

        let mut rx_events = engine.subscribe();
        let mut rx_raise = engine.subscribe_raise();
        let q_ev = msgs.clone();
        let engine_dirty = Arc::new(AtomicBool::new(false));
        let dirty_ev = Arc::clone(&engine_dirty);
        rt.spawn(async move {
            let mut prev_stats_active = false;
            loop {
                tokio::select! {
                    Ok(ev) = rx_events.recv() => {
                        match ev {
                            EngineEvent::FileConflict { id, path } => {
                                q_ev.push(UiAsyncMsg::FileConflict { id, path });
                                dirty_ev.store(true, Ordering::Release);
                            }
                            EngineEvent::Added { id } => {
                                q_ev.push(UiAsyncMsg::DownloadAddedNotification { id });
                                dirty_ev.store(true, Ordering::Release);
                            }
                            EngineEvent::Completed { id } => {
                                q_ev.push(UiAsyncMsg::DownloadCompletedNotification { id });
                                dirty_ev.store(true, Ordering::Release);
                            }
                            EngineEvent::Failed { id, error } => {
                                q_ev.push(UiAsyncMsg::DownloadFailedNotification { id, error });
                                dirty_ev.store(true, Ordering::Release);
                            }
                            EngineEvent::QuitRequested => {
                                q_ev.push(UiAsyncMsg::QuitRequested);
                                dirty_ev.store(true, Ordering::Release);
                            }
                            EngineEvent::QueueStats {
                                total_download_bps,
                                total_upload_bps,
                                active_count,
                                ..
                            } => {
                                let now_active = active_count > 0
                                    || total_download_bps > 0
                                    || total_upload_bps > 0;
                                if now_active || prev_stats_active {
                                    dirty_ev.store(true, Ordering::Release);
                                }
                                prev_stats_active = now_active;
                            }
                            EngineEvent::Updated { .. }
                            | EngineEvent::Removed { .. }
                            | EngineEvent::TrafficModeChanged { .. }
                            | EngineEvent::SettingsUpdated => {
                                dirty_ev.store(true, Ordering::Release);
                            }
                        }
                    }
                    Ok(()) = rx_raise.recv() => {
                        q_ev.push(UiAsyncMsg::RaiseRequested);
                        dirty_ev.store(true, Ordering::Release);
                    }
                    else => break,
                }
            }
        });

        let dirty_timer = Arc::clone(&engine_dirty);
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let keep = this
                    .update(cx, |app, cx| {
                        let had_msgs =
                            app.msgs.has_pending() || crate::macos_tray::has_pending_tray_actions();
                        if had_msgs {
                            app.drain_async_messages(cx);
                        }
                        let ev_dirty = dirty_timer.swap(false, Ordering::AcqRel);
                        let has_active = app.summary.active_count > 0
                            || app.all_downloads.iter().any(|d| {
                                d.state.is_running_or_seeding() || d.state == DownloadState::Queued
                            });
                        let toast_expired = app
                            .toast
                            .as_ref()
                            .is_some_and(|(_, _, when)| when.elapsed() > Duration::from_secs(5));
                        let closing_done = app
                            .inspector_closing_since
                            .as_ref()
                            .is_some_and(|t| t.elapsed() >= Duration::from_millis(200));
                        if had_msgs || ev_dirty || has_active || toast_expired || closing_done {
                            if closing_done {
                                app.inspector_closing_since = None;
                            }
                            app.refresh_snapshot(cx);
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !keep {
                    break;
                }
            },
        )
        .detach();

        let mk_input = |cx: &mut Context<Self>, ph: &str, init: &str| -> Entity<TextInput> {
            cx.new(|cx| {
                let mut t = TextInput::new(cx, ph.to_string(), init);
                t.apply_colors(
                    theme.bg_input,
                    theme.border,
                    theme.border_focus,
                    theme.text_primary,
                    theme.text_muted,
                    theme.accent,
                );
                t.apply_appearance(theme.font_scale(), theme.ui_scale(), theme.is_classic());
                t
            })
        };

        let mk_masked = |cx: &mut Context<Self>, ph: &str, init: &str| -> Entity<TextInput> {
            cx.new(|cx| {
                let mut t = TextInput::new(cx, ph.to_string(), init);
                t.masked = true;
                t.apply_colors(
                    theme.bg_input,
                    theme.border,
                    theme.border_focus,
                    theme.text_primary,
                    theme.text_muted,
                    theme.accent,
                );
                t.apply_appearance(theme.font_scale(), theme.ui_scale(), theme.is_classic());
                t
            })
        };

        let def_dir_s = cfg
            .core
            .default_download_path
            .to_string_lossy()
            .into_owned();
        let init_tum = cfg.core.current_tum;
        let init_prof = cfg.active_profile();
        let loc = i18n::l(&cfg.app.locale_name);

        let initial_url = launch.initial_url.unwrap_or_default();
        let use_standalone_create = cfg.app.enable_standalone_create_downloads_windows;
        let initial_modal = if !initial_url.is_empty() && !use_standalone_create {
            ActiveModal::AddDownload
        } else {
            ActiveModal::None
        };

        let add_url_input = mk_input(
            cx,
            "https://..., magnet:?xt=..., or .torrent path",
            &initial_url,
        );

        let search_input = cx.new(|cx| {
            let mut t = TextInput::new(cx, format_platform_shortcuts(loc.search_placeholder), "");
            t.borderless = true;
            t.apply_colors(
                theme.bg_surface,
                theme.border,
                theme.border_focus,
                theme.text_primary,
                theme.text_muted,
                theme.accent,
            );
            t.apply_appearance(theme.font_scale(), theme.ui_scale(), theme.is_classic());
            t
        });

        let wb_small_val = if cfg.core.wb_small_size_unit_mb {
            cfg.app.wb_too_small_size_bytes / (1024 * 1024)
        } else {
            cfg.app.wb_too_small_size_bytes / 1024
        };
        let init_sound_s = cfg
            .app
            .notification_sounds
            .get(&NotificationEvent::DownloadComplete)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut app = Self {
            rt,
            engine: Arc::clone(&engine),
            msgs,
            focus_handle: cx.focus_handle(),
            theme,
            active_page: ActivePage::Downloads,
            summary: engine.summary(),
            all_downloads: Vec::new(),
            visible_downloads: Vec::new(),
            tags: engine.list_tags().unwrap_or_default(),
            selected_id: None,
            parent_folder_id: None,
            hovered_row_id: None,
            checked_ids: HashSet::new(),
            active_filter: StateFilter::All,
            active_tag_id: None,
            sort_by: cfg.app.sort_by,
            sort_ascending: cfg.app.sort_ascending,
            inspector_tab: InspectorTab::General,
            inspector_expanded: true,
            last_non_snail_tum: if init_tum == TrafficUsageMode::Snail {
                TrafficUsageMode::High
            } else {
                init_tum
            },
            active_popover: ActivePopover::None,
            shutdown_submenu_open: false,
            active_modal: initial_modal.clone(),
            delete_remember_choice: false,
            toast: None,

            modal_anim_epoch: if initial_modal == ActiveModal::None {
                0
            } else {
                1
            },
            popover_anim_epoch: 0,
            plugins_gear_anim_epoch: 0,
            inspector_anim_epoch: 0,
            inspector_closing_since: None,
            toast_anim_epoch: 0,
            page_anim_epoch: 0,
            settings_tab_anim_epoch: 0,
            inspector_tab_anim_epoch: 0,
            adv_panel_anim_epoch: 0,
            probe_anim_epoch: if !initial_url.is_empty() && !use_standalone_create {
                1
            } else {
                0
            },
            shutdown_sub_anim_epoch: 0,
            proxy_box_anim_epoch: 0,
            last_dock_badge: None,
            window_active: !launch.hidden,
            manual_torrent_check_pending: false,

            search_input,

            add_url_input,
            add_dir_input: mk_input(cx, "Destination directory", &def_dir_s),
            add_name_input: mk_input(cx, "Optional output filename", ""),
            add_ref_input: mk_input(cx, "HTTP Referer (optional)", ""),
            add_user_input: mk_input(cx, "Username (optional)", ""),
            add_pass_input: mk_masked(cx, "Password (optional)", ""),
            add_show_pass: false,
            add_priority: DownloadPriority::Normal,
            add_start_paused: false,
            add_play_asap: false,
            add_apply_schedule: cfg.core.enable_scheduler_for_new_downloads,
            add_show_advanced: false,
            add_probe_busy: false,
            add_probe_outcome: None,
            add_selected_files: HashSet::new(),
            add_selected_variant: 0,

            batch_pattern_input: mk_input(
                cx,
                "https://host/files/part-[01-20].zip or multiline URLs",
                "",
            ),
            batch_group_title_input: mk_input(cx, "Optional group folder name", ""),
            batch_dir_input: mk_input(cx, "Target directory", &def_dir_s),
            batch_start_paused: false,
            batch_as_group: true,

            hash_algo: HashAlgorithm::Sha256,
            hash_expected_input: mk_input(cx, "Optional expected hex digest to verify against", ""),
            hash_computed: None,
            hash_matched: None,

            convert_mode_mp3: true,
            convert_mp3_bitrate: 320,
            convert_use_vbr: false,
            convert_vbr_quality: 2,
            convert_stream_copy: true,
            convert_out_input: mk_input(cx, "Output file path", ""),
            convert_busy: false,
            convert_status: None,

            remote_pairing: None,
            remote_loading: false,
            remote_error: None,

            new_tag_name_input: mk_input(cx, "Tag name (e.g. Research ISOs)", ""),
            new_tag_color_hex: "#3b82f6".into(),
            tag_exts_input: mk_input(cx, "zip tar.gz iso", ""),
            tag_dir_input: mk_input(cx, "Optional folder path for this tag", ""),

            source_url_input: mk_input(cx, "Primary download URL", ""),
            source_mirrors_input: mk_input(cx, "Comma-separated mirror URLs", ""),

            move_dir_input: mk_input(cx, "New folder path", &def_dir_s),
            rename_file_input: mk_input(cx, "New file name", ""),
            portable_dir_input: mk_input(
                cx,
                "Target portable directory",
                &engine.data_dir().join("ferrite-portable").to_string_lossy(),
            ),

            sched_enabled: cfg.core.default_schedule.enabled,
            sched_days_mask: cfg.core.default_schedule.days_mask,
            sched_start_input: mk_input(
                cx,
                "HH:MM",
                &format_minute_hhmm(cfg.core.default_schedule.start_minute),
            ),
            sched_end_input: mk_input(
                cx,
                "HH:MM",
                &format_minute_hhmm(cfg.core.default_schedule.end_minute),
            ),

            bt_add_tracker_input: mk_input(cx, "udp://tracker.example.org:6969/announce", ""),

            plugins_title_clicks: 0,
            show_plugin_dev: false,
            plugins_gear_menu_open: false,
            plugin_install_input: mk_input(cx, "/path/to/plugin.js or plugin manifest folder", ""),

            settings_plugins: engine.list_js_plugins(),
            settings_ssl_exceptions: init_ssl,
            prefs_title_clicks: 0,
            show_troubleshooting: false,
            show_proxy_pass: false,
            show_remote_pass: false,

            set_default_dir: mk_input(cx, "Default folder", &def_dir_s),
            set_partial_suffix: mk_input(cx, ".part", &cfg.core.partial_download_suffix),
            set_keep_days: mk_input(
                cx,
                "Days",
                &cfg.core.auto_remove_finished_keep_days.to_string(),
            ),
            set_max_retries: mk_input(cx, "Retries", &cfg.core.max_auto_retries.to_string()),
            set_slow_speed_kb: mk_input(
                cx,
                "KB/s (0 = off)",
                &format_kb_limit(cfg.core.unacceptably_slow_speed_bps),
            ),
            set_slow_interval_s: mk_input(
                cx,
                "60",
                &cfg.core.unacceptably_slow_interval_secs.to_string(),
            ),
            set_max_query_dl: mk_input(
                cx,
                "2",
                &cfg.core.max_additional_query_info_downloads.to_string(),
            ),
            set_max_small_dl: mk_input(
                cx,
                "2",
                &cfg.core.max_additional_small_downloads.to_string(),
            ),
            set_max_slow_dl: mk_input(
                cx,
                "2",
                &cfg.core.max_additional_downloads_if_slow.to_string(),
            ),
            set_tum_dl_kb: mk_input(
                cx,
                "0 = unlimited",
                &format_kb_limit(init_prof.max_download_speed_bps),
            ),
            set_tum_ul_kb: mk_input(
                cx,
                "0 = unlimited",
                &format_kb_limit(init_prof.max_upload_speed_bps),
            ),
            set_tum_conns: mk_input(cx, "Connections", &init_prof.max_connections.to_string()),
            set_tum_per_srv: mk_input(
                cx,
                "Per server",
                &init_prof.max_connections_per_server.to_string(),
            ),
            set_tum_concurrent: mk_input(
                cx,
                "Concurrent",
                &init_prof.max_concurrent_downloads.to_string(),
            ),
            set_tum_ratio: mk_input(
                cx,
                "0.0 = unlimited",
                &format!("{:.1}", init_prof.max_upload_ratio),
            ),
            set_proxy_host: mk_input(cx, "127.0.0.1", &cfg.app.proxy.http.host),
            set_proxy_port: mk_input(cx, "8080", &cfg.app.proxy.http.port.to_string()),
            set_socks5_host: mk_input(cx, "127.0.0.1", &cfg.app.proxy.socks5.host),
            set_socks5_port: mk_input(cx, "1080", &cfg.app.proxy.socks5.port.to_string()),
            set_proxy_user: mk_input(cx, "Username", &cfg.app.proxy.http.username),
            set_proxy_pass: mk_masked(cx, "Password", &cfg.app.proxy.http.password),
            set_bt_port: mk_input(cx, "6881", &cfg.core.bt_session_port.to_string()),
            set_bt_watch_dir: mk_input(
                cx,
                "Optional watch folder for .torrent files",
                &cfg.app
                    .torrents_watch_folder
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            set_bt_trackers: mk_input(
                cx,
                "Comma-separated default tracker URLs",
                &cfg.core.bt_tracker_list.join(", "),
            ),
            set_sched_start: mk_input(
                cx,
                "00:00",
                &format_minute_hhmm(cfg.core.default_schedule.start_minute),
            ),
            set_sched_end: mk_input(
                cx,
                "06:00",
                &format_minute_hhmm(cfg.core.default_schedule.end_minute),
            ),
            set_battery_min: mk_input(cx, "15", &cfg.core.battery_min_level_percent.to_string()),
            set_wb_small_kb: mk_input(cx, "100", &wb_small_val.to_string()),
            set_wb_skip_ext: mk_input(cx, "pls m3u", &cfg.app.wb_unwanted_extensions.join(" ")),
            set_wb_only_ext: mk_input(
                cx,
                "zip iso mp4",
                &cfg.app.wb_intercept_extensions.join(" "),
            ),
            set_wb_skip_hosts: mk_input(
                cx,
                "mega.nz rapidgator.net",
                &cfg.app.wb_unwanted_hosts.join(", "),
            ),
            set_ffmpeg_path: mk_input(
                cx,
                "/opt/homebrew/bin/ffmpeg",
                &cfg.core
                    .ffmpeg_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            set_ytdlp_path: mk_input(
                cx,
                "Optional yt-dlp path",
                &cfg.core
                    .yt_dlp_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            set_av_path: mk_input(
                cx,
                "Antivirus binary path",
                &cfg.core
                    .antivirus_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            set_av_args: mk_input(cx, "\"{file}\"", &cfg.core.antivirus_args),
            set_ext_path: mk_input(
                cx,
                "Post-completion binary/script path",
                &cfg.core
                    .external_app_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            set_ext_args: mk_input(cx, "\"{file}\"", &cfg.core.external_app_args_template),
            set_remote_bind: mk_input(cx, "0.0.0.0", &cfg.app.remote_access_bind),
            set_remote_port: mk_input(cx, "9443", &cfg.app.remote_access_port.to_string()),
            set_remote_pass: mk_masked(cx, "Remote access password", &init_remote_pw),
            set_notify_sound_path: mk_input(cx, "/System/Library/Sounds/Glass.aiff", &init_sound_s),
            set_low_disk_mb: mk_input(
                cx,
                "256",
                &(cfg.core.low_disk_space_min_bytes / (1024 * 1024)).to_string(),
            ),
            set_low_disk_pct: mk_input(
                cx,
                "2.0",
                &format!("{:.1}", cfg.core.low_disk_space_min_percent),
            ),
            set_backup_interval_m: mk_input(
                cx,
                "60",
                &(cfg.app.db_backup_interval_secs.max(0) / 60).to_string(),
            ),
            set_batch_max_urls: mk_input(
                cx,
                "1000",
                &cfg.app.batch_download_max_urls_count.to_string(),
            ),
            set_data_io_path: mk_input(
                cx,
                "Path to .json export file",
                &engine
                    .data_dir()
                    .join("ferrite-downloads-export.json")
                    .to_string_lossy(),
            ),
            set_diag_zip_path: mk_input(
                cx,
                "Path for diagnostics .zip",
                &engine
                    .data_dir()
                    .join("ferrite-diagnostics.zip")
                    .to_string_lossy(),
            ),
            settings_cfg: cfg,
            settings_tum_tab: init_tum,
            settings_dirty: false,
            downloads_scroll_handle: UniformListScrollHandle::new(),
            context_menu_pos: None,
            cached_add_disk_free: None,
            cached_native_host_installed: HashMap::new(),
            cached_antivirus_presets: Vec::new(),
            macos_tray: MacosTrayController::new(),
        };

        let weak_close = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            weak_close
                .update(cx, |view, cx| {
                    view.confirm_or_hide_on_window_close(window, cx)
                })
                .unwrap_or(true)
        });

        cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            cx.notify();
        })
        .detach();

        cx.observe(&app.search_input, |this, _, cx| {
            this.refresh_snapshot(cx);
            cx.notify();
        })
        .detach();

        cx.observe(&app.add_dir_input, |this, inp, cx| {
            let d = inp.read(cx).text();
            this.refresh_disk_space_cache(&d);
            cx.notify();
        })
        .detach();

        for watched_input in [
            &app.add_url_input,
            &app.add_name_input,
            &app.batch_pattern_input,
        ] {
            cx.observe(watched_input, |_, _, cx| cx.notify()).detach();
        }

        for settings_input in [
            &app.set_default_dir,
            &app.set_partial_suffix,
            &app.set_keep_days,
            &app.set_max_retries,
            &app.set_slow_speed_kb,
            &app.set_slow_interval_s,
            &app.set_max_query_dl,
            &app.set_max_small_dl,
            &app.set_max_slow_dl,
            &app.set_tum_dl_kb,
            &app.set_tum_ul_kb,
            &app.set_tum_conns,
            &app.set_tum_per_srv,
            &app.set_tum_concurrent,
            &app.set_tum_ratio,
            &app.set_proxy_host,
            &app.set_proxy_port,
            &app.set_socks5_host,
            &app.set_socks5_port,
            &app.set_proxy_user,
            &app.set_proxy_pass,
            &app.set_bt_port,
            &app.set_bt_watch_dir,
            &app.set_bt_trackers,
            &app.set_sched_start,
            &app.set_sched_end,
            &app.set_battery_min,
            &app.set_wb_small_kb,
            &app.set_wb_skip_ext,
            &app.set_wb_only_ext,
            &app.set_wb_skip_hosts,
            &app.set_ffmpeg_path,
            &app.set_ytdlp_path,
            &app.set_av_path,
            &app.set_av_args,
            &app.set_ext_path,
            &app.set_ext_args,
            &app.set_remote_bind,
            &app.set_remote_port,
            &app.set_remote_pass,
            &app.set_notify_sound_path,
            &app.set_low_disk_mb,
            &app.set_low_disk_pct,
            &app.set_backup_interval_m,
            &app.set_batch_max_urls,
            &app.set_data_io_path,
            &app.set_diag_zip_path,
        ] {
            cx.observe(settings_input, |this, _, cx| {
                this.settings_dirty = true;
                cx.notify();
            })
            .detach();
        }

        app.refresh_settings_caches();
        app.refresh_disk_space_cache(&def_dir_s);

        set_macos_hide_dock_icon(app.settings_cfg.app.hide_dock_icon);
        if app.settings_cfg.app.launch_minimized && !launch.hidden {
            window.minimize_window();
        }
        if app.settings_cfg.app.check_if_default_torrent_client {
            let q = app.msgs.clone();
            app.rt.spawn(async move {
                let is_default = is_default_torrent_client().await;
                q.push(UiAsyncMsg::DefaultTorrentCheckResult { is_default });
            });
        }

        app.refresh_snapshot(cx);
        if app.settings_cfg.app.auto_check_updates {
            app.check_for_app_updates_action(false, cx);
        }
        if app.settings_cfg.app.update_plugins_automatically {
            app.check_plugin_updates_action(false, cx);
        }
        if !initial_url.is_empty() && use_standalone_create {
            let rt_sa = Arc::clone(&app.rt);
            let eng_sa = Arc::clone(&app.engine);
            let url_sa = initial_url;
            cx.spawn(async move |_, cx: &mut gpui::AsyncApp| {
                let _ = cx.update(|cx| open_standalone_add_window(rt_sa, eng_sa, url_sa, cx));
            })
            .detach();
            window.focus(&app.focus_handle);
        } else if !initial_url.is_empty() {
            let fh = app.add_url_input.read(cx).focus_handle.clone();
            window.focus(&fh);
            app.trigger_probe(cx);
        } else {
            window.focus(&app.focus_handle);
        }
        app
    }
}

impl Render for FerriteApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.theme;
        let modal_epoch = self.modal_anim_epoch;
        let toast_epoch = self.toast_anim_epoch;
        let page_epoch = self.page_anim_epoch;

        let titlebar = self.render_window_titlebar();
        let top_bar = self.render_top_bar(cx);
        let main_content: AnyElement = match self.active_page {
            ActivePage::Downloads => {
                let table = self.render_downloads_table(cx);
                let bottom_panel = render_bottom_panel(self, cx);
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(table)
                    .child(bottom_panel)
                    .into_any_element()
            }
            ActivePage::Settings(_) => render_settings_page(self, cx),
            ActivePage::Plugins => render_plugins_page(self, cx),
        };
        let status_bar = self.render_status_bar(cx);
        let popover = render_popover(self, cx);
        let modal = render_active_modal(self, window, cx);
        let toast = self.toast.clone();

        let root = attach_root_actions(
            div()
                .relative()
                .size_full()
                .bg(p.bg_100)
                .text_color(p.text_primary)
                .key_context("FerriteList")
                .track_focus(&self.focus_handle),
            cx,
        );

        root.flex()
            .flex_col()
            .child(titlebar)
            .child(top_bar)
            .when_some(toast, |el, (msg, is_err, _)| {
                el.child(
                    div()
                        .id("toast-banner")
                        .px_4()
                        .py(p.spx(5.))
                        .bg(if is_err { p.danger } else { p.accent_soft })
                        .border_b_1()
                        .border_color(p.border)
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(p.fpx(12.))
                                .text_color(if is_err { p.on_accent } else { p.text_primary })
                                .child(msg),
                        )
                        .child(ghost_icon_btn(
                            "toast-dismiss",
                            "icons/close_sm.svg",
                            p.spx_f32(13.),
                            if is_err { p.on_accent } else { p.text_muted },
                            p.bg_elevated,
                            cx,
                            |this, _, _, cx| {
                                this.toast = None;
                                cx.notify();
                            },
                        ))
                        .with_animation(
                            SharedString::from(format!("toast-enter-{toast_epoch}")),
                            Animation::new(Duration::from_millis(160))
                                .with_easing(ease_out_quint()),
                            |el, delta| el.opacity(delta).mt(px(-6.0 * (1.0 - delta))),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(main_content)
                    .with_animation(
                        SharedString::from(format!("page-enter-{page_epoch}")),
                        Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                        |el, d| {
                            el.opacity((0.35 + 0.65 * d).min(1.0))
                                .pt(px(8.0 * (1.0 - d)))
                        },
                    ),
            )
            .child(status_bar)
            .when_some(popover, |el, pop| el.child(pop))
            .when_some(modal, |el, modal_card| {
                el.child(
                    div()
                        .id("modal-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(p.modal_backdrop)
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_6()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, w, cx| {
                                this.close_modal(w, cx);
                            }),
                        )
                        .child(modal_card)
                        .with_animation(
                            SharedString::from(format!("modal-backdrop-fade-{modal_epoch}")),
                            Animation::new(Duration::from_millis(130))
                                .with_easing(ease_out_quint()),
                            |el, delta| el.opacity(delta),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_input_application_clears_socks5_creds_when_host_empty() {
        let mut p = ProxySettings::default();
        apply_proxy_inputs_to_settings(
            &mut p,
            "127.0.0.1".into(),
            8080,
            "".into(),
            1080,
            "alice".into(),
            "secret".into(),
        );
        assert_eq!(p.http.username, "alice");
        assert_eq!(p.http.password, "secret");
        assert_eq!(p.socks5.username, "");
        assert_eq!(p.socks5.password, "");

        apply_proxy_inputs_to_settings(
            &mut p,
            "127.0.0.1".into(),
            8080,
            "127.0.0.1".into(),
            1080,
            "alice".into(),
            "secret".into(),
        );
        assert_eq!(p.socks5.username, "alice");
        assert_eq!(p.socks5.password, "secret");
    }

    #[test]
    fn config_defaults_match_settings_input_fallbacks() {
        let cfg = FerriteConfig::default();
        assert_eq!(cfg.app.remote_access_port, 9443);
        assert_eq!(cfg.app.db_backup_interval_secs, 3600);
        assert_eq!(cfg.app.batch_download_max_urls_count, 1000);
        assert_eq!(cfg.core.unacceptably_slow_interval_secs, 60);
        assert_eq!(cfg.core.low_disk_space_min_bytes, 256 * 1024 * 1024);
        assert!((cfg.core.low_disk_space_min_percent - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn app_and_input_keymaps_dispatch_clipboard_actions_by_context() {
        let mut bindings = crate::text_input::text_input_key_bindings();
        bindings.extend(app_key_bindings());
        let keymap = gpui::Keymap::new(bindings);

        let primary_mod = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };

        let mut list_ctx = gpui::KeyContext::new_with_defaults();
        list_ctx.add("FerriteList");
        let list_only = [list_ctx.clone()];

        let mut input_ctx = gpui::KeyContext::new_with_defaults();
        input_ctx.add("FerriteInput");
        let list_with_input = [list_ctx, input_ctx];

        for (key, expected) in [
            ("v", "ferrite_input::InputPaste"),
            ("c", "ferrite_input::InputCopy"),
            ("x", "ferrite_input::InputCut"),
            ("a", "ferrite_input::InputSelectAll"),
        ] {
            let stroke =
                gpui::Keystroke::parse(&format!("{primary_mod}-{key}")).expect("valid keystroke");
            let (matched, _) = keymap.bindings_for_input(&[stroke], &list_with_input);
            assert_eq!(
                matched.first().map(|b| b.action().name()),
                Some(expected),
                "expected {primary_mod}-{key} in FerriteInput to dispatch {expected}, got {:?}",
                matched
                    .iter()
                    .map(|b| b.action().name())
                    .collect::<Vec<_>>()
            );
        }

        for (key, expected) in [
            ("v", "ferrite::PasteUrlAction"),
            ("c", "ferrite_input::InputCopy"),
            ("a", "ferrite_input::InputSelectAll"),
            ("shift-v", "ferrite::PasteUrlAction"),
            ("f", "ferrite::FocusSearchAction"),
            ("n", "ferrite::AddUrlAction"),
        ] {
            let stroke =
                gpui::Keystroke::parse(&format!("{primary_mod}-{key}")).expect("valid keystroke");
            let (matched, _) = keymap.bindings_for_input(&[stroke], &list_only);
            assert!(
                matched.iter().any(|b| b.action().name() == expected),
                "expected {primary_mod}-{key} in FerriteList to dispatch {expected}, got {:?}",
                matched
                    .iter()
                    .map(|b| b.action().name())
                    .collect::<Vec<_>>()
            );
        }

        #[cfg(not(target_os = "macos"))]
        {
            for (stroke_str, expected) in [
                ("ctrl-insert", "ferrite_input::InputCopy"),
                ("shift-insert", "ferrite::PasteUrlAction"),
                ("f11", "ferrite::ToggleFullscreenAction"),
            ] {
                let stroke = gpui::Keystroke::parse(stroke_str).expect("valid keystroke");
                let (matched, _) = keymap.bindings_for_input(&[stroke], &list_only);
                assert!(
                    matched.iter().any(|b| b.action().name() == expected),
                    "expected {stroke_str} in FerriteList to dispatch {expected}"
                );
            }
        }
    }
}
