use crate::app::{
    ActiveModal, ActivePage, ActivePopover, FerriteApp, InspectorTab, SettingsTab,
    apply_proxy_inputs_to_settings,
};
use crate::helpers::{
    UiAsyncMsg, browser_display_name, detect_antivirus_presets, detect_lan_ip,
    extract_first_download_url, format_compact_dock_badge, format_kb_limit, format_minute_hhmm,
    format_platform_shortcuts, install_native_host_for_browser, is_native_host_installed,
    macos_restore_windows_and_activate, open_path_os, parse_kb_limit, parse_list_tokens,
    parse_minute_hhmm, parse_wb_threshold_bytes, read_clipboard_text, reveal_path_os,
    set_macos_hide_dock_icon, sync_portable_marker, update_macos_dock_badge, write_clipboard_text,
    zoom_rem_size_px,
};
use crate::i18n;
use crate::macos_tray::{TrayAction, drain_tray_actions};
use crate::standalone::{open_standalone_add_window, open_standalone_download_window};
use crate::text_input::TextInput;
use crate::theme::ThemePalette;
use ferrite_core::os_integration::{
    disk_space_stats, is_default_torrent_client, play_notification_sound, send_os_notification,
    sync_autorun_on_login,
};
use ferrite_core::{
    DeleteButtonAction, DownloadId, DownloadPriority, DownloadState, DownloadType, FerriteConfig,
    FileExistsAction, FilterQuery, KnownWebBrowser, NewDownloadRequest, NotificationEvent,
    ScheduleRule, ShutdownType, SortField, StateFilter, ThemeMode, TrafficUsageMode,
};
use ferrite_media::{Mp3QualityOption, convert_to_mp3_with_options, convert_to_mp4};
use gpui::{App, Context, Entity, PathPromptOptions, ScrollStrategy, Window, px};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

impl FerriteApp {
    pub(crate) fn refresh_snapshot(&mut self, cx: &App) {
        self.summary = self.engine.summary();
        self.all_downloads = self.engine.list_downloads(&FilterQuery::default());
        self.tags = self.engine.list_tags().unwrap_or_default();
        if let Some(pid) = self.parent_folder_id
            && !self.all_downloads.iter().any(|d| d.id == pid)
        {
            self.parent_folder_id = None;
        }
        let query = FilterQuery {
            state_filter: self.active_filter,
            tag_id: self.active_tag_id,
            missing_filter: Default::default(),
            search_text: self.search_input.read(cx).text(),
            sort_by: self.sort_by,
            sort_ascending: self.sort_ascending,
            only_top_level: self.parent_folder_id.is_none(),
            parent_id_filter: self.parent_folder_id,
        };
        self.visible_downloads = self.engine.list_downloads(&query);
        self.checked_ids
            .retain(|id| self.all_downloads.iter().any(|d| d.id == *id));
        if let Some(sel) = self.selected_id {
            if !self.all_downloads.iter().any(|d| d.id == sel) {
                self.selected_id = self.visible_downloads.first().map(|d| d.id);
            }
        } else {
            self.selected_id = self.visible_downloads.first().map(|d| d.id);
        }

        if let Some((_, _, when)) = self.toast
            && when.elapsed() > Duration::from_secs(5)
        {
            self.toast = None;
        }

        self.refresh_dock_badge();
    }

    pub(crate) fn refresh_dock_badge(&mut self) {
        let cfg = &self.settings_cfg.app;
        let active_count = self
            .all_downloads
            .iter()
            .filter(|d| d.state.is_running_or_seeding() || d.state == DownloadState::Queued)
            .count();
        let next = if cfg.hide_dock_icon {
            None
        } else {
            format_compact_dock_badge(
                self.summary.total_download_bps,
                self.summary.total_upload_bps,
                active_count,
                cfg.show_total_upload_speed_in_dock,
                cfg.indicate_dl_ul_in_tray,
            )
        };
        if next != self.last_dock_badge {
            update_macos_dock_badge(next.as_deref(), active_count);
            self.last_dock_badge = next;
        }
        self.macos_tray.update(
            cfg.indicate_dl_ul_in_tray,
            self.summary.total_download_bps,
            self.summary.total_upload_bps,
            self.summary.tum == TrafficUsageMode::Snail,
        );
    }

    pub(crate) fn drain_async_messages(&mut self, cx: &mut Context<Self>) {
        for msg in self.msgs.drain() {
            match msg {
                UiAsyncMsg::Toast { text, is_error } => {
                    self.set_toast(text, is_error);
                }
                UiAsyncMsg::ProbeCompleted { outcome } => {
                    self.add_probe_busy = false;
                    self.add_dir_input.update(cx, |inp, cx| {
                        inp.set_text(&outcome.resolved_dir.to_string_lossy(), cx);
                    });
                    self.add_name_input.update(cx, |inp, cx| {
                        inp.set_text(&outcome.resolved_file_name, cx);
                    });
                    self.add_selected_files = outcome
                        .files
                        .iter()
                        .filter(|f| f.priority != DownloadPriority::DontDownload)
                        .map(|f| f.index)
                        .collect();
                    self.add_selected_variant = 0;
                    self.add_probe_outcome = Some(*outcome);
                    self.probe_anim_epoch = self.probe_anim_epoch.wrapping_add(1);
                }
                UiAsyncMsg::ProbeFailed { error } => {
                    self.add_probe_busy = false;
                    self.set_toast(format!("Probe failed: {error}"), true);
                }
                UiAsyncMsg::EnqueueFinished { count, label } => {
                    if matches!(
                        self.active_modal,
                        ActiveModal::AddDownload | ActiveModal::BatchAdd
                    ) {
                        self.active_modal = ActiveModal::None;
                    }
                    self.set_toast(format!("Enqueued {count} download(s): {label}"), false);
                }
                UiAsyncMsg::FileConflict { id, path } => {
                    self.set_active_modal(ActiveModal::FileConflict { id, path });
                }
                UiAsyncMsg::HashResult {
                    download_id,
                    algo,
                    digest,
                    matched,
                } => {
                    if matches!(self.active_modal, ActiveModal::HashVerify { id } if id == download_id)
                    {
                        self.hash_computed = Some(digest);
                        self.hash_matched = matched;
                    }
                    self.set_toast(format!("{} digest computed", algo.label()), false);
                }
                UiAsyncMsg::ConvertFinished { out_path } => {
                    self.convert_busy = false;
                    self.convert_status = Some(format!("Saved: {}", out_path.display()));
                    self.set_toast(format!("Converted -> {}", out_path.display()), false);
                }
                UiAsyncMsg::RemoteControlReady { pairing } => {
                    self.remote_loading = false;
                    self.remote_error = None;
                    self.remote_pairing = Some(pairing);
                }
                UiAsyncMsg::RemoteControlFailed { error } => {
                    self.remote_loading = false;
                    self.remote_error = Some(error.clone());
                    self.set_toast(format!("Remote server error: {error}"), true);
                }
                UiAsyncMsg::RemotePasswordRegenerated { password } => {
                    self.settings_cfg.app.remote_access_password = password.clone();
                    self.set_remote_pass
                        .update(cx, |i, cx| i.set_text(&password, cx));
                    self.set_toast("Regenerated Remote Access password", false);
                }
                UiAsyncMsg::PortableCreated { dir } => {
                    if self.active_modal == ActiveModal::CreatePortable {
                        self.active_modal = ActiveModal::None;
                    }
                    self.set_toast(
                        format!("Portable version created at {}", dir.display()),
                        false,
                    );
                }
                UiAsyncMsg::DiagnosticsArchived { zip_path } => {
                    self.set_toast(
                        format!("Saved diagnostics archive to {}", zip_path.display()),
                        false,
                    );
                }
                UiAsyncMsg::PluginActionFinished { message, is_error } => {
                    self.settings_plugins = self.engine.list_js_plugins();
                    self.set_toast(message, is_error);
                }
                UiAsyncMsg::PluginUpdatesFinished { updated, manual } => {
                    self.settings_plugins = self.engine.list_js_plugins();
                    if !updated.is_empty() {
                        let summary: Vec<String> = updated
                            .iter()
                            .map(|u| format!("{} v{}", u.name, u.new_version))
                            .collect();
                        self.set_toast(
                            format!(
                                "Updated {} add-on(s): {}",
                                updated.len(),
                                summary.join(", ")
                            ),
                            false,
                        );
                    } else if manual {
                        self.set_toast("All installed add-ons are up to date", false);
                    }
                }
                UiAsyncMsg::AppUpdateCheckFinished { result, manual } => {
                    if result.update_available || manual {
                        self.set_toast(result.message, false);
                    }
                }
                UiAsyncMsg::VirusCheckFinished {
                    download_id,
                    clean,
                    summary,
                } => {
                    self.set_toast(format!("#{download_id}: {summary}"), !clean);
                }
                UiAsyncMsg::UpdateCheckFinished {
                    download_id,
                    changed,
                    message,
                } => {
                    let state = if changed {
                        "update available"
                    } else {
                        "up to date"
                    };
                    self.set_toast(format!("#{download_id} ({state}): {message}"), false);
                }
                UiAsyncMsg::DownloadAddedNotification { id } => {
                    let cfg = self.engine.get_config();
                    let allow_notify = !cfg.app.notify_when_inactive_only || !self.window_active;
                    if cfg.app.notify_added && allow_notify {
                        let title = self
                            .engine
                            .get_download(id)
                            .map(|r| r.title)
                            .unwrap_or_else(|| format!("Download #{id}"));
                        send_os_notification("Download Added", &title);
                    }
                }
                UiAsyncMsg::DownloadCompletedNotification { id } => {
                    let title = self
                        .engine
                        .get_download(id)
                        .map(|r| r.title)
                        .unwrap_or_else(|| format!("Download #{id}"));
                    self.set_toast(format!("Download #{id} completed: {title}"), false);
                    let cfg = self.engine.get_config();
                    let allow_notify = !cfg.app.notify_when_inactive_only || !self.window_active;
                    if cfg.app.notify_finished && allow_notify {
                        send_os_notification("Download Completed", &title);
                    }
                    if cfg.app.enable_sound_notifications {
                        let custom = cfg
                            .app
                            .notification_sounds
                            .get(&NotificationEvent::DownloadComplete)
                            .map(|p| p.as_path());
                        play_notification_sound(custom);
                    }
                }
                UiAsyncMsg::DownloadFailedNotification { id, error } => {
                    let title = self
                        .engine
                        .get_download(id)
                        .map(|r| r.title)
                        .unwrap_or_else(|| format!("Download #{id}"));
                    self.set_toast(format!("Download #{id} failed: {error}"), true);
                    let cfg = self.engine.get_config();
                    let allow_notify = !cfg.app.notify_when_inactive_only || !self.window_active;
                    if cfg.app.notify_failed && allow_notify {
                        send_os_notification("Download Failed", &format!("{title}: {error}"));
                    }
                }
                UiAsyncMsg::DefaultTorrentCheckResult { is_default } => {
                    let manual = std::mem::take(&mut self.manual_torrent_check_pending);
                    if manual {
                        if is_default {
                            self.set_toast("Ferrite is the default BitTorrent client", false);
                        } else {
                            self.set_toast(
                                "Ferrite is not the default .torrent / magnet handler in macOS",
                                true,
                            );
                        }
                    } else if !is_default && self.settings_cfg.app.check_if_default_torrent_client {
                        self.set_toast(
                            "Ferrite is not set as the default BitTorrent client",
                            false,
                        );
                    }
                }
                UiAsyncMsg::QuitRequested => {
                    cx.quit();
                }
                UiAsyncMsg::RaiseRequested => {
                    macos_restore_windows_and_activate();
                    cx.activate(true);
                }
                UiAsyncMsg::DiskSpaceUpdated { path, free_bytes } => {
                    self.cached_add_disk_free = Some((path, free_bytes));
                }
            }
        }

        for tray_act in drain_tray_actions() {
            match tray_act {
                TrayAction::ShowFerrite => {
                    macos_restore_windows_and_activate();
                    cx.activate(true);
                }
                TrayAction::AddDownload => {
                    macos_restore_windows_and_activate();
                    cx.activate(true);
                    if let Some(g) = cx.try_global::<crate::app::AppWindowGlobal>().cloned() {
                        cx.defer(move |cx| {
                            let _ = g.window.update(cx, |view, window, cx| {
                                view.open_add_modal("", window, cx);
                            });
                        });
                    }
                }
                TrayAction::StartAll => {
                    let eng = Arc::clone(&self.engine);
                    self.rt.spawn(async move {
                        let _ = eng.start_all().await;
                    });
                }
                TrayAction::PauseAll => {
                    let eng = Arc::clone(&self.engine);
                    self.rt.spawn(async move {
                        let _ = eng.pause_all().await;
                    });
                }
                TrayAction::ToggleSnailMode => {
                    self.toggle_snail_mode(cx);
                }
                TrayAction::Quit => {
                    self.request_quit(cx);
                }
            }
        }
    }

