use crate::app::{ActivePopover, FerriteApp};
use crate::helpers::{
    download_icon_and_color, extract_domain_label, format_added_timestamp, format_bytes,
    format_eta, format_speed, state_badge_info,
};
use crate::i18n;
use crate::theme::parse_hex_color;
use crate::ui_widgets::{
    action_btn, badge_themed, ellipsis_text, ferrite_checkbox, ghost_icon_btn, icon_action_btn,
    slim_progress_bar_active, svg_icon, tooltip_text,
};
use ferrite_core::{
    DownloadPriority, DownloadState, DownloadType, SortField, StateFilter, Tag, TrafficUsageMode,
};
use gpui::{
    AnyElement, Context, FontWeight, MouseButton, SharedString, div, prelude::*, px, rgb,
    uniform_list,
};
use std::collections::HashMap;

impl FerriteApp {
    pub(crate) fn render_sort_header_cell(
        &self,
        id: &'static str,
        label: impl Into<String>,
        field: SortField,
        p: &crate::theme::ThemePalette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let active = self.sort_by == field;
        let arrow = if self.sort_ascending {
            "icons/chevron_up.svg"
        } else {
            "icons/chevron_down.svg"
        };
        let hover_fg = p.text_primary;
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .text_size(p.fpx(12.))
            .text_color(if active { p.text_primary } else { p.text_muted })
            .hover(move |s| s.text_color(hover_fg))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_sort_column(field, cx);
            }))
            .child(label.into())
            .when(active, |el| {
                el.child(svg_icon(arrow, p.spx_f32(11.), p.accent))
            })
    }

    pub(crate) fn render_downloads_table(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.theme;
        let s = i18n::l(&self.settings_cfg.app.locale_name);
        let row_count = self.visible_downloads.len();
        let has_any_downloads = !self.all_downloads.is_empty();
        let compact = self.settings_cfg.app.row_density.is_compact();

        let parent_banner: Option<AnyElement> = self.parent_folder_id.map(|pid| {
            let parent_title = self
                .all_downloads
                .iter()
                .find(|d| d.id == pid)
                .map(|d| d.title.clone())
                .unwrap_or_else(|| format!("Batch #{pid}"));

            div()
                .h(p.spx(36.))
                .flex_shrink_0()
                .px_4()
                .bg(p.bg_200)
                .border_b_1()
                .border_color(p.separator)
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(icon_action_btn(
                            "batch-up-level",
                            "icons/level_up.svg",
                            Some(s.back_to_all.into()),
                            &p,
                            false,
                            cx,
                            |this, _, _, cx| {
                                this.parent_folder_id = None;
                                this.refresh_snapshot(cx);
                                cx.notify();
                            },
                        ))
                        .child(svg_icon("icons/folder_mac.svg", p.spx_f32(14.), p.accent))
                        .child(
                            div()
                                .text_size(p.fpx(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.text_primary)
                                .child(parent_title),
                        )
                        .child(badge_themed(
                            format!("{row_count} items"),
                            p.text_muted,
                            p.bg_surface,
                            &p,
                        )),
                )
                .into_any_element()
        });

        let show_table_header = row_count > 0;

        let header = div()
            .w_full()
            .h(p.spx(32.))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_4()
            .bg(p.bg_100)
            .border_b_1()
            .border_color(p.separator)
            .text_size(p.fpx(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.text_muted)
            .child(
                div()
                    .w(p.spx(62.))
                    .pl(p.spx(30.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .child("#"),
            )
            .child(
                self.render_sort_header_cell("hdr-name", s.col_name, SortField::Title, &p, cx)
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                self.render_sort_header_cell("hdr-size", s.col_size, SortField::Size, &p, cx)
                    .w(p.spx(134.))
                    .flex_shrink_0(),
            )
            .child(
                self.render_sort_header_cell("hdr-status", s.col_status, SortField::Status, &p, cx)
                    .w(p.spx(156.))
                    .flex_shrink_0(),
            )
            .child(div().w(p.spx(76.)).flex_shrink_0().child(s.col_download))
            .child(div().w(p.spx(68.)).flex_shrink_0().child(s.col_upload))
            .child(
                self.render_sort_header_cell(
                    "hdr-added",
                    s.col_added,
                    SortField::CreationTime,
                    &p,
                    cx,
                )
                .w(p.spx(100.))
                .flex_shrink_0(),
            )
            .child(div().w(p.spx(148.)).flex_shrink_0());

        let body: AnyElement = if row_count == 0 {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_8()
                .child(
                    div()
                        .w(p.spx(72.))
                        .h(p.spx(72.))
                        .rounded_full()
                        .bg(p.bg_surface)
                        .border_1()
                        .border_color(p.border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg_icon("icons/tray_empty.svg", p.spx_f32(34.), p.accent)),
                )
                .child(
                    div()
                        .text_size(p.fpx(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.text_primary)
                        .child(if self.parent_folder_id.is_some() {
                            s.empty_batch_title
                        } else if has_any_downloads {
                            s.empty_filter_title
                        } else {
                            s.empty_title
                        }),
                )
                .child(
                    div()
                        .text_size(p.fpx(12.))
                        .text_color(p.text_muted)
                        .child(crate::helpers::format_platform_shortcuts(s.empty_hint)),
                )
                .child(
                    div()
                        .pt_1()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(icon_action_btn(
                            "empty-add-btn",
                            "icons/plus_icon.svg",
                            Some(s.add_download.into()),
                            &p,
                            true,
                            cx,
                            |this, _, w, cx| {
                                this.open_add_modal("", w, cx);
                            },
                        ))
                        .when(self.parent_folder_id.is_some(), |el| {
                            el.child(action_btn(
                                "empty-back-batch",
                                s.back_to_all,
                                &p,
                                false,
                                cx,
                                |this, _, _, cx| {
                                    this.parent_folder_id = None;
                                    this.refresh_snapshot(cx);
                                    cx.notify();
                                },
                            ))
                        })
                        .when(self.parent_folder_id.is_none() && has_any_downloads, |el| {
                            el.child(action_btn(
                                "empty-reset-flt",
                                s.show_all_files,
                                &p,
                                false,
                                cx,
                                |this, _, _, cx| {
                                    this.active_filter = StateFilter::All;
                                    this.active_tag_id = None;
                                    this.search_input.update(cx, |i, cx| i.set_text("", cx));
                                    this.refresh_snapshot(cx);
                                    cx.notify();
                                },
                            ))
                        }),
                )
                .into_any_element()
        } else {
            let row_h = p.spx(if compact { 42. } else { 54. });
            let reveal_tip = if cfg!(target_os = "macos") {
                "Reveal in Finder"
            } else {
                "Show in File Manager"
            };

            uniform_list(
                "dl-rows-list",
                row_count,
                cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                    let p = this.theme;
                    let s = i18n::l(&this.settings_cfg.app.locale_name);
                    let is_snail = this.summary.tum == TrafficUsageMode::Snail;
                    let compact = this.settings_cfg.app.row_density.is_compact();
                    let tag_map: HashMap<i64, &Tag> = this.tags.iter().map(|t| (t.id, t)).collect();

                    let mut items = Vec::with_capacity(range.len());
                    for idx in range {
                        let Some(rec) = this.visible_downloads.get(idx) else {
                            continue;
                        };
                        let id = rec.id;
                        let selected = this.selected_id == Some(id);
                        let hovered = this.hovered_row_id == Some(id);
                        let is_checked = this.checked_ids.contains(&id);
                        let is_batch = rec.download_type == DownloadType::Batch;
                        let is_multi_file_folder = is_batch || rec.files.len() > 1;
                        let frac = rec.progress_fraction();
                        let pct_int = (frac * 100.0).round() as u32;
                        let (st_label, st_color) = state_badge_info(rec, &p);
                        let (type_icon, type_color) = if is_batch {
                            ("icons/single_download_folder_mac.svg", p.accent)
                        } else {
                            download_icon_and_color(rec)
                        };

                        let is_running =
                            rec.state.is_running_or_seeding() || rec.state == DownloadState::Queued;
                        let is_completed = rec.state == DownloadState::Completed;

                        let size_text = if is_batch {
                            let count = rec
                                .module_data
                                .get("batch_count")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(rec.children_ids.len() as u64);
                            format!("{count} items")
                        } else {
                            match rec.total_bytes {
                                Some(total) if is_completed || rec.downloaded_bytes >= total => {
                                    format_bytes(total)
                                }
                                Some(total) => format!(
                                    "{} / {}",
                                    format_bytes(rec.downloaded_bytes),
                                    format_bytes(total)
                                ),
                                None if rec.downloaded_bytes > 0 => {
                                    format_bytes(rec.downloaded_bytes)
                                }
                                None => "--".into(),
                            }
                        };

                        let row_bg = if selected {
                            p.bg_row_selected
                        } else if idx % 2 == 1 {
                            p.bg_row_alt
                        } else {
                            p.bg_app
                        };

                        let tag_swatches: Vec<AnyElement> = rec
                            .tags
                            .iter()
                            .filter_map(|tid| tag_map.get(tid).copied())
                            .take(3)
                            .map(|t| {
                                let c = parse_hex_color(&t.color_hex);
                                div()
                                    .flex_shrink_0()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .px_1()
                                    .rounded(p.radius(4., 2.))
                                    .bg(p.bg_surface)
                                    .border_1()
                                    .border_color(p.border)
                                    .child(div().w(p.spx(6.)).h(p.spx(6.)).rounded(px(2.)).bg(c))
                                    .child(
                                        div()
                                            .text_size(p.fpx(10.))
                                            .text_color(p.text_muted)
                                            .whitespace_nowrap()
                                            .child(t.name.clone()),
                                    )
                                    .into_any_element()
                            })
                            .collect();

                        let progress_fill_color =
                            if rec.state == DownloadState::Downloading && is_snail {
                                p.warning
                            } else {
                                st_color
                            };

                        let status_cell: AnyElement = if is_batch {
                            div()
                                .w(p.spx(156.))
                                .flex_shrink_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1p5()
                                .child(svg_icon("icons/batch.svg", p.spx_f32(13.), p.accent))
                                .child(
                                    div()
                                        .text_size(p.fpx(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.accent)
                                        .truncate()
                                        .child("Batch Group (Open)"),
                                )
                                .into_any_element()
                        } else if is_completed && !rec.flags.is_missing_files {
                            div()
                                .w(p.spx(156.))
                                .flex_shrink_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1p5()
                                .child(svg_icon(
                                    "icons/checkmark_v2.svg",
                                    p.spx_f32(13.),
                                    p.success,
                                ))
                                .child(
                                    div()
                                        .text_size(p.fpx(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.success)
                                        .child("Completed"),
                                )
                                .into_any_element()
                        } else if rec.flags.is_missing_files && is_completed {
                            div()
                                .w(p.spx(156.))
                                .flex_shrink_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1p5()
                                .child(svg_icon(
                                    "icons/triangle_alert.svg",
                                    p.spx_f32(12.),
                                    p.warning,
                                ))
                                .child(
                                    div()
                                        .text_size(p.fpx(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.warning)
                                        .child("Missing Files"),
                                )
                                .into_any_element()
                        } else if rec.state == DownloadState::Error {
                            div()
                                .w(p.spx(156.))
                                .flex_shrink_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1p5()
                                .child(svg_icon(
                                    "icons/single_download_error.svg",
                                    p.spx_f32(13.),
                                    p.danger,
                                ))
                                .child(
                                    div()
                                        .text_size(p.fpx(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.danger)
                                        .child("Error"),
                                )
                                .into_any_element()
                        } else if rec.state == DownloadState::Scheduled {
                            div()
                                .w(p.spx(156.))
                                .flex_shrink_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1p5()
                                .child(svg_icon(
                                    "icons/single_download_scheduler.svg",
                                    p.spx_f32(13.),
                                    p.warning,
                                ))
                                .child(
                                    div()
                                        .text_size(p.fpx(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.warning)
                                        .child(format!("Scheduled ({pct_int}%)")),
                                )
                                .into_any_element()
                        } else {
                            div()
                                .w(p.spx(156.))
                                .flex_shrink_0()
                                .flex()
                                .flex_col()
                                .justify_center()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .child(div().flex_1().child(slim_progress_bar_active(
                                            format!("row-prog-{id}"),
                                            frac,
                                            p.spx_f32(4.0),
                                            progress_fill_color,
                                            p.bg_400,
                                            rec.state == DownloadState::Downloading,
                                        )))
                                        .child(
                                            div()
                                                .w(p.spx(34.))
                                                .text_right()
                                                .text_size(p.fpx(12.))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(p.text_primary)
                                                .child(format!("{pct_int}%")),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(p.fpx(10.))
                                        .text_color(if rec.state == DownloadState::Downloading {
                                            progress_fill_color
                                        } else {
                                            p.text_muted
                                        })
                                        .child(st_label),
                                )
                                .into_any_element()
                        };

                        let added_or_eta_cell: AnyElement = if is_running && rec.eta_secs.is_some()
                        {
                            div()
                                .w(p.spx(100.))
                                .flex_shrink_0()
                                .text_size(p.fpx(12.))
                                .text_color(if is_snail {
                                    p.warning
                                } else {
                                    p.text_secondary
                                })
                                .truncate()
                                .child(format!("{} {}", s.lbl_remaining, format_eta(rec.eta_secs)))
                                .into_any_element()
                        } else {
                            div()
                                .w(p.spx(100.))
                                .flex_shrink_0()
                                .text_size(p.fpx(12.))
                                .text_color(p.text_muted)
                                .child(format_added_timestamp(rec.created_at))
                                .into_any_element()
                        };

                        let subtitle_text = if let Some(ref err) = rec.last_error {
                            err.clone()
                        } else {
                            format!(
                                "{} · {}",
                                extract_domain_label(&rec.url),
                                rec.target_dir.display()
                            )
                        };

                        let primary_row_icon = if is_batch {
                            "icons/folder_open.svg"
                        } else if is_completed {
                            if is_multi_file_folder {
                                "icons/single_download_folder_mac.svg"
                            } else {
                                "icons/single_download_folder.svg"
                            }
                        } else if is_running {
                            "icons/single_download_pause.svg"
                        } else if rec.state == DownloadState::Error {
                            "icons/single_download_error.svg"
                        } else if rec.state == DownloadState::Scheduled {
                            "icons/single_download_scheduler.svg"
                        } else {
                            "icons/single_download_play.svg"
                        };

                        let primary_row_tip = if is_batch {
                            "Open batch folder"
                        } else if is_completed {
                            "Open file"
                        } else if is_running {
                            "Pause download"
                        } else {
                            "Start / resume download"
                        };

                        let show_prio_arrows = hovered || selected;
                        let is_hi_prio = rec.priority == DownloadPriority::High;
                        let is_lo_prio = rec.priority == DownloadPriority::Low;
                        let has_meta_pills = rec.download_type != DownloadType::Www
                            || is_hi_prio
                            || is_lo_prio
                            || rec.flags.sequential_play_asap
                            || !tag_swatches.is_empty();

                        let title_text = rec.title.clone();
                        let dl_bps = rec.download_speed_bps;
                        let ul_bps = rec.upload_speed_bps;
                        let dl_type = rec.download_type;
                        let is_play_asap = rec.flags.sequential_play_asap;
                        let has_err = rec.last_error.is_some();
                        let open_standalone =
                            this.settings_cfg.app.enable_standalone_downloads_windows;

                        items.push(
                            div()
                                .id(SharedString::from(format!("dl-row-{id}")))
                                .w_full()
                                .h(row_h)
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_3()
                                .px_4()
                                .bg(row_bg)
                                .hover(move |s| s.bg(p.bg_row_hover))
                                .border_b_1()
                                .border_color(p.separator)
                                .cursor_pointer()
                                .on_hover(cx.listener(move |this, is_hovered: &bool, _, cx| {
                                    if *is_hovered {
                                        this.hovered_row_id = Some(id);
                                    } else if this.hovered_row_id == Some(id) {
                                        this.hovered_row_id = None;
                                    }
                                    cx.notify();
                                }))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.selected_id = Some(id);
                                        this.context_menu_pos = Some(ev.position);
                                        this.active_popover = ActivePopover::RowContextMenu(id);
                                        this.popover_anim_epoch =
                                            this.popover_anim_epoch.wrapping_add(1);
                                        cx.notify();
                                    }),
                                )
                                .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                                    this.active_popover = ActivePopover::None;
                                    if is_batch && ev.click_count() >= 2 {
                                        this.parent_folder_id = Some(id);
                                        this.refresh_snapshot(cx);
                                        cx.notify();
                                        return;
                                    }
                                    if !is_batch && ev.click_count() >= 2 && open_standalone {
                                        this.selected_id = Some(id);
                                        this.open_download_window(id, cx);
                                        cx.notify();
                                        return;
                                    }
                                    if this.selected_id == Some(id)
                                        && this.settings_cfg.app.toggle_bottom_panel_on_click
                                    {
                                        let next = !this.inspector_expanded;
                                        this.set_inspector_expanded(next, cx);
                                    } else {
                                        this.selected_id = Some(id);
                                        if this.settings_cfg.app.toggle_bottom_panel_on_click {
                                            this.set_inspector_expanded(true, cx);
                                        } else {
                                            cx.notify();
                                        }
                                    }
                                }))
                                .child(
                                    div()
                                        .w(p.spx(62.))
                                        .flex_shrink_0()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("chk-{id}")))
                                                .w(p.spx(22.))
                                                .h(p.spx(22.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.toggle_check(id, cx);
                                                }))
                                                .child(ferrite_checkbox(is_checked, &p)),
                                        )
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("type-btn-{id}")))
                                                .w(p.spx(if compact { 26. } else { 32. }))
                                                .h(p.spx(if compact { 26. } else { 32. }))
                                                .rounded(p.radius(8., 3.))
                                                .bg(type_color)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    if is_batch {
                                                        this.parent_folder_id = Some(id);
                                                        this.refresh_snapshot(cx);
                                                    } else if is_completed {
                                                        this.open_download_file(id);
                                                    } else {
                                                        this.toggle_download(id);
                                                    }
                                                    cx.notify();
                                                }))
                                                .child(svg_icon(
                                                    type_icon,
                                                    p.spx_f32(if compact { 14. } else { 16. }),
                                                    rgb(0xffffff).into(),
                                                )),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .flex()
                                        .flex_col()
                                        .justify_center()
                                        .gap(px(2.))
                                        .child(
                                            div()
                                                .w_full()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_size(p.fpx(13.))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(p.text_primary)
                                                .child(ellipsis_text(title_text)),
                                        )
                                        .when(!compact || has_err || has_meta_pills, |el| {
                                            el.child(
                                                div()
                                                    .w_full()
                                                    .min_w_0()
                                                    .overflow_hidden()
                                                    .flex()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap_1p5()
                                                    .when(dl_type != DownloadType::Www, |el| {
                                                        el.child(badge_themed(
                                                            dl_type.display_name(),
                                                            p.accent,
                                                            p.accent_soft,
                                                            &p,
                                                        ))
                                                    })
                                                    .when(is_hi_prio, |el| {
                                                        el.child(badge_themed(
                                                            "High",
                                                            p.amber,
                                                            p.bg_surface,
                                                            &p,
                                                        ))
                                                    })
                                                    .when(is_lo_prio, |el| {
                                                        el.child(badge_themed(
                                                            "Low",
                                                            p.text_muted,
                                                            p.bg_surface,
                                                            &p,
                                                        ))
                                                    })
                                                    .when(is_play_asap, |el| {
                                                        el.child(badge_themed(
                                                            "Play ASAP",
                                                            p.success,
                                                            p.bg_surface,
                                                            &p,
                                                        ))
                                                    })
                                                    .children(tag_swatches)
                                                    .when(!compact || has_err, |el| {
                                                        el.child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .overflow_hidden()
                                                                .text_size(p.fpx(11.))
                                                                .text_color(if has_err {
                                                                    p.danger
                                                                } else {
                                                                    p.text_muted
                                                                })
                                                                .child(ellipsis_text(
                                                                    subtitle_text,
                                                                )),
                                                        )
                                                    }),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .w(p.spx(134.))
                                        .flex_shrink_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_size(p.fpx(12.))
                                        .text_color(p.text_secondary)
                                        .truncate()
                                        .child(size_text),
                                )
                                .child(status_cell)
                                .child(
                                    div()
                                        .w(p.spx(76.))
                                        .flex_shrink_0()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1()
                                        .text_size(p.fpx(12.))
                                        .when(dl_bps > 0, |el| {
                                            el.child(svg_icon(
                                                "icons/arrow_down.svg",
                                                p.spx_f32(11.),
                                                p.accent,
                                            ))
                                            .child(
                                                div()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(p.text_primary)
                                                    .child(format_speed(dl_bps)),
                                            )
                                        })
                                        .when(dl_bps == 0, |el| {
                                            el.child(div().text_color(p.text_subtle).child("--"))
                                        }),
                                )
                                .child(
                                    div()
                                        .w(p.spx(68.))
                                        .flex_shrink_0()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1()
                                        .text_size(p.fpx(12.))
                                        .when(ul_bps > 0, |el| {
                                            el.child(svg_icon(
                                                "icons/arrow_up.svg",
                                                p.spx_f32(11.),
                                                p.success,
                                            ))
                                            .child(
                                                div()
                                                    .text_color(p.text_primary)
                                                    .child(format_speed(ul_bps)),
                                            )
                                        })
                                        .when(ul_bps == 0, |el| {
                                            el.child(div().text_color(p.text_subtle).child("--"))
                                        }),
                                )
                                .child(added_or_eta_cell)
                                .child(
                                    div()
                                        .w(p.spx(148.))
                                        .flex_shrink_0()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .justify_end()
                                        .gap_0p5()
                                        .when(show_prio_arrows && !is_batch, |el| {
                                            el.child(
                                                ghost_icon_btn(
                                                    format!("row-prio-up-{id}"),
                                                    "icons/priority_up.svg",
                                                    p.spx_f32(13.),
                                                    if is_hi_prio {
                                                        p.accent
                                                    } else {
                                                        p.text_muted
                                                    },
                                                    p.bg_elevated,
                                                    cx,
                                                    move |this, _, _, cx| {
                                                        cx.stop_propagation();
                                                        this.bump_priority_action(id, true, cx);
                                                    },
                                                )
                                                .tooltip(tooltip_text("Increase priority", &p)),
                                            )
                                            .child(
                                                ghost_icon_btn(
                                                    format!("row-prio-dn-{id}"),
                                                    "icons/priority_down.svg",
                                                    p.spx_f32(13.),
                                                    if is_lo_prio {
                                                        p.warning
                                                    } else {
                                                        p.text_muted
                                                    },
                                                    p.bg_elevated,
                                                    cx,
                                                    move |this, _, _, cx| {
                                                        cx.stop_propagation();
                                                        this.bump_priority_action(id, false, cx);
                                                    },
                                                )
                                                .tooltip(tooltip_text("Decrease priority", &p)),
                                            )
                                        })
                                        .child(
                                            ghost_icon_btn(
                                                format!("row-act-{id}"),
                                                primary_row_icon,
                                                p.spx_f32(14.),
                                                if is_batch || is_completed {
                                                    p.accent
                                                } else if is_running {
                                                    p.text_primary
                                                } else if rec.state == DownloadState::Error {
                                                    p.danger
                                                } else {
                                                    p.success
                                                },
                                                p.bg_elevated,
                                                cx,
                                                move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    if is_batch {
                                                        this.parent_folder_id = Some(id);
                                                        this.refresh_snapshot(cx);
                                                    } else if is_completed {
                                                        this.open_download_file(id);
                                                    } else {
                                                        this.toggle_download(id);
                                                    }
                                                    cx.notify();
                                                },
                                            )
                                            .tooltip(tooltip_text(primary_row_tip, &p)),
                                        )
                                        .child(
                                            ghost_icon_btn(
                                                format!("row-rev-{id}"),
                                                "icons/folder_mac.svg",
                                                p.spx_f32(14.),
                                                p.text_muted,
                                                p.bg_elevated,
                                                cx,
                                                move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.reveal_download_file(id);
                                                    cx.notify();
                                                },
                                            )
                                            .tooltip(tooltip_text(reveal_tip, &p)),
                                        )
                                        .child(
                                            ghost_icon_btn(
                                                format!("row-more-{id}"),
                                                "icons/menu_dots.svg",
                                                p.spx_f32(14.),
                                                p.text_muted,
                                                p.bg_elevated,
                                                cx,
                                                move |this, ev, _, cx| {
                                                    cx.stop_propagation();
                                                    this.selected_id = Some(id);
                                                    this.context_menu_pos = Some(ev.position());
                                                    this.toggle_popover(
                                                        ActivePopover::RowContextMenu(id),
                                                        cx,
                                                    );
                                                },
                                            )
                                            .tooltip(tooltip_text("More actions", &p)),
                                        ),
                                ),
                        );
                    }
                    items
                }),
            )
            .w_full()
            .flex_1()
            .track_scroll(self.downloads_scroll_handle.clone())
            .into_any_element()
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(p.bg_100)
            .when_some(parent_banner, |el, banner| el.child(banner))
            .when(show_table_header, |el| el.child(header))
            .child(body)
            .into_any_element()
    }
}