    pub(crate) fn set_toast(&mut self, text: impl Into<String>, is_error: bool) {
        self.toast = Some((text.into(), is_error, Instant::now()));
        self.toast_anim_epoch = self.toast_anim_epoch.wrapping_add(1);
    }

    pub(crate) fn set_active_modal(&mut self, modal: ActiveModal) {
        if self.active_modal != modal {
            self.active_modal = modal;
            if self.active_modal != ActiveModal::None {
                self.modal_anim_epoch = self.modal_anim_epoch.wrapping_add(1);
            }
        }
    }

    pub(crate) fn refresh_disk_space_cache(&mut self, raw_dir: &str) {
        let expanded = crate::helpers::expand_path_preview(raw_dir, "", "");
        if let Some((ref cached_dir, _)) = self.cached_add_disk_free
            && cached_dir == &expanded
        {
            return;
        }
        let q = self.msgs.clone();
        self.rt.spawn_blocking(move || {
            let mut probe = PathBuf::from(&expanded);
            while !probe.exists() {
                if !probe.pop() {
                    probe = PathBuf::from("/");
                    break;
                }
            }
            let free = disk_space_stats(&probe).map(|s| s.available_bytes);
            q.push(UiAsyncMsg::DiskSpaceUpdated {
                path: expanded,
                free_bytes: free,
            });
        });
    }

    pub(crate) fn refresh_settings_caches(&mut self) {
        let browsers = [
            KnownWebBrowser::Chrome,
            KnownWebBrowser::Firefox,
            KnownWebBrowser::Edge,
            KnownWebBrowser::Brave,
            KnownWebBrowser::Opera,
            KnownWebBrowser::Vivaldi,
        ];
        self.cached_native_host_installed = browsers
            .into_iter()
            .map(|b| (b, is_native_host_installed(b)))
            .collect();
        self.cached_antivirus_presets = detect_antivirus_presets();
    }

    pub(crate) fn select_adjacent_download(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.visible_downloads.len();
        if len == 0 {
            return;
        }
        let cur_idx = self
            .selected_id
            .and_then(|id| self.visible_downloads.iter().position(|d| d.id == id));
        let next_idx = match cur_idx {
            Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
            None => 0,
        };
        if let Some(rec) = self.visible_downloads.get(next_idx) {
            self.selected_id = Some(rec.id);
            self.downloads_scroll_handle
                .scroll_to_item(next_idx, ScrollStrategy::Center);
            cx.notify();
        }
    }

    pub(crate) fn activate_selected_download(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected_id else {
            return;
        };
        let Some(rec) = self.visible_downloads.iter().find(|d| d.id == id) else {
            return;
        };
        if rec.download_type == DownloadType::Batch {
            self.parent_folder_id = Some(id);
            self.refresh_snapshot(cx);
        } else if rec.state == DownloadState::Completed {
            self.open_download_file(id);
        } else {
            self.toggle_download(id);
        }
        cx.notify();
    }

    pub(crate) fn set_inspector_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        if !self.inspector_expanded && expanded {
            self.inspector_closing_since = None;
            self.inspector_anim_epoch = self.inspector_anim_epoch.wrapping_add(1);
        } else if self.inspector_expanded && !expanded {
            self.inspector_closing_since = Some(Instant::now());
            self.inspector_anim_epoch = self.inspector_anim_epoch.wrapping_add(1);
        }
        self.inspector_expanded = expanded;
        cx.notify();
    }

    pub(crate) fn set_active_page(&mut self, page: ActivePage, cx: &mut Context<Self>) {
        let same_discriminant =
            std::mem::discriminant(&self.active_page) == std::mem::discriminant(&page);
        if self.active_page != page {
            if !same_discriminant {
                self.page_anim_epoch = self.page_anim_epoch.wrapping_add(1);
            }
            if let (ActivePage::Settings(old_tab), ActivePage::Settings(new_tab)) =
                (self.active_page, page)
                && old_tab != new_tab
            {
                self.settings_tab_anim_epoch = self.settings_tab_anim_epoch.wrapping_add(1);
            }
            self.active_page = page;
            cx.notify();
        }
    }

    pub(crate) fn set_settings_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        self.set_active_page(ActivePage::Settings(tab), cx);
    }

    pub(crate) fn set_inspector_tab(&mut self, tab: InspectorTab, cx: &mut Context<Self>) {
        if self.inspector_tab != tab {
            self.inspector_tab = tab;
            self.inspector_tab_anim_epoch = self.inspector_tab_anim_epoch.wrapping_add(1);
        }
        self.set_inspector_expanded(true, cx);
    }

    pub(crate) fn check_for_app_updates_action(&mut self, manual: bool, cx: &mut Context<Self>) {
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        if manual {
            self.set_toast("Checking for Ferrite updates…", false);
        }
        self.rt.spawn(async move {
            match eng.check_for_app_updates(None).await {
                Ok(result) => q.push(UiAsyncMsg::AppUpdateCheckFinished { result, manual }),
                Err(e) => {
                    if manual {
                        q.push(UiAsyncMsg::Toast {
                            text: format!("App update check failed: {e}"),
                            is_error: true,
                        });
                    }
                }
            }
        });
        cx.notify();
    }

    pub(crate) fn check_plugin_updates_action(&mut self, manual: bool, cx: &mut Context<Self>) {
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        if manual {
            self.set_toast("Checking for add-on updates…", false);
        }
        self.rt.spawn(async move {
            match eng.check_and_update_js_plugins().await {
                Ok(updated) => q.push(UiAsyncMsg::PluginUpdatesFinished { updated, manual }),
                Err(e) => {
                    if manual {
                        q.push(UiAsyncMsg::Toast {
                            text: format!("Add-on update check failed: {e}"),
                            is_error: true,
                        });
                    }
                }
            }
        });
        cx.notify();
    }

    pub(crate) fn apply_theme_and_appearance_to_inputs(&self, cx: &mut Context<Self>) {
        let p = self.theme;
        let loc = i18n::l(&self.settings_cfg.app.locale_name);
        self.search_input.update(cx, |inp, cx| {
            inp.placeholder = format_platform_shortcuts(loc.search_placeholder).into();
            inp.apply_colors(
                p.bg_surface,
                p.border,
                p.border_focus,
                p.text_primary,
                p.text_muted,
                p.accent,
            );
            inp.apply_appearance(p.font_scale(), p.ui_scale(), p.is_classic());
            cx.notify();
        });
        let all_inputs = [
            &self.add_url_input,
            &self.add_dir_input,
            &self.add_name_input,
            &self.add_ref_input,
            &self.add_user_input,
            &self.add_pass_input,
            &self.batch_pattern_input,
            &self.batch_group_title_input,
            &self.batch_dir_input,
            &self.hash_expected_input,
            &self.convert_out_input,
            &self.new_tag_name_input,
            &self.tag_exts_input,
            &self.tag_dir_input,
            &self.source_url_input,
            &self.source_mirrors_input,
            &self.move_dir_input,
            &self.rename_file_input,
            &self.portable_dir_input,
            &self.sched_start_input,
            &self.sched_end_input,
            &self.bt_add_tracker_input,
            &self.plugin_install_input,
            &self.set_default_dir,
            &self.set_partial_suffix,
            &self.set_keep_days,
            &self.set_max_retries,
            &self.set_slow_speed_kb,
            &self.set_slow_interval_s,
            &self.set_max_query_dl,
            &self.set_max_small_dl,
            &self.set_max_slow_dl,
            &self.set_tum_dl_kb,
            &self.set_tum_ul_kb,
            &self.set_tum_conns,
            &self.set_tum_per_srv,
            &self.set_tum_concurrent,
            &self.set_tum_ratio,
            &self.set_proxy_host,
            &self.set_proxy_port,
            &self.set_socks5_host,
            &self.set_socks5_port,
            &self.set_proxy_user,
            &self.set_proxy_pass,
            &self.set_bt_port,
            &self.set_bt_watch_dir,
            &self.set_bt_trackers,
            &self.set_sched_start,
            &self.set_sched_end,
            &self.set_battery_min,
            &self.set_wb_small_kb,
            &self.set_wb_skip_ext,
            &self.set_wb_only_ext,
            &self.set_wb_skip_hosts,
            &self.set_ffmpeg_path,
            &self.set_ytdlp_path,
            &self.set_av_path,
            &self.set_av_args,
            &self.set_ext_path,
            &self.set_ext_args,
            &self.set_remote_bind,
            &self.set_remote_port,
            &self.set_remote_pass,
            &self.set_notify_sound_path,
            &self.set_low_disk_mb,
            &self.set_low_disk_pct,
            &self.set_backup_interval_m,
            &self.set_batch_max_urls,
            &self.set_data_io_path,
            &self.set_diag_zip_path,
        ];
        for e in all_inputs {
            e.update(cx, |inp, cx| {
                inp.apply_colors(
                    p.bg_input,
                    p.border,
                    p.border_focus,
                    p.text_primary,
                    p.text_muted,
                    p.accent,
                );
                inp.apply_appearance(p.font_scale(), p.ui_scale(), p.is_classic());
                cx.notify();
            });
        }
    }

    pub(crate) fn preview_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.theme = ThemePalette::from_config(
            self.settings_cfg.app.theme_mode,
            self.settings_cfg.app.ui_style,
            self.settings_cfg.app.overall_zoom_pct,
            self.settings_cfg.app.fonts_zoom_pct,
        );
        window.set_rem_size(px(zoom_rem_size_px(self.settings_cfg.app.overall_zoom_pct)));
        self.apply_theme_and_appearance_to_inputs(cx);
        cx.notify();
    }

    pub(crate) fn set_theme_mode(
        &mut self,
        mode: ThemeMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_cfg.app.theme_mode = mode;
        self.preview_appearance(window, cx);
    }

    pub(crate) fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let next_mode = if self.theme.is_dark {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        self.settings_cfg.app.theme_mode = next_mode;
        self.theme = ThemePalette::from_config(
            next_mode,
            self.settings_cfg.app.ui_style,
            self.settings_cfg.app.overall_zoom_pct,
            self.settings_cfg.app.fonts_zoom_pct,
        );
        let mut live = self.engine.get_config();
        live.app.theme_mode = next_mode;
        let _ = self.engine.update_config(live);
        self.apply_theme_and_appearance_to_inputs(cx);
        cx.notify();
    }

    pub(crate) fn adjust_zoom(
        &mut self,
        delta: Option<i16>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = match delta {
            None => 100,
            Some(d) => (self.settings_cfg.app.overall_zoom_pct as i16 + d).clamp(75, 150) as u16,
        };
        self.settings_cfg.app.overall_zoom_pct = next;
        if !matches!(self.active_page, ActivePage::Settings(_)) {
            let mut live = self.engine.get_config();
            live.app.overall_zoom_pct = next;
            let _ = self.engine.update_config(live);
        }
        self.preview_appearance(window, cx);
    }

    pub(crate) fn check_default_torrent_action(&mut self, cx: &mut Context<Self>) {
        self.manual_torrent_check_pending = true;
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            let is_default = is_default_torrent_client().await;
            q.push(UiAsyncMsg::DefaultTorrentCheckResult { is_default });
        });
        cx.notify();
    }

    pub(crate) fn browse_folder_into(
        &mut self,
        input: Entity<TextInput>,
        prompt: &'static str,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                if let Ok(Ok(Some(paths))) = rx.await
                    && let Some(chosen) = paths.into_iter().next()
                {
                    let s = chosen.to_string_lossy().into_owned();
                    let _ = this.update(cx, |_, cx| {
                        input.update(cx, |inp, cx| inp.set_text(&s, cx));
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    pub(crate) fn browse_file_into(
        &mut self,
        input: Entity<TextInput>,
        prompt: &'static str,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                if let Ok(Ok(Some(paths))) = rx.await
                    && let Some(chosen) = paths.into_iter().next()
                {
                    let s = chosen.to_string_lossy().into_owned();
                    let _ = this.update(cx, |_, cx| {
                        input.update(cx, |inp, cx| inp.set_text(&s, cx));
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    pub(crate) fn persist_sort(&mut self, cx: &mut Context<Self>) {
        self.settings_cfg.app.sort_by = self.sort_by;
        self.settings_cfg.app.sort_ascending = self.sort_ascending;
        let mut live = self.engine.get_config();
        live.app.sort_by = self.sort_by;
        live.app.sort_ascending = self.sort_ascending;
        let _ = self.engine.update_config(live);
        cx.notify();
    }

    pub(crate) fn open_download_window(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        open_standalone_download_window(Arc::clone(&self.rt), Arc::clone(&self.engine), id, cx);
    }

    pub(crate) fn open_about_modal(&mut self, cx: &mut Context<Self>) {
        self.active_popover = ActivePopover::None;
        self.set_active_modal(ActiveModal::About);
        cx.notify();
    }

    pub(crate) fn open_portable_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_popover = ActivePopover::None;
        self.set_active_modal(ActiveModal::CreatePortable);
        let fh = self.portable_dir_input.read(cx).focus_handle.clone();
        window.focus(&fh);
        cx.notify();
    }

    pub(crate) fn apply_state_filter_shortcut(
        &mut self,
        filter: StateFilter,
        cx: &mut Context<Self>,
    ) {
        self.set_active_page(ActivePage::Downloads, cx);
        self.active_tag_id = None;
        self.active_filter = filter;
        self.refresh_snapshot(cx);
        cx.notify();
    }

    pub(crate) fn handle_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_page == ActivePage::Plugins && self.plugins_gear_menu_open {
            self.plugins_gear_menu_open = false;
            cx.notify();
            return;
        }
        if self.active_popover != ActivePopover::None {
            self.close_popover(cx);
            return;
        }
        if self.active_modal != ActiveModal::None {
            self.close_modal(window, cx);
            return;
        }
        if matches!(self.active_page, ActivePage::Settings(_)) {
            let dirty = self.has_unsaved_settings_changes(cx);
            if dirty {
                self.set_toast("Discarded unsaved settings changes", false);
            }
            self.cancel_settings_internal(false, window, cx);
            return;
        }
        if self.active_page == ActivePage::Plugins {
            self.set_active_page(ActivePage::Downloads, cx);
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        if self.parent_folder_id.is_some() {
            self.parent_folder_id = None;
            self.refresh_snapshot(cx);
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        if !self.search_input.read(cx).text().is_empty() {
            self.search_input.update(cx, |i, cx| i.set_text("", cx));
            self.refresh_snapshot(cx);
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        if !self.checked_ids.is_empty() {
            self.checked_ids.clear();
            cx.notify();
            return;
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub(crate) fn confirm_or_hide_on_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let cfg = self.engine.get_config();
        if cfg.app.close_to_background {
            self.active_popover = ActivePopover::None;
            #[cfg(target_os = "macos")]
            {
                let _ = window;
                crate::helpers::macos_hide_all_windows();
            }
            #[cfg(not(target_os = "macos"))]
            {
                window.minimize_window();
                cx.hide();
            }
            return false;
        }
        let has_active = self
            .all_downloads
            .iter()
            .any(|d| d.state.is_running_or_seeding() || d.state == DownloadState::Queued);
        if cfg.app.quit_confirmation && has_active {
            self.active_popover = ActivePopover::None;
            self.set_active_modal(ActiveModal::QuitConfirm);
            cx.notify();
            return false;
        }
        true
    }

    pub(crate) fn handle_close_window_request(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.confirm_or_hide_on_window_close(window, cx) {
            window.remove_window();
        }
    }

    pub(crate) fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_popover != ActivePopover::None {
            self.active_popover = ActivePopover::None;
            self.shutdown_submenu_open = false;
            cx.notify();
            return;
        }
        self.active_modal = ActiveModal::None;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub(crate) fn close_popover(&mut self, cx: &mut Context<Self>) {
        if self.active_popover != ActivePopover::None {
            self.active_popover = ActivePopover::None;
            self.shutdown_submenu_open = false;
            cx.notify();
        }
    }

    pub(crate) fn toggle_popover(&mut self, pop: ActivePopover, cx: &mut Context<Self>) {
        self.shutdown_submenu_open = false;
        if self.active_popover == pop {
            self.active_popover = ActivePopover::None;
        } else {
            self.active_popover = pop;
            self.popover_anim_epoch = self.popover_anim_epoch.wrapping_add(1);
        }
        cx.notify();
    }

    pub(crate) fn toggle_check(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        if !self.checked_ids.remove(&id) {
            self.checked_ids.insert(id);
        }
        cx.notify();
    }

    pub(crate) fn toggle_select_all_visible(&mut self, cx: &mut Context<Self>) {
        let vis: Vec<DownloadId> = self.visible_downloads.iter().map(|d| d.id).collect();
        if vis.is_empty() {
            self.checked_ids.clear();
        } else {
            let all_selected = vis.iter().all(|id| self.checked_ids.contains(id));
            if all_selected {
                for id in vis {
                    self.checked_ids.remove(&id);
                }
            } else {
                for id in vis {
                    self.checked_ids.insert(id);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn batch_start_checked(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<DownloadId> = self.checked_ids.iter().copied().collect();
        let eng = Arc::clone(&self.engine);
        self.rt.spawn(async move {
            for id in ids {
                let _ = eng.resume_download(id).await;
            }
        });
        cx.notify();
    }

    pub(crate) fn batch_pause_checked(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<DownloadId> = self.checked_ids.iter().copied().collect();
        let eng = Arc::clone(&self.engine);
        self.rt.spawn(async move {
            for id in ids {
                let _ = eng.pause_download(id).await;
            }
        });
        cx.notify();
    }

    pub(crate) fn paste_from_clipboard_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let clip = read_clipboard_text(cx).unwrap_or_default();
        self.active_popover = ActivePopover::None;
        if let Some(url) = extract_first_download_url(&clip) {
            let standalone = self
                .engine
                .get_config()
                .app
                .enable_standalone_create_downloads_windows;
            self.open_add_modal(&url, window, cx);
            if !standalone {
                self.trigger_probe(cx);
            }
        } else {
            self.open_add_modal("", window, cx);
            if clip.trim().is_empty() {
                self.set_toast("Clipboard empty — enter a URL or magnet URI", false);
            }
        }
    }

    pub(crate) fn set_sort_column(&mut self, field: SortField, cx: &mut Context<Self>) {
        if self.sort_by == field {
            self.sort_ascending = !self.sort_ascending;
        } else {
            self.sort_by = field;
            self.sort_ascending = !matches!(
                field,
                SortField::CreationTime | SortField::Size | SortField::Progress
            );
        }
        self.persist_sort(cx);
        self.refresh_snapshot(cx);
        cx.notify();
    }

    pub(crate) fn open_add_modal(
        &mut self,
        prefill_url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cfg = self.engine.get_config();
        if cfg.app.enable_standalone_create_downloads_windows {
            self.active_popover = ActivePopover::None;
            open_standalone_add_window(
                Arc::clone(&self.rt),
                Arc::clone(&self.engine),
                prefill_url.to_string(),
                cx,
            );
            cx.notify();
            return;
        }
        let def_dir = cfg
            .core
            .default_download_path
            .to_string_lossy()
            .into_owned();
        self.add_probe_outcome = None;
        self.add_probe_busy = false;
        self.add_selected_files.clear();
        self.add_selected_variant = 0;
        self.add_priority = DownloadPriority::Normal;
        self.add_start_paused = false;
        self.add_play_asap = false;
        self.add_show_pass = false;
        self.add_apply_schedule = cfg.core.enable_scheduler_for_new_downloads;
        self.add_url_input
            .update(cx, |inp, cx| inp.set_text(prefill_url, cx));
        self.add_dir_input
            .update(cx, |inp, cx| inp.set_text(&def_dir, cx));
        self.add_name_input
            .update(cx, |inp, cx| inp.set_text("", cx));
        self.add_ref_input
            .update(cx, |inp, cx| inp.set_text("", cx));
        self.add_user_input
            .update(cx, |inp, cx| inp.set_text("", cx));
        self.add_pass_input.update(cx, |inp, cx| {
            inp.set_masked(true, cx);
            inp.set_text("", cx);
        });
        self.refresh_disk_space_cache(&def_dir);
        self.set_active_modal(ActiveModal::AddDownload);
        let fh = self.add_url_input.read(cx).focus_handle.clone();
        window.focus(&fh);
        cx.notify();
    }

    pub(crate) fn open_batch_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let def_dir = self
            .engine
            .get_config()
            .core
            .default_download_path
            .to_string_lossy()
            .into_owned();
        self.batch_dir_input
            .update(cx, |inp, cx| inp.set_text(&def_dir, cx));
        self.batch_group_title_input
            .update(cx, |inp, cx| inp.set_text("", cx));
        self.batch_start_paused = false;
        self.set_active_modal(ActiveModal::BatchAdd);
        let fh = self.batch_pattern_input.read(cx).focus_handle.clone();
        window.focus(&fh);
        cx.notify();
    }

    pub(crate) fn trigger_probe(&mut self, cx: &mut Context<Self>) {
        let url = self.add_url_input.read(cx).text().trim().to_string();
        if url.is_empty() {
            self.set_toast("Enter a URL, magnet URI, or .torrent file path first", true);
            return;
        }
        self.add_probe_busy = true;
        self.add_probe_outcome = None;
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            match eng.probe_source(&url).await {
                Ok(outcome) => q.push(UiAsyncMsg::ProbeCompleted {
                    outcome: Box::new(outcome),
                }),
                Err(e) => q.push(UiAsyncMsg::ProbeFailed {
                    error: e.to_string(),
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn submit_add_download(&mut self, cx: &mut Context<Self>) {
        let mut source_url = self.add_url_input.read(cx).text().trim().to_string();
        if source_url.is_empty() {
            self.set_toast("Download source URL cannot be empty", true);
            return;
        }

        let dir_str = self.add_dir_input.read(cx).text().trim().to_string();
        let name_str = self.add_name_input.read(cx).text().trim().to_string();
        let referer = self.add_ref_input.read(cx).text().trim().to_string();
        let user = self.add_user_input.read(cx).text().trim().to_string();
        let pass = self.add_pass_input.read(cx).text().to_string();

        if (!user.is_empty() || !pass.is_empty())
            && let Ok(parsed) = url::Url::parse(&source_url)
            && let Some(host) = parsed.host_str()
        {
            let _ = self.engine.save_credential(host, &user, &pass);
        }

        let mut req = NewDownloadRequest::from_url(source_url.clone());
        let mut chosen_type: Option<DownloadType> = None;

        if let Some(ref outcome) = self.add_probe_outcome {
            if outcome.download_type == DownloadType::MediaParser {
                if let Some(ref media) = outcome.media
                    && let Some(var) = media.variants.get(self.add_selected_variant)
                {
                    if !var.http_headers.is_empty() {
                        req.http.custom_headers.extend(var.http_headers.clone());
                    }
                    if var.audio_url.is_none() {
                        source_url = var.url.clone();
                        let saved_headers = req.http.custom_headers.clone();
                        req = NewDownloadRequest::from_url(source_url.clone());
                        req.http.custom_headers = saved_headers;
                        chosen_type = if source_url.to_ascii_lowercase().contains(".m3u8") {
                            Some(DownloadType::HlsM3u8)
                        } else {
                            Some(DownloadType::Www)
                        };
                    } else {
                        chosen_type = Some(DownloadType::MediaParser);
                    }
                }
            } else {
                chosen_type = Some(outcome.download_type);
            }

            if outcome.download_type == DownloadType::BitTorrent && !outcome.files.is_empty() {
                let mut indices: Vec<usize> = self.add_selected_files.iter().copied().collect();
                indices.sort_unstable();
                req.selected_file_indices = Some(indices);
            }
        }

        if let Some(dt) = chosen_type {
            req.download_type = Some(dt);
        }
        if !dir_str.is_empty() {
            req.target_dir = Some(PathBuf::from(dir_str));
        }
        if !name_str.is_empty() {
            req.file_name = Some(name_str.clone());
            req.title = Some(name_str);
        }
        if !referer.is_empty() {
            req.http.referrer = Some(referer);
        }
        req.priority = self.add_priority;
        req.start_paused = self.add_start_paused;
        req.sequential_play_asap = self.add_play_asap;
        if self.add_apply_schedule {
            let mut s = self.engine.get_config().core.default_schedule;
            s.enabled = true;
            req.schedule = Some(s);
        }

        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            match eng.add_download(req).await {
                Ok(rec) => q.push(UiAsyncMsg::EnqueueFinished {
                    count: 1,
                    label: rec.title,
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Failed to add download: {e}"),
                    is_error: true,
                }),
            }
        });
    }

    pub(crate) fn submit_batch(&mut self, cx: &mut Context<Self>) {
        let pattern = self.batch_pattern_input.read(cx).text();
        if pattern.trim().is_empty() {
            self.set_toast("Batch pattern cannot be empty", true);
            return;
        }
        let dir_s = self.batch_dir_input.read(cx).text();
        let target_dir = if dir_s.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(dir_s.trim()))
        };
        let paused = self.batch_start_paused;
        let as_group = self.batch_as_group;
        let title_raw = self.batch_group_title_input.read(cx).text();
        let group_title = {
            let t = title_raw.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        };
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            if as_group {
                match eng
                    .add_batch_group(&pattern, group_title.as_deref(), target_dir, paused)
                    .await
                {
                    Ok((parent, children)) => q.push(UiAsyncMsg::EnqueueFinished {
                        count: children.len(),
                        label: parent.title,
                    }),
                    Err(e) => q.push(UiAsyncMsg::Toast {
                        text: format!("Batch failed: {e}"),
                        is_error: true,
                    }),
                }
            } else {
                match eng.add_batch_pattern(&pattern, target_dir, paused).await {
                    Ok(list) => q.push(UiAsyncMsg::EnqueueFinished {
                        count: list.len(),
                        label: "Batch queue".into(),
                    }),
                    Err(e) => q.push(UiAsyncMsg::Toast {
                        text: format!("Batch failed: {e}"),
                        is_error: true,
                    }),
                }
            }
        });
    }

    pub(crate) fn toggle_download(&mut self, id: DownloadId) {
        let Some(rec) = self.engine.get_download(id) else {
            return;
        };
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        if rec.state.is_running_or_seeding() || rec.state == DownloadState::Queued {
            self.rt.spawn(async move {
                if let Err(e) = eng.pause_download(id).await {
                    q.push(UiAsyncMsg::Toast {
                        text: format!("Pause failed: {e}"),
                        is_error: true,
                    });
                }
            });
        } else {
            self.rt.spawn(async move {
                if let Err(e) = eng.resume_download(id).await {
                    q.push(UiAsyncMsg::Toast {
                        text: format!("Resume failed: {e}"),
                        is_error: true,
                    });
                }
            });
        }
    }

    pub(crate) fn toggle_selected_download(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_id {
            self.toggle_download(id);
            cx.notify();
        }
    }

    pub(crate) fn cancel_download(&mut self, id: DownloadId) {
        let eng = Arc::clone(&self.engine);
        self.rt.spawn(async move {
            let _ = eng.cancel_download(id).await;
        });
    }

    pub(crate) fn remove_download(&mut self, id: DownloadId, delete_files: bool) {
        self.checked_ids.remove(&id);
        let eng = Arc::clone(&self.engine);
        self.rt.spawn(async move {
            let _ = eng.remove_download(id, delete_files).await;
        });
    }

    pub(crate) fn request_delete_downloads(
        &mut self,
        ids: Vec<DownloadId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            return;
        }
        let action = self.engine.get_config().app.delete_button_action;
        match action {
            DeleteButtonAction::RemoveOnly => {
                self.confirm_delete_downloads(ids, false, false, window, cx);
            }
            DeleteButtonAction::DeleteFiles => {
                self.confirm_delete_downloads(ids, true, false, window, cx);
            }
            DeleteButtonAction::AlwaysAsk => {
                self.delete_remember_choice = false;
                self.active_popover = ActivePopover::None;
                self.set_active_modal(ActiveModal::DeleteConfirm { ids });
                cx.notify();
            }
        }
    }

    pub(crate) fn confirm_delete_downloads(
        &mut self,
        ids: Vec<DownloadId>,
        delete_files: bool,
        remember: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if remember {
            let new_policy = if delete_files {
                DeleteButtonAction::DeleteFiles
            } else {
                DeleteButtonAction::RemoveOnly
            };
            let mut live = self.engine.get_config();
            live.app.delete_button_action = new_policy;
            self.settings_cfg.app.delete_button_action = new_policy;
            let _ = self.engine.update_config(live);
        }
        for id in &ids {
            self.checked_ids.remove(id);
        }
        let count = ids.len();
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            for id in ids {
                let _ = eng.remove_download(id, delete_files).await;
            }
            q.push(UiAsyncMsg::Toast {
                text: if delete_files {
                    format!("Deleted {count} download(s) and files")
                } else {
                    format!("Removed {count} download(s) from list")
                },
                is_error: false,
            });
        });
        if matches!(self.active_modal, ActiveModal::DeleteConfirm { .. }) {
            self.active_modal = ActiveModal::None;
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    pub(crate) fn request_quit(&mut self, cx: &mut Context<Self>) {
        let cfg = self.engine.get_config();
        let has_active = self
            .all_downloads
            .iter()
            .any(|d| d.state.is_running_or_seeding() || d.state == DownloadState::Queued);
        if cfg.app.quit_confirmation && has_active {
            self.active_popover = ActivePopover::None;
            self.set_active_modal(ActiveModal::QuitConfirm);
            cx.notify();
        } else {
            cx.quit();
        }
    }

    pub(crate) fn copy_to_clipboard(
        &mut self,
        text: String,
        toast_msg: &str,
        cx: &mut Context<Self>,
    ) {
        write_clipboard_text(text, true, cx);
        self.set_toast(toast_msg, false);
        cx.notify();
    }

    pub(crate) fn copy_to_clipboard_from_keyboard(
        &mut self,
        text: String,
        toast_msg: &str,
        cx: &mut Context<Self>,
    ) {
        write_clipboard_text(text, false, cx);
        self.set_toast(toast_msg, false);
        cx.notify();
    }

    pub(crate) fn open_download_file(&mut self, id: DownloadId) {
        if let Some(rec) = self.engine.get_download(id)
            && let Err(e) = open_path_os(&rec.output_path())
        {
            self.set_toast(format!("Open failed: {e}"), true);
        }
    }

    pub(crate) fn reveal_download_file(&mut self, id: DownloadId) {
        if let Some(rec) = self.engine.get_download(id)
            && let Err(e) = reveal_path_os(&rec.output_path())
        {
            self.set_toast(format!("Reveal failed: {e}"), true);
        }
    }

    pub(crate) fn set_tum(&mut self, mode: TrafficUsageMode, cx: &mut Context<Self>) {
        if mode != TrafficUsageMode::Snail {
            self.last_non_snail_tum = mode;
        }
        if let Err(e) = self.engine.set_traffic_usage_mode(mode) {
            self.set_toast(format!("Failed to set speed mode: {e}"), true);
        } else {
            self.refresh_snapshot(cx);
            self.set_toast(
                format!("Traffic profile switched to {}", mode.label()),
                false,
            );
        }
        cx.notify();
    }

    pub(crate) fn toggle_snail_mode(&mut self, cx: &mut Context<Self>) {
        if self.summary.tum == TrafficUsageMode::Snail {
            let restore = self.last_non_snail_tum;
            self.set_tum(restore, cx);
        } else {
            if self.summary.tum != TrafficUsageMode::Snail {
                self.last_non_snail_tum = self.summary.tum;
            }
            self.set_tum(TrafficUsageMode::Snail, cx);
        }
    }

    pub(crate) fn set_shutdown_when_done(&mut self, st: ShutdownType, cx: &mut Context<Self>) {
        let mut live = self.engine.get_config();
        live.core.shutdown_when_done = st;
        self.settings_cfg.core.shutdown_when_done = st;
        if let Err(e) = self.engine.update_config(live) {
            self.set_toast(format!("Failed setting auto-shutdown: {e}"), true);
        } else {
            let label = match st {
                ShutdownType::None => "Auto-shutdown disabled",
                ShutdownType::Sleep => "Auto-action: Sleep when done",
                ShutdownType::Hibernate => "Auto-action: Hibernate when done",
                ShutdownType::Shutdown => "Auto-action: Shut down computer when done",
                ShutdownType::QuitApp => "Auto-action: Quit Ferrite when done",
            };
            self.set_toast(label, false);
        }
        cx.notify();
    }

    pub(crate) fn bump_priority_action(
        &mut self,
        id: DownloadId,
        up: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(rec) = self.engine.get_download(id) else {
            return;
        };
        let next = match (rec.priority, up) {
            (DownloadPriority::Low, true) => DownloadPriority::Normal,
            (DownloadPriority::Normal, true) => DownloadPriority::High,
            (DownloadPriority::High, false) => DownloadPriority::Normal,
            (DownloadPriority::Normal, false) => DownloadPriority::Low,
            (same, _) => same,
        };
        if next != rec.priority {
            self.set_priority_action(id, next, cx);
        }
    }

    pub(crate) fn set_priority_action(
        &mut self,
        id: DownloadId,
        prio: DownloadPriority,
        cx: &mut Context<Self>,
    ) {
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            match eng.set_priority(id, prio).await {
                Ok(()) => q.push(UiAsyncMsg::Toast {
                    text: format!("Set #{id} priority to {:?}", prio),
                    is_error: false,
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Priority error: {e}"),
                    is_error: true,
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn restart_download_action(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            match eng.restart_download(id).await {
                Ok(()) => q.push(UiAsyncMsg::Toast {
                    text: format!("Restarted download #{id}"),
                    is_error: false,
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Restart failed: {e}"),
                    is_error: true,
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn save_stream_complete_action(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            match eng.save_and_complete_download(id).await {
                Ok(()) => q.push(UiAsyncMsg::Toast {
                    text: format!("Saved & finalized stream #{id}"),
                    is_error: false,
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Save stream failed: {e}"),
                    is_error: true,
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn check_virus_action(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.set_toast(
            format!("Scanning download #{id} with configured antivirus..."),
            false,
        );
        self.rt.spawn(async move {
            match eng.check_download_virus(id).await {
                Ok(summary) => q.push(UiAsyncMsg::VirusCheckFinished {
                    download_id: id,
                    clean: true,
                    summary,
                }),
                Err(e) => q.push(UiAsyncMsg::VirusCheckFinished {
                    download_id: id,
                    clean: false,
                    summary: format!("Antivirus check: {e}"),
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn check_update_action(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.set_toast(format!("Checking server for updates on #{id}..."), false);
        self.rt.spawn(async move {
            match eng.check_download_for_update(id).await {
                Ok(changed) => q.push(UiAsyncMsg::UpdateCheckFinished {
                    download_id: id,
                    changed,
                    message: if changed {
                        format!("Remote file changed for #{id} — ready to restart")
                    } else {
                        format!("Remote file unchanged for #{id}")
                    },
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Update check failed: {e}"),
                    is_error: true,
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn toggle_auto_retry_action(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let Some(rec) = self.engine.get_download(id) else {
            return;
        };
        let next_allow = rec.flags.disable_auto_retry;
        match self.engine.toggle_disable_auto_retry(id, next_allow) {
            Ok(()) => {
                self.refresh_snapshot(cx);
                self.set_toast(
                    if next_allow {
                        "Auto-retry enabled for this download"
                    } else {
                        "Auto-retry disabled for this download"
                    },
                    false,
                );
            }
            Err(e) => self.set_toast(format!("Toggle auto-retry failed: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn bt_reannounce_action(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            match eng.force_reannounce_bt(id).await {
                Ok(()) => q.push(UiAsyncMsg::Toast {
                    text: format!("Reannounced trackers for #{id}"),
                    is_error: false,
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Reannounce failed: {e}"),
                    is_error: true,
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn bt_toggle_seeding_action(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let Some(rec) = self.engine.get_download(id) else {
            return;
        };
        let next = !rec.flags.bt_disable_seeding;
        match self.engine.toggle_bt_disable_seeding(id, next) {
            Ok(()) => {
                self.refresh_snapshot(cx);
                self.set_toast(
                    if next {
                        "Seeding disabled for this torrent"
                    } else {
                        "Seeding enabled for this torrent"
                    },
                    false,
                );
            }
            Err(e) => self.set_toast(format!("Failed toggling seeding: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn bt_toggle_ignore_ratio_action(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let Some(rec) = self.engine.get_download(id) else {
            return;
        };
        let next = !rec.flags.bt_ignore_upload_ratio;
        match self.engine.toggle_bt_ignore_ratio(id, next) {
            Ok(()) => {
                self.refresh_snapshot(cx);
                self.set_toast(
                    if next {
                        "Ignoring upload ratio limit for this torrent"
                    } else {
                        "Respecting global upload ratio limit"
                    },
                    false,
                );
            }
            Err(e) => self.set_toast(format!("Failed toggling ignore-ratio: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn add_bt_tracker_action(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let raw = self.bt_add_tracker_input.read(cx).text();
        let trackers: Vec<String> = raw
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if trackers.is_empty() {
            self.set_toast("Enter a tracker announce URL first", true);
            return;
        }
        self.bt_add_tracker_input
            .update(cx, |i, cx| i.set_text("", cx));
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            match eng.add_bt_trackers(id, trackers).await {
                Ok(()) => q.push(UiAsyncMsg::Toast {
                    text: "Added tracker URL(s)".into(),
                    is_error: false,
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Add tracker failed: {e}"),
                    is_error: true,
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn open_hash_modal(
        &mut self,
        id: DownloadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hash_computed = None;
        self.hash_matched = None;
        self.hash_expected_input
            .update(cx, |i, cx| i.set_text("", cx));
        self.active_modal = ActiveModal::HashVerify { id };
        let fh = self.hash_expected_input.read(cx).focus_handle.clone();
        window.focus(&fh);
        cx.notify();
    }

    pub(crate) fn run_hash_check(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let algo = self.hash_algo;
        let expected = self.hash_expected_input.read(cx).text();
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn_blocking(move || {
            let exp_opt = if expected.trim().is_empty() {
                None
            } else {
                Some(expected.as_str())
            };
            match eng.verify_download_hash(id, algo, exp_opt) {
                Ok((digest, matched)) => q.push(UiAsyncMsg::HashResult {
                    download_id: id,
                    algo,
                    digest,
                    matched,
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Hash error: {e}"),
                    is_error: true,
                }),
            }
        });
    }

    pub(crate) fn open_convert_modal(
        &mut self,
        id: DownloadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_mode_mp3 = true;
        self.convert_mp3_bitrate = 320;
        self.convert_use_vbr = false;
        self.convert_vbr_quality = 2;
        self.convert_stream_copy = true;
        self.convert_busy = false;
        self.convert_status = None;
        if let Some(rec) = self.engine.get_download(id) {
            let out = rec.output_path().with_extension("mp3");
            self.convert_out_input
                .update(cx, |i, cx| i.set_text(&out.to_string_lossy(), cx));
        }
        self.active_modal = ActiveModal::MediaConvert { id };
        let fh = self.convert_out_input.read(cx).focus_handle.clone();
        window.focus(&fh);
        cx.notify();
    }

    pub(crate) fn set_convert_target_ext(
        &mut self,
        mp3: bool,
        id: DownloadId,
        cx: &mut Context<Self>,
    ) {
        self.convert_mode_mp3 = mp3;
        if let Some(rec) = self.engine.get_download(id) {
            let ext = if mp3 { "mp3" } else { "mp4" };
            let mut out = rec.output_path().with_extension(ext);
            if out == rec.output_path() {
                let stem = rec
                    .output_path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("converted")
                    .to_string();
                out = rec.target_dir.join(format!("{stem}.converted.{ext}"));
            }
            self.convert_out_input
                .update(cx, |i, cx| i.set_text(&out.to_string_lossy(), cx));
        }
        cx.notify();
    }

    pub(crate) fn run_media_convert(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        let Some(rec) = self.engine.get_download(id) else {
            return;
        };
        let input = rec.output_path();
        let output = PathBuf::from(self.convert_out_input.read(cx).text().trim().to_string());
        let cfg = self.engine.get_config();
        let mp3 = self.convert_mode_mp3;
        let bitrate = self.convert_mp3_bitrate;
        let use_vbr = self.convert_use_vbr;
        let vbr_q = self.convert_vbr_quality;
        let copy = self.convert_stream_copy;
        let q = self.msgs.clone();
        self.convert_busy = true;
        self.convert_status = Some("Converting with FFmpeg...".into());
        self.rt.spawn(async move {
            let res = if mp3 {
                let opt = if use_vbr {
                    Mp3QualityOption::Vbr(vbr_q)
                } else {
                    Mp3QualityOption::Cbr(bitrate)
                };
                convert_to_mp3_with_options(&cfg, &input, &output, opt, |_| {}).await
            } else {
                convert_to_mp4(&cfg, &input, &output, copy, |_| {}).await
            };
            match res {
                Ok(()) => q.push(UiAsyncMsg::ConvertFinished { out_path: output }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Conversion failed: {e}"),
                    is_error: true,
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn open_remote_qr_modal(&mut self, cx: &mut Context<Self>) {
        self.active_modal = ActiveModal::RemoteQr;
        self.remote_loading = true;
        self.remote_error = None;
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        let host = detect_lan_ip();
        self.rt.spawn(async move {
            match eng.ensure_remote_control(&host).await {
                Ok(pairing) => q.push(UiAsyncMsg::RemoteControlReady { pairing }),
                Err(e) => q.push(UiAsyncMsg::RemoteControlFailed {
                    error: e.to_string(),
                }),
            }
        });
        cx.notify();
    }

    pub(crate) fn regenerate_remote_password_action(&mut self, cx: &mut Context<Self>) {
        match self.engine.regenerate_remote_password() {
            Ok(pw) => {
                self.msgs
                    .push(UiAsyncMsg::RemotePasswordRegenerated { password: pw });
            }
            Err(e) => self.set_toast(format!("Failed regenerating password: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn open_edit_tag_modal(
        &mut self,
        id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tid) = id {
            if let Some(tag) = self.tags.iter().find(|t| t.id == tid).cloned() {
                self.new_tag_name_input
                    .update(cx, |i, cx| i.set_text(&tag.name, cx));
                self.new_tag_color_hex = tag.color_hex;
                self.tag_exts_input.update(cx, |i, cx| {
                    i.set_text(&tag.default_extensions.join(", "), cx);
                });
                self.tag_dir_input.update(cx, |i, cx| {
                    let s = tag
                        .default_download_folder
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    i.set_text(&s, cx);
                });
            }
        } else {
            self.new_tag_name_input
                .update(cx, |i, cx| i.set_text("", cx));
            self.new_tag_color_hex = "#3b82f6".into();
            self.tag_exts_input.update(cx, |i, cx| i.set_text("", cx));
            self.tag_dir_input.update(cx, |i, cx| i.set_text("", cx));
        }
        self.active_modal = ActiveModal::EditTag { id };
        let fh = self.new_tag_name_input.read(cx).focus_handle.clone();
        window.focus(&fh);
        cx.notify();
    }

    pub(crate) fn submit_edit_tag(
        &mut self,
        id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.new_tag_name_input.read(cx).text().trim().to_string();
        if name.is_empty() {
            self.set_toast("Tag name cannot be empty", true);
            return;
        }
        let res = match id {
            Some(tid) => {
                let exts = parse_list_tokens(&self.tag_exts_input.read(cx).text());
                let dir_s = self.tag_dir_input.read(cx).text().trim().to_string();
                let dir_pb = (!dir_s.is_empty()).then(|| PathBuf::from(dir_s));
                self.engine.edit_tag(
                    tid,
                    &name,
                    &self.new_tag_color_hex,
                    &exts,
                    dir_pb.as_deref(),
                )
            }
            None => self.engine.create_tag(&name, &self.new_tag_color_hex),
        };
        match res {
            Ok(_) => {
                self.refresh_snapshot(cx);
                self.set_toast(
                    if id.is_some() {
                        format!("Updated tag '{name}'")
                    } else {
                        format!("Created tag '{name}'")
                    },
                    false,
                );
                self.close_modal(window, cx);
            }
            Err(e) => self.set_toast(format!("Failed saving tag: {e}"), true),
        }
    }

    pub(crate) fn open_change_source_modal(
        &mut self,
        id: DownloadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(rec) = self.engine.get_download(id) {
            self.source_url_input
                .update(cx, |i, cx| i.set_text(&rec.url, cx));
            self.source_mirrors_input
                .update(cx, |i, cx| i.set_text(&rec.http.mirrors.join(", "), cx));
            self.active_modal = ActiveModal::ChangeSource { id };
            let fh = self.source_url_input.read(cx).focus_handle.clone();
            window.focus(&fh);
            cx.notify();
        }
    }

    pub(crate) fn submit_change_source(
        &mut self,
        id: DownloadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_url = self.source_url_input.read(cx).text().trim().to_string();
        let mirrors: Vec<String> = self
            .source_mirrors_input
            .read(cx)
            .text()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            if let Err(e) = eng.change_source_url(id, &new_url).await {
                q.push(UiAsyncMsg::Toast {
                    text: format!("Invalid URL: {e}"),
                    is_error: true,
                });
                return;
            }
            let _ = eng.update_mirrors(id, mirrors).await;
            q.push(UiAsyncMsg::Toast {
                text: "Updated source URL & mirrors".into(),
                is_error: false,
            });
        });
        self.close_modal(window, cx);
    }

    pub(crate) fn open_move_dir_modal(
        &mut self,
        ids: Vec<DownloadId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(first_id) = ids.first().copied() else {
            return;
        };
        if let Some(rec) = self.engine.get_download(first_id) {
            self.move_dir_input.update(cx, |i, cx| {
                i.set_text(&rec.target_dir.to_string_lossy(), cx)
            });
            self.set_active_modal(ActiveModal::MoveDir { ids });
            let fh = self.move_dir_input.read(cx).focus_handle.clone();
            window.focus(&fh);
            cx.notify();
        }
    }

    pub(crate) fn submit_move_dir(
        &mut self,
        ids: Vec<DownloadId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dest = PathBuf::from(self.move_dir_input.read(cx).text().trim());
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            let mut moved = 0usize;
            let mut last_err: Option<String> = None;
            let mut last_path: Option<PathBuf> = None;
            for id in &ids {
                match eng.move_download_files(*id, &dest).await {
                    Ok(p) => {
                        moved += 1;
                        last_path = Some(p);
                    }
                    Err(e) => {
                        last_err = Some(e.to_string());
                    }
                }
            }
            if let Some(err) = last_err {
                q.push(UiAsyncMsg::Toast {
                    text: if moved > 0 {
                        format!("Moved {moved}/{} item(s); error: {err}", ids.len())
                    } else {
                        format!("Move failed: {err}")
                    },
                    is_error: true,
                });
            } else if moved > 1 {
                q.push(UiAsyncMsg::Toast {
                    text: format!("Moved {moved} downloads to {}", dest.display()),
                    is_error: false,
                });
            } else if let Some(p) = last_path {
                q.push(UiAsyncMsg::Toast {
                    text: format!("Moved to {}", p.display()),
                    is_error: false,
                });
            }
        });
        self.close_modal(window, cx);
    }

    pub(crate) fn open_rename_modal(
        &mut self,
        id: DownloadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(rec) = self.engine.get_download(id) {
            self.rename_file_input
                .update(cx, |i, cx| i.set_text(&rec.file_name, cx));
            self.active_modal = ActiveModal::RenameDownload { id };
            let fh = self.rename_file_input.read(cx).focus_handle.clone();
            window.focus(&fh);
            cx.notify();
        }
    }

    pub(crate) fn submit_rename_download(
        &mut self,
        id: DownloadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_name = self.rename_file_input.read(cx).text().trim().to_string();
        if new_name.is_empty() {
            self.set_toast("File name cannot be empty", true);
            return;
        }
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            match eng.rename_download_file(id, &new_name).await {
                Ok(p) => q.push(UiAsyncMsg::Toast {
                    text: format!("Renamed to {}", p.display()),
                    is_error: false,
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Rename failed: {e}"),
                    is_error: true,
                }),
            }
        });
        self.close_modal(window, cx);
    }

    pub(crate) fn open_schedule_modal(
        &mut self,
        id: DownloadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(rec) = self.engine.get_download(id) {
            let r = rec
                .schedule
                .unwrap_or_else(|| self.engine.get_config().core.default_schedule);
            self.sched_enabled = r.enabled;
            self.sched_days_mask = r.days_mask;
            self.sched_start_input.update(cx, |i, cx| {
                i.set_text(&format_minute_hhmm(r.start_minute), cx)
            });
            self.sched_end_input.update(cx, |i, cx| {
                i.set_text(&format_minute_hhmm(r.end_minute), cx)
            });
            self.active_modal = ActiveModal::DownloadSchedule { id };
            let fh = self.sched_start_input.read(cx).focus_handle.clone();
            window.focus(&fh);
            cx.notify();
        }
    }

    pub(crate) fn submit_download_schedule(
        &mut self,
        id: DownloadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rule = if self.sched_enabled {
            Some(ScheduleRule {
                enabled: true,
                days_mask: self.sched_days_mask,
                start_minute: parse_minute_hhmm(&self.sched_start_input.read(cx).text(), 0),
                end_minute: parse_minute_hhmm(&self.sched_end_input.read(cx).text(), 360),
            })
        } else {
            None
        };
        if let Err(e) = self.engine.set_schedule(id, rule) {
            self.set_toast(format!("Schedule update failed: {e}"), true);
        } else {
            self.set_toast("Updated download schedule", false);
        }
        self.close_modal(window, cx);
    }

    pub(crate) fn open_plugins_page(&mut self, cx: &mut Context<Self>) {
        self.settings_cfg = self.engine.get_config();
        self.settings_plugins = self.engine.list_js_plugins();
        self.plugins_gear_menu_open = false;
        self.active_popover = ActivePopover::None;
        self.set_active_page(ActivePage::Plugins, cx);
        cx.notify();
    }

    pub(crate) fn install_plugin_from_input(&mut self, cx: &mut Context<Self>) {
        let raw = self.plugin_install_input.read(cx).text().trim().to_string();
        if raw.is_empty() {
            self.set_toast("Enter a path to a JS plugin file or folder first", true);
            return;
        }
        match self
            .engine
            .install_js_plugin_from_path(std::path::Path::new(&raw))
        {
            Ok(info) => {
                self.plugin_install_input
                    .update(cx, |i, cx| i.set_text("", cx));
                self.msgs.push(UiAsyncMsg::PluginActionFinished {
                    message: format!("Installed add-on '{}' (v{})", info.name, info.version),
                    is_error: false,
                });
            }
            Err(e) => self.set_toast(format!("Install add-on failed: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn toggle_plugin_enabled(
        &mut self,
        uid: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        match self.engine.set_js_plugin_enabled(&uid, enabled) {
            Ok(()) => {
                self.settings_plugins = self.engine.list_js_plugins();
                self.set_toast(
                    if enabled {
                        format!("Enabled add-on {uid}")
                    } else {
                        format!("Disabled add-on {uid}")
                    },
                    false,
                );
            }
            Err(e) => self.set_toast(format!("Plugin toggle failed: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn uninstall_plugin_action(&mut self, uid: String, cx: &mut Context<Self>) {
        match self.engine.uninstall_js_plugin(&uid) {
            Ok(()) => {
                self.settings_plugins = self.engine.list_js_plugins();
                self.set_toast(format!("Removed add-on {uid}"), false);
            }
            Err(e) => self.set_toast(format!("Uninstall failed: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn open_settings_page(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        let _ = self.engine.ensure_remote_password();
        let cfg = self.engine.get_config();
        let tum = cfg.core.current_tum;
        self.settings_cfg = cfg;
        self.settings_tum_tab = tum;
        self.settings_plugins = self.engine.list_js_plugins();
        self.settings_ssl_exceptions = self.engine.list_ssl_exceptions().unwrap_or_default();
        self.show_proxy_pass = false;
        self.show_remote_pass = false;
        self.refresh_settings_caches();
        self.sync_settings_inputs_from_draft(cx);
        self.load_tum_inputs_from_draft(cx);
        self.settings_dirty = false;
        self.active_popover = ActivePopover::None;
        self.set_settings_tab(tab, cx);
        cx.notify();
    }

    pub(crate) fn sync_settings_inputs_from_draft(&mut self, cx: &mut Context<Self>) {
        let c = &self.settings_cfg;
        let remote_pw = if c.app.remote_access_password.trim().is_empty() {
            self.engine.ensure_remote_password()
        } else {
            c.app.remote_access_password.clone()
        };
        let wb_small_val = if c.core.wb_small_size_unit_mb {
            c.app.wb_too_small_size_bytes / (1024 * 1024)
        } else {
            c.app.wb_too_small_size_bytes / 1024
        };
        let sound_s = c
            .app
            .notification_sounds
            .get(&NotificationEvent::DownloadComplete)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        self.set_default_dir.update(cx, |i, cx| {
            i.set_text(&c.core.default_download_path.to_string_lossy(), cx)
        });
        self.set_partial_suffix
            .update(cx, |i, cx| i.set_text(&c.core.partial_download_suffix, cx));
        self.set_keep_days.update(cx, |i, cx| {
            i.set_text(&c.core.auto_remove_finished_keep_days.to_string(), cx)
        });
        self.set_max_retries.update(cx, |i, cx| {
            i.set_text(&c.core.max_auto_retries.to_string(), cx)
        });
        self.set_slow_speed_kb.update(cx, |i, cx| {
            i.set_text(&format_kb_limit(c.core.unacceptably_slow_speed_bps), cx)
        });
        self.set_slow_interval_s.update(cx, |i, cx| {
            i.set_text(&c.core.unacceptably_slow_interval_secs.to_string(), cx)
        });
        self.set_max_query_dl.update(cx, |i, cx| {
            i.set_text(&c.core.max_additional_query_info_downloads.to_string(), cx)
        });
        self.set_max_small_dl.update(cx, |i, cx| {
            i.set_text(&c.core.max_additional_small_downloads.to_string(), cx)
        });
        self.set_max_slow_dl.update(cx, |i, cx| {
            i.set_text(&c.core.max_additional_downloads_if_slow.to_string(), cx)
        });
        self.set_proxy_host
            .update(cx, |i, cx| i.set_text(&c.app.proxy.http.host, cx));
        self.set_proxy_port.update(cx, |i, cx| {
            i.set_text(&c.app.proxy.http.port.to_string(), cx)
        });
        self.set_socks5_host
            .update(cx, |i, cx| i.set_text(&c.app.proxy.socks5.host, cx));
        self.set_socks5_port.update(cx, |i, cx| {
            i.set_text(&c.app.proxy.socks5.port.to_string(), cx)
        });
        self.set_proxy_user
            .update(cx, |i, cx| i.set_text(&c.app.proxy.http.username, cx));
        self.set_proxy_pass.update(cx, |i, cx| {
            i.set_masked(true, cx);
            i.set_text(&c.app.proxy.http.password, cx);
        });
        self.set_bt_port.update(cx, |i, cx| {
            i.set_text(&c.core.bt_session_port.to_string(), cx)
        });
        self.set_bt_watch_dir.update(cx, |i, cx| {
            i.set_text(
                &c.app
                    .torrents_watch_folder
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                cx,
            )
        });
        self.set_bt_trackers.update(cx, |i, cx| {
            i.set_text(&c.core.bt_tracker_list.join(", "), cx)
        });
        self.set_sched_start.update(cx, |i, cx| {
            i.set_text(
                &format_minute_hhmm(c.core.default_schedule.start_minute),
                cx,
            )
        });
        self.set_sched_end.update(cx, |i, cx| {
            i.set_text(&format_minute_hhmm(c.core.default_schedule.end_minute), cx)
        });
        self.set_battery_min.update(cx, |i, cx| {
            i.set_text(&c.core.battery_min_level_percent.to_string(), cx)
        });
        self.set_wb_small_kb
            .update(cx, |i, cx| i.set_text(&wb_small_val.to_string(), cx));
        self.set_wb_skip_ext.update(cx, |i, cx| {
            i.set_text(&c.app.wb_unwanted_extensions.join(" "), cx)
        });
        self.set_wb_only_ext.update(cx, |i, cx| {
            i.set_text(&c.app.wb_intercept_extensions.join(" "), cx)
        });
        self.set_wb_skip_hosts.update(cx, |i, cx| {
            i.set_text(&c.app.wb_unwanted_hosts.join(", "), cx)
        });
        self.set_ffmpeg_path.update(cx, |i, cx| {
            i.set_text(
                &c.core
                    .ffmpeg_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                cx,
            )
        });
        self.set_ytdlp_path.update(cx, |i, cx| {
            i.set_text(
                &c.core
                    .yt_dlp_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                cx,
            )
        });
        self.set_av_path.update(cx, |i, cx| {
            i.set_text(
                &c.core
                    .antivirus_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                cx,
            )
        });
        self.set_av_args
            .update(cx, |i, cx| i.set_text(&c.core.antivirus_args, cx));
        self.set_ext_path.update(cx, |i, cx| {
            i.set_text(
                &c.core
                    .external_app_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                cx,
            )
        });
        self.set_ext_args.update(cx, |i, cx| {
            i.set_text(&c.core.external_app_args_template, cx)
        });
        self.set_remote_bind
            .update(cx, |i, cx| i.set_text(&c.app.remote_access_bind, cx));
        self.set_remote_port.update(cx, |i, cx| {
            i.set_text(&c.app.remote_access_port.to_string(), cx)
        });
        self.set_remote_pass.update(cx, |i, cx| {
            i.set_masked(true, cx);
            i.set_text(&remote_pw, cx);
        });
        self.set_notify_sound_path
            .update(cx, |i, cx| i.set_text(&sound_s, cx));
        self.set_low_disk_mb.update(cx, |i, cx| {
            i.set_text(
                &(c.core.low_disk_space_min_bytes / (1024 * 1024)).to_string(),
                cx,
            )
        });
        self.set_low_disk_pct.update(cx, |i, cx| {
            i.set_text(&format!("{:.1}", c.core.low_disk_space_min_percent), cx)
        });
        self.set_backup_interval_m.update(cx, |i, cx| {
            i.set_text(&(c.app.db_backup_interval_secs.max(0) / 60).to_string(), cx)
        });
        self.set_batch_max_urls.update(cx, |i, cx| {
            i.set_text(&c.app.batch_download_max_urls_count.to_string(), cx)
        });
    }

    pub(crate) fn flush_tum_inputs_into_draft(&mut self, cx: &App) {
        let dl = parse_kb_limit(&self.set_tum_dl_kb.read(cx).text());
        let ul = parse_kb_limit(&self.set_tum_ul_kb.read(cx).text());
        let conns = self
            .set_tum_conns
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(16)
            .max(1);
        let per_srv = self
            .set_tum_per_srv
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(4)
            .max(1);
        let conc = self
            .set_tum_concurrent
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(4)
            .max(1);
        let ratio = self
            .set_tum_ratio
            .read(cx)
            .text()
            .trim()
            .parse::<f32>()
            .unwrap_or(0.0)
            .max(0.0);

        if let Some(p) = self
            .settings_cfg
            .core
            .tum_profiles
            .get_mut(&self.settings_tum_tab)
        {
            p.max_download_speed_bps = dl;
            p.max_upload_speed_bps = ul;
            p.max_connections = conns;
            p.max_connections_per_server = per_srv;
            p.max_concurrent_downloads = conc;
            p.max_upload_ratio = ratio;
        }
    }

    pub(crate) fn load_tum_inputs_from_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self
            .settings_cfg
            .core
            .tum_profiles
            .get(&self.settings_tum_tab)
            .cloned()
        {
            self.set_tum_dl_kb.update(cx, |i, cx| {
                i.set_text(&format_kb_limit(p.max_download_speed_bps), cx)
            });
            self.set_tum_ul_kb.update(cx, |i, cx| {
                i.set_text(&format_kb_limit(p.max_upload_speed_bps), cx)
            });
            self.set_tum_conns
                .update(cx, |i, cx| i.set_text(&p.max_connections.to_string(), cx));
            self.set_tum_per_srv.update(cx, |i, cx| {
                i.set_text(&p.max_connections_per_server.to_string(), cx)
            });
            self.set_tum_concurrent.update(cx, |i, cx| {
                i.set_text(&p.max_concurrent_downloads.to_string(), cx)
            });
            self.set_tum_ratio.update(cx, |i, cx| {
                i.set_text(&format!("{:.1}", p.max_upload_ratio), cx)
            });
        }
    }

    pub(crate) fn switch_settings_tum(&mut self, next: TrafficUsageMode, cx: &mut Context<Self>) {
        self.flush_tum_inputs_into_draft(cx);
        self.settings_tum_tab = next;
        self.load_tum_inputs_from_draft(cx);
        cx.notify();
    }

    pub(crate) fn build_draft_config_with_inputs(&self, cx: &App) -> FerriteConfig {
        let mut next = self.settings_cfg.clone();

        let dl = parse_kb_limit(&self.set_tum_dl_kb.read(cx).text());
        let ul = parse_kb_limit(&self.set_tum_ul_kb.read(cx).text());
        let conns = self
            .set_tum_conns
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(16)
            .max(1);
        let per_srv = self
            .set_tum_per_srv
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(4)
            .max(1);
        let conc = self
            .set_tum_concurrent
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(4)
            .max(1);
        let ratio = self
            .set_tum_ratio
            .read(cx)
            .text()
            .trim()
            .parse::<f32>()
            .unwrap_or(0.0)
            .max(0.0);
        if let Some(p) = next.core.tum_profiles.get_mut(&self.settings_tum_tab) {
            p.max_download_speed_bps = dl;
            p.max_upload_speed_bps = ul;
            p.max_connections = conns;
            p.max_connections_per_server = per_srv;
            p.max_concurrent_downloads = conc;
            p.max_upload_ratio = ratio;
        }

        let def_dir = self.set_default_dir.read(cx).text().trim().to_string();
        if !def_dir.is_empty() {
            next.core.default_download_path = PathBuf::from(def_dir);
        }
        next.core.partial_download_suffix =
            self.set_partial_suffix.read(cx).text().trim().to_string();
        next.core.auto_remove_finished_keep_days = self
            .set_keep_days
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(30);
        next.core.max_auto_retries = self
            .set_max_retries
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(3);
        next.core.unacceptably_slow_speed_bps =
            parse_kb_limit(&self.set_slow_speed_kb.read(cx).text());
        next.core.unacceptably_slow_interval_secs = self
            .set_slow_interval_s
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(60);
        next.core.max_additional_query_info_downloads = self
            .set_max_query_dl
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(2);
        next.core.max_additional_small_downloads = self
            .set_max_small_dl
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(2);
        next.core.max_additional_downloads_if_slow = self
            .set_max_slow_dl
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(2);

        let p_host = self.set_proxy_host.read(cx).text().trim().to_string();
        let parsed_p_port = self
            .set_proxy_port
            .read(cx)
            .text()
            .trim()
            .parse::<u16>()
            .unwrap_or(0);
        let p_port = if parsed_p_port == 0 && !p_host.is_empty() {
            8080
        } else {
            parsed_p_port
        };
        let p_user = self.set_proxy_user.read(cx).text();
        let p_pass = self.set_proxy_pass.read(cx).text();
        let s5_host = self.set_socks5_host.read(cx).text().trim().to_string();
        let parsed_s5_port = self
            .set_socks5_port
            .read(cx)
            .text()
            .trim()
            .parse::<u16>()
            .unwrap_or(0);
        let s5_port = if parsed_s5_port == 0 && !s5_host.is_empty() {
            1080
        } else {
            parsed_s5_port
        };
        apply_proxy_inputs_to_settings(
            &mut next.app.proxy,
            p_host,
            p_port,
            s5_host,
            s5_port,
            p_user,
            p_pass,
        );

        next.core.bt_session_port = self
            .set_bt_port
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(6881);
        let watch_s = self.set_bt_watch_dir.read(cx).text().trim().to_string();
        next.app.torrents_watch_folder = if watch_s.is_empty() {
            None
        } else {
            Some(PathBuf::from(watch_s))
        };
        next.core.bt_tracker_list = self
            .set_bt_trackers
            .read(cx)
            .text()
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        next.core.default_schedule.start_minute =
            parse_minute_hhmm(&self.set_sched_start.read(cx).text(), 0);
        next.core.default_schedule.end_minute =
            parse_minute_hhmm(&self.set_sched_end.read(cx).text(), 360);
        next.core.battery_min_level_percent = self
            .set_battery_min
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(15);

        next.app.wb_too_small_size_bytes = parse_wb_threshold_bytes(
            &self.set_wb_small_kb.read(cx).text(),
            next.core.wb_small_size_unit_mb,
        );
        next.app.wb_unwanted_extensions = parse_list_tokens(&self.set_wb_skip_ext.read(cx).text());
        next.app.wb_intercept_extensions = parse_list_tokens(&self.set_wb_only_ext.read(cx).text());
        next.app.wb_unwanted_hosts = self
            .set_wb_skip_hosts
            .read(cx)
            .text()
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(|x| x.to_ascii_lowercase())
            .collect();

        let ff = self.set_ffmpeg_path.read(cx).text().trim().to_string();
        next.core.ffmpeg_path = if ff.is_empty() {
            None
        } else {
            Some(PathBuf::from(ff))
        };
        let ytdlp = self.set_ytdlp_path.read(cx).text().trim().to_string();
        next.core.yt_dlp_path = if ytdlp.is_empty() {
            None
        } else {
            Some(PathBuf::from(ytdlp))
        };

        let av = self.set_av_path.read(cx).text().trim().to_string();
        next.core.antivirus_path = if av.is_empty() {
            None
        } else {
            Some(PathBuf::from(av))
        };
        next.core.antivirus_args = self.set_av_args.read(cx).text();
        let ext = self.set_ext_path.read(cx).text().trim().to_string();
        next.core.external_app_path = if ext.is_empty() {
            None
        } else {
            Some(PathBuf::from(ext))
        };
        next.core.external_app_args_template = self.set_ext_args.read(cx).text();

        let rbind = self.set_remote_bind.read(cx).text().trim().to_string();
        if !rbind.is_empty() {
            next.app.remote_access_bind = rbind;
        }
        next.app.remote_access_port = self
            .set_remote_port
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(9443);
        let rpass = self.set_remote_pass.read(cx).text();
        if !rpass.trim().is_empty() {
            next.app.remote_access_password = rpass;
        }

        let sound_raw = self
            .set_notify_sound_path
            .read(cx)
            .text()
            .trim()
            .to_string();
        if sound_raw.is_empty() {
            next.app
                .notification_sounds
                .remove(&NotificationEvent::DownloadComplete);
        } else {
            next.app.notification_sounds.insert(
                NotificationEvent::DownloadComplete,
                PathBuf::from(sound_raw),
            );
        }

        let low_disk_mb = self
            .set_low_disk_mb
            .read(cx)
            .text()
            .trim()
            .parse::<u64>()
            .unwrap_or(256);
        next.core.low_disk_space_min_bytes = low_disk_mb.saturating_mul(1024 * 1024);
        next.core.low_disk_space_min_percent = self
            .set_low_disk_pct
            .read(cx)
            .text()
            .trim()
            .parse::<f32>()
            .unwrap_or(2.0)
            .clamp(0.0, 95.0);

        let backup_mins = self
            .set_backup_interval_m
            .read(cx)
            .text()
            .trim()
            .parse::<i64>()
            .unwrap_or(60);
        next.app.db_backup_interval_secs = backup_mins.max(1).saturating_mul(60);
        next.app.batch_download_max_urls_count = self
            .set_batch_max_urls
            .read(cx)
            .text()
            .trim()
            .parse()
            .unwrap_or(1000);

        let live_cfg = self.engine.get_config();
        next.path_by_url_hash = live_cfg.path_by_url_hash;
        next.path_by_file_type = live_cfg.path_by_file_type;
        next.recent_folders = live_cfg.recent_folders;

        next
    }

    pub(crate) fn has_unsaved_settings_changes(&self, _cx: &App) -> bool {
        self.settings_dirty
    }

    pub(crate) fn apply_settings(
        &mut self,
        close_page: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_cfg = self.build_draft_config_with_inputs(cx);

        match self.engine.update_config(self.settings_cfg.clone()) {
            Ok(()) => {
                let mut warn_toast: Option<String> = None;
                if let Err(e) = sync_autorun_on_login(
                    self.settings_cfg.app.autorun_on_login,
                    self.settings_cfg.app.launch_minimized,
                ) {
                    warn_toast = Some(format!("Settings saved, but LaunchAgent sync failed: {e}"));
                }
                if let Err(e) = sync_portable_marker(self.settings_cfg.app.portable_mode) {
                    warn_toast = Some(format!("Settings saved, but portable flag failed: {e}"));
                }
                set_macos_hide_dock_icon(self.settings_cfg.app.hide_dock_icon);
                self.preview_appearance(window, cx);
                self.refresh_dock_badge();

                if self.settings_cfg.app.enable_remote_access {
                    let eng_rc = Arc::clone(&self.engine);
                    let host = detect_lan_ip();
                    self.rt.spawn(async move {
                        let _ = eng_rc.ensure_remote_control(&host).await;
                    });
                } else {
                    self.engine.stop_remote_control();
                }

                self.settings_dirty = false;
                if let Some(warn) = warn_toast {
                    self.set_toast(warn, true);
                } else {
                    self.set_toast("Saved all settings", false);
                }
                if close_page {
                    self.set_active_page(ActivePage::Downloads, cx);
                    window.focus(&self.focus_handle);
                }
            }
            Err(e) => self.set_toast(format!("Failed saving settings: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn cancel_settings_internal(
        &mut self,
        show_toast: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_dirty = show_toast && self.has_unsaved_settings_changes(cx);
        let cfg = self.engine.get_config();
        self.settings_cfg = cfg.clone();
        self.settings_tum_tab = cfg.core.current_tum;
        set_macos_hide_dock_icon(cfg.app.hide_dock_icon);
        self.preview_appearance(window, cx);
        self.sync_settings_inputs_from_draft(cx);
        self.load_tum_inputs_from_draft(cx);
        self.settings_dirty = false;
        self.refresh_dock_badge();
        if was_dirty {
            self.set_toast("Reverted unsaved settings", false);
        }
        self.set_active_page(ActivePage::Downloads, cx);
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub(crate) fn cancel_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_settings_internal(true, window, cx);
    }

    pub(crate) fn reset_settings_to_defaults_action(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.engine.reset_settings_to_defaults() {
            Ok(()) => {
                let _ = self.engine.ensure_remote_password();
                let cfg = self.engine.get_config();
                self.settings_cfg = cfg.clone();
                self.settings_tum_tab = cfg.core.current_tum;
                let _ = sync_autorun_on_login(cfg.app.autorun_on_login, cfg.app.launch_minimized);
                let _ = sync_portable_marker(cfg.app.portable_mode);
                set_macos_hide_dock_icon(cfg.app.hide_dock_icon);
                self.preview_appearance(window, cx);
                self.sync_settings_inputs_from_draft(cx);
                self.load_tum_inputs_from_draft(cx);
                self.settings_dirty = false;
                self.refresh_dock_badge();
                self.set_toast("Reset all settings to defaults", false);
            }
            Err(e) => self.set_toast(format!("Reset failed: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn install_native_host(
        &mut self,
        browser: Option<KnownWebBrowser>,
        cx: &mut Context<Self>,
    ) {
        let targets = match browser {
            Some(b) => vec![b],
            None => vec![
                KnownWebBrowser::Chrome,
                KnownWebBrowser::Firefox,
                KnownWebBrowser::Brave,
                KnownWebBrowser::Edge,
                KnownWebBrowser::Vivaldi,
                KnownWebBrowser::Opera,
            ],
        };
        let mut ok_names = Vec::new();
        let mut last_err = None;
        for b in targets {
            match install_native_host_for_browser(b) {
                Ok(_) => ok_names.push(browser_display_name(b)),
                Err(e) => last_err = Some(e.to_string()),
            }
        }
        if !ok_names.is_empty() {
            self.refresh_settings_caches();
            self.set_toast(
                format!("Installed Native Host for: {}", ok_names.join(", ")),
                false,
            );
        } else if let Some(err) = last_err {
            self.set_toast(format!("Install failed: {err}"), true);
        }
        cx.notify();
    }

    pub(crate) fn export_downloads_action(&mut self, cx: &mut Context<Self>) {
        let path = PathBuf::from(self.set_data_io_path.read(cx).text().trim());
        let fmt = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("json")
            .to_string();
        match self
            .engine
            .export_downloads_formatted(&path, &fmt, None, false)
        {
            Ok(n) => self.set_toast(
                format!("Exported {n} downloads to {}", path.display()),
                false,
            ),
            Err(e) => self.set_toast(format!("Export failed: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn export_settings_action(&mut self, cx: &mut Context<Self>) {
        let raw = PathBuf::from(self.set_data_io_path.read(cx).text().trim());
        let path = raw
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(self.engine.data_dir())
            .join("ferrite-settings-export.json");
        match self.engine.export_settings(&path) {
            Ok(()) => self.set_toast(format!("Exported settings to {}", path.display()), false),
            Err(e) => self.set_toast(format!("Settings export failed: {e}"), true),
        }
        cx.notify();
    }

    pub(crate) fn import_downloads_action(&mut self, cx: &mut Context<Self>) {
        let path = PathBuf::from(self.set_data_io_path.read(cx).text().trim());
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            match eng.import_downloads(&path, true).await {
                Ok(n) => q.push(UiAsyncMsg::Toast {
                    text: format!("Imported {n} downloads from {}", path.display()),
                    is_error: false,
                }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Import failed: {e}"),
                    is_error: true,
                }),
            }
        });
    }

    pub(crate) fn archive_diagnostics_action(&mut self, logs_only: bool, cx: &mut Context<Self>) {
        let raw = self.set_diag_zip_path.read(cx).text().trim().to_string();
        let dest = if raw.is_empty() {
            self.engine.data_dir().join("ferrite-diagnostics.zip")
        } else {
            PathBuf::from(raw)
        };
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn_blocking(
            move || match eng.archive_diagnostics_bundle(&dest, logs_only) {
                Ok(zip_path) => q.push(UiAsyncMsg::DiagnosticsArchived { zip_path }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Diagnostics failed: {e}"),
                    is_error: true,
                }),
            },
        );
        cx.notify();
    }

    pub(crate) fn create_portable_action(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.portable_dir_input.read(cx).text().trim().to_string();
        let target = if raw.is_empty() {
            self.engine.data_dir().join("ferrite-portable")
        } else {
            PathBuf::from(raw)
        };
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt
            .spawn_blocking(move || match eng.create_portable_bundle(&target) {
                Ok(dir) => q.push(UiAsyncMsg::PortableCreated { dir }),
                Err(e) => q.push(UiAsyncMsg::Toast {
                    text: format!("Portable creation failed: {e}"),
                    is_error: true,
                }),
            });
        cx.notify();
    }

    pub(crate) fn resolve_conflict_action(
        &mut self,
        id: DownloadId,
        action: FileExistsAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let eng = Arc::clone(&self.engine);
        let q = self.msgs.clone();
        self.rt.spawn(async move {
            if let Err(e) = eng.resolve_conflict(id, action).await {
                q.push(UiAsyncMsg::Toast {
                    text: format!("Conflict error: {e}"),
                    is_error: true,
                });
            }
        });
        self.close_modal(window, cx);
    }
}
