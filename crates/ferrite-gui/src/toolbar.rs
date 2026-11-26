use crate::app::{ActivePage, ActivePopover, FerriteApp};
use crate::helpers::{
    format_platform_shortcuts, format_speed, preventer_label, sort_field_short_label,
    state_filter_short_label,
};
use crate::i18n;
use crate::theme::parse_hex_color;
use crate::ui_widgets::{
    badge_themed, danger_btn, ferrite_indeterminate_checkbox, ghost_icon_btn, icon_action_btn,
    snail_switch_btn, svg_icon, tooltip_text,
};
use ferrite_core::{DownloadId, DownloadState, ShutdownType, StateFilter, TrafficUsageMode};
use gpui::{AnyElement, Context, FontWeight, WindowControlArea, div, prelude::*, px};

impl FerriteApp {
    pub(crate) fn render_window_titlebar(&self) -> AnyElement {
        #[cfg(target_os = "macos")]
        {
            let p = self.theme;
            div()
                .id("custom-titlebar")
                .h(p.spx(28.))
                .flex_shrink_0()
                .bg(p.bg_200)
                .border_b_1()
                .border_color(p.separator)
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .window_control_area(WindowControlArea::Drag)
                .on_click(|ev, window, _| {
                    if ev.click_count() == 2 {
                        window.titlebar_double_click();
                    }
                })
                .child(div().w(p.spx(76.)).flex_shrink_0())
                .child(
                    div()
                        .text_size(p.fpx(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.text_secondary)
                        .child("Ferrite"),
                )
                .child(div().w(p.spx(76.)).flex_shrink_0())
                .into_any_element()
        }
        #[cfg(not(target_os = "macos"))]
        {
            div().into_any_element()
        }
    }

    pub(crate) fn render_top_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.theme;
        let s = i18n::l(&self.settings_cfg.app.locale_name);
        let menu_open = self.active_popover == ActivePopover::MainMenu;

        if self.active_page != ActivePage::Downloads {
            let is_settings = matches!(self.active_page, ActivePage::Settings(_));
            let left_cluster = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(icon_action_btn(
                    "tb-back-downloads",
                    "icons/arrow_left.svg",
                    Some(s.back_to_downloads.into()),
                    &p,
                    false,
                    cx,
                    move |this, _, w, cx| {
                        if is_settings {
                            if this.has_unsaved_settings_changes(cx) {
                                this.apply_settings(true, w, cx);
                            } else {
                                this.cancel_settings_internal(false, w, cx);
                            }
                        } else {
                            this.set_active_page(ActivePage::Downloads, cx);
                            w.focus(&this.focus_handle);
                            cx.notify();
                        }
                    },
                ))
                .when(!is_settings, |el| {
                    el.child(div().w(px(1.)).h(p.spx(18.)).bg(p.separator))
                        .child(
                            div()
                                .text_size(p.fpx(14.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.text_primary)
                                .child(s.addons),
                        )
                });

            return div()
                .h(p.spx(54.))
                .flex_shrink_0()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .bg(p.bg_200)
                .border_b_1()
                .border_color(p.separator)
                .child(left_cluster)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            ghost_icon_btn(
                                "btn-theme",
                                if p.is_dark {
                                    "icons/sun.svg"
                                } else {
                                    "icons/moon.svg"
                                },
                                p.spx_f32(16.),
                                p.text_secondary,
                                p.bg_elevated,
                                cx,
                                |this, _, _, cx| {
                                    this.toggle_theme(cx);
                                },
                            )
                            .tooltip(tooltip_text(
                                if p.is_dark {
                                    "Switch to Light theme"
                                } else {
                                    "Switch to Dark theme"
                                },
                                &p,
                            )),
                        )
                        .child(
                            div()
                                .id("btn-main-menu")
                                .w(p.spx(32.))
                                .h(p.spx(32.))
                                .rounded(p.radius(8., 3.))
                                .bg(if menu_open {
                                    p.bg_elevated
                                } else {
                                    gpui::transparent_black()
                                })
                                .hover(move |s| s.bg(p.bg_elevated))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .tooltip(tooltip_text("Main menu", &p))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_popover(ActivePopover::MainMenu, cx);
                                }))
                                .child(svg_icon(
                                    "icons/main_menu_btn.svg",
                                    p.spx_f32(17.),
                                    p.text_primary,
                                )),
                        ),
                )
                .into_any_element();
        }

        let visible_count = self.visible_downloads.len();
        let checked_count = self.checked_ids.len();
        let all_checked = !self.visible_downloads.is_empty()
            && self
                .visible_downloads
                .iter()
                .all(|d| self.checked_ids.contains(&d.id));
        let some_checked = checked_count > 0 && !all_checked;

        let is_filtered = self.active_filter != StateFilter::All || self.active_tag_id.is_some();
        let active_tag = self
            .active_tag_id
            .and_then(|tid| self.tags.iter().find(|t| t.id == tid).cloned());
        let search_non_empty = !self.search_input.read(cx).text().is_empty();

        let left_cluster: AnyElement = if checked_count > 0 {
            let checked_ids_vec: Vec<DownloadId> = self.checked_ids.iter().copied().collect();
            let ids_for_move = checked_ids_vec.clone();
            let ids_for_del = checked_ids_vec;

            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .child(badge_themed(
                    format!("{checked_count} {}", s.selected_word),
                    p.accent,
                    p.accent_soft,
                    &p,
                ))
                .child(icon_action_btn(
                    "sel-start",
                    "icons/play.svg",
                    Some(s.start.into()),
                    &p,
                    false,
                    cx,
                    |this, _, _, cx| {
                        this.batch_start_checked(cx);
                    },
                ))
                .child(icon_action_btn(
                    "sel-pause",
                    "icons/pause.svg",
                    Some(s.pause.into()),
                    &p,
                    false,
                    cx,
                    |this, _, _, cx| {
                        this.batch_pause_checked(cx);
                    },
                ))
                .child(icon_action_btn(
                    "sel-move",
                    "icons/file_move.svg",
                    Some(s.move_to.into()),
                    &p,
                    false,
                    cx,
                    move |this, _, w, cx| {
                        this.open_move_dir_modal(ids_for_move.clone(), w, cx);
                    },
                ))
                .child(danger_btn(
                    "sel-delete",
                    s.delete,
                    &p,
                    cx,
                    move |this, _, w, cx| {
                        this.request_delete_downloads(ids_for_del.clone(), w, cx);
                    },
                ))
                .child(
                    ghost_icon_btn(
                        "sel-clear",
                        "icons/close_sm.svg",
                        p.spx_f32(14.),
                        p.text_muted,
                        p.bg_elevated,
                        cx,
                        |this, _, _, cx| {
                            this.checked_ids.clear();
                            cx.notify();
                        },
                    )
                    .tooltip(tooltip_text("Clear selection", &p)),
                )
                .into_any_element()
        } else {
            let sort_open = self.active_popover == ActivePopover::SortMenu;
            let filter_open = self.active_popover == ActivePopover::FilterMenu;

            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("tb-sort-btn")
                        .h(p.spx(32.))
                        .px_2p5()
                        .rounded(p.radius(8., 3.))
                        .bg(if sort_open {
                            p.bg_elevated
                        } else {
                            p.bg_surface
                        })
                        .hover(move |s| s.bg(p.bg_elevated))
                        .border_1()
                        .border_color(if sort_open { p.accent } else { p.border })
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_popover(ActivePopover::SortMenu, cx);
                        }))
                        .child(svg_icon(
                            "icons/sort_icon.svg",
                            p.spx_f32(14.),
                            p.text_secondary,
                        ))
                        .child(
                            div()
                                .text_size(p.fpx(12.))
                                .text_color(p.text_muted)
                                .child(s.sort_prefix),
                        )
                        .child(
                            div()
                                .text_size(p.fpx(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.text_primary)
                                .child(sort_field_short_label(self.sort_by)),
                        )
                        .child(svg_icon(
                            if self.sort_ascending {
                                "icons/arrow_up.svg"
                            } else {
                                "icons/arrow_down.svg"
                            },
                            p.spx_f32(11.),
                            p.accent,
                        ))
                        .child(svg_icon(
                            "icons/chevron_down.svg",
                            p.spx_f32(12.),
                            p.text_muted,
                        )),
                )
                .child(
                    div()
                        .id("tb-filter-btn")
                        .h(p.spx(32.))
                        .px_2p5()
                        .rounded(p.radius(8., 3.))
                        .bg(if filter_open {
                            p.bg_elevated
                        } else if is_filtered {
                            p.accent_soft
                        } else {
                            p.bg_surface
                        })
                        .hover(move |s| s.bg(p.bg_elevated))
                        .border_1()
                        .border_color(if filter_open || is_filtered {
                            p.accent
                        } else {
                            p.border
                        })
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_popover(ActivePopover::FilterMenu, cx);
                        }))
                        .child(svg_icon(
                            "icons/filter.svg",
                            p.spx_f32(14.),
                            if is_filtered {
                                p.accent
                            } else {
                                p.text_secondary
                            },
                        ))
                        .when_some(active_tag.clone(), |el, tag| {
                            el.child(
                                div()
                                    .w(p.spx(9.))
                                    .h(p.spx(9.))
                                    .rounded(px(2.))
                                    .bg(parse_hex_color(&tag.color_hex)),
                            )
                            .child(
                                div()
                                    .text_size(p.fpx(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.text_primary)
                                    .child(tag.name),
                            )
                        })
                        .when(active_tag.is_none(), |el| {
                            el.child(
                                div()
                                    .text_size(p.fpx(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.text_primary)
                                    .child(state_filter_short_label(self.active_filter)),
                            )
                        })
                        .child(
                            div()
                                .px_1p5()
                                .py(px(1.))
                                .rounded_full()
                                .bg(p.bg_app)
                                .text_size(p.fpx(11.))
                                .text_color(p.text_muted)
                                .child(visible_count.to_string()),
                        )
                        .child(svg_icon(
                            "icons/chevron_down.svg",
                            p.spx_f32(12.),
                            p.text_muted,
                        )),
                )
                .when(is_filtered, |el| {
                    el.child(
                        ghost_icon_btn(
                            "tb-filter-clear",
                            "icons/close_sm.svg",
                            p.spx_f32(13.),
                            p.text_muted,
                            p.bg_elevated,
                            cx,
                            |this, _, _, cx| {
                                this.active_filter = StateFilter::All;
                                this.active_tag_id = None;
                                this.refresh_snapshot(cx);
                                cx.notify();
                            },
                        )
                        .tooltip(tooltip_text("Clear filter", &p)),
                    )
                })
                .into_any_element()
        };

        div()
            .h(p.spx(54.))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .bg(p.bg_200)
            .border_b_1()
            .border_color(p.separator)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2p5()
                    .child(
                        div()
                            .id("tb-check-all")
                            .w(p.spx(28.))
                            .h(p.spx(28.))
                            .rounded(p.radius(6., 2.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(move |s| s.bg(p.bg_elevated))
                            .cursor_pointer()
                            .tooltip(tooltip_text(
                                format_platform_shortcuts("Select all visible (Cmd+Shift+A)"),
                                &p,
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_select_all_visible(cx);
                            }))
                            .child(ferrite_indeterminate_checkbox(
                                all_checked,
                                some_checked,
                                &p,
                            )),
                    )
                    .child(left_cluster),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        icon_action_btn(
                            "btn-add-url",
                            "icons/plus_icon.svg",
                            Some(s.add_download.into()),
                            &p,
                            true,
                            cx,
                            |this, _, w, cx| {
                                this.active_popover = ActivePopover::None;
                                this.open_add_modal("", w, cx);
                            },
                        )
                        .tooltip(tooltip_text(
                            format_platform_shortcuts("Add download URL or torrent (Cmd+N)"),
                            &p,
                        )),
                    )
                    .child(
                        icon_action_btn(
                            "btn-batch",
                            "icons/batch_download_icon.svg",
                            Some(s.batch.into()),
                            &p,
                            false,
                            cx,
                            |this, _, w, cx| {
                                this.active_popover = ActivePopover::None;
                                this.open_batch_modal(w, cx);
                            },
                        )
                        .tooltip(tooltip_text(
                            format_platform_shortcuts("Batch download (Cmd+B)"),
                            &p,
                        )),
                    )
                    .child(
                        div()
                            .w(p.spx(230.))
                            .h(p.spx(32.))
                            .px_2p5()
                            .rounded(p.radius(8., 3.))
                            .bg(p.bg_surface)
                            .border_1()
                            .border_color(p.border)
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .child(svg_icon(
                                "icons/search_icon.svg",
                                p.spx_f32(14.),
                                p.text_muted,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(self.search_input.clone()),
                            )
                            .when(search_non_empty, |el| {
                                el.child(
                                    ghost_icon_btn(
                                        "search-clear",
                                        "icons/close_sm.svg",
                                        p.spx_f32(12.),
                                        p.text_muted,
                                        p.bg_elevated,
                                        cx,
                                        |this, _, _, cx| {
                                            this.search_input
                                                .update(cx, |i, cx| i.set_text("", cx));
                                            this.refresh_snapshot(cx);
                                            cx.notify();
                                        },
                                    )
                                    .tooltip(tooltip_text("Clear search", &p)),
                                )
                            }),
                    )
                    .child(
                        ghost_icon_btn(
                            "btn-theme",
                            if p.is_dark {
                                "icons/sun.svg"
                            } else {
                                "icons/moon.svg"
                            },
                            p.spx_f32(16.),
                            p.text_secondary,
                            p.bg_elevated,
                            cx,
                            |this, _, _, cx| {
                                this.toggle_theme(cx);
                            },
                        )
                        .tooltip(tooltip_text(
                            if p.is_dark {
                                "Switch to Light theme"
                            } else {
                                "Switch to Dark theme"
                            },
                            &p,
                        )),
                    )
                    .child(
                        div()
                            .id("btn-main-menu")
                            .w(p.spx(32.))
                            .h(p.spx(32.))
                            .rounded(p.radius(8., 3.))
                            .bg(if menu_open {
                                p.bg_elevated
                            } else {
                                gpui::transparent_black()
                            })
                            .hover(move |s| s.bg(p.bg_elevated))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .tooltip(tooltip_text("Main menu", &p))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_popover(ActivePopover::MainMenu, cx);
                            }))
                            .child(svg_icon(
                                "icons/main_menu_btn.svg",
                                p.spx_f32(17.),
                                p.text_primary,
                            )),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_status_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.theme;
        let s = i18n::l(&self.settings_cfg.app.locale_name);
        let current_tum = self.summary.tum;
        let is_snail = current_tum == TrafficUsageMode::Snail;
        let bar_bg = if is_snail { p.snail_bg } else { p.bg_200 };
        let bar_fg = if is_snail {
            p.snail_text
        } else {
            p.text_secondary
        };
        let bar_border = if is_snail {
            p.snail_border
        } else {
            p.separator
        };
        let tum_badge_color = match current_tum {
            TrafficUsageMode::High => p.accent,
            TrafficUsageMode::Medium => p.success,
            TrafficUsageMode::Low => p.warning,
            TrafficUsageMode::Snail => p.snail_border,
        };
        let tum_pill_bg = if is_snail {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        } else if current_tum != TrafficUsageMode::High {
            p.accent_soft
        } else {
            p.bg_surface
        };
        let pill_bg = if is_snail {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        } else {
            p.bg_surface
        };
        let pill_border = if is_snail { p.snail_border } else { p.border };

        let dl_speed = format_speed(self.summary.total_download_bps);
        let ul_speed = format_speed(self.summary.total_upload_bps);
        let prev = preventer_label(self.summary.preventer);
        let total_cnt = self.all_downloads.len();
        let active_cnt = self
            .all_downloads
            .iter()
            .filter(|d| d.state.is_running_or_seeding() || d.state == DownloadState::Queued)
            .count();
        let shutdown_active = self.engine.get_config().core.shutdown_when_done;
        let shutdown_badge_lbl = match shutdown_active {
            ShutdownType::None => None,
            ShutdownType::Sleep => Some("Auto-Sleep"),
            ShutdownType::Hibernate => Some("Auto-Hibernate"),
            ShutdownType::Shutdown => Some("Auto-Shutdown"),
            ShutdownType::QuitApp => Some("Auto-Quit"),
        };

        div()
            .h(p.spx(36.))
            .flex_shrink_0()
            .px_3()
            .bg(bar_bg)
            .border_t_1()
            .border_color(bar_border)
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
                    .child(
                        snail_switch_btn("sb-snail-btn", is_snail, &p, cx, |this, _, _, cx| {
                            this.toggle_snail_mode(cx);
                        })
                        .tooltip(tooltip_text(
                            if is_snail {
                                "Disable Snail Mode (restore normal speed)"
                            } else {
                                "Enable Snail Mode (throttle bandwidth)"
                            },
                            &p,
                        )),
                    )
                    .when(is_snail, |el| {
                        el.child(
                            div()
                                .text_size(p.fpx(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.snail_text)
                                .child(s.snail_mode),
                        )
                    })
                    .child(
                        div()
                            .id("sb-tum-btn")
                            .h(p.spx(26.))
                            .px_2p5()
                            .rounded(p.radius(6., 2.))
                            .bg(tum_pill_bg)
                            .hover(|s| s.opacity(0.9))
                            .border_1()
                            .border_color(if !is_snail && current_tum != TrafficUsageMode::High {
                                tum_badge_color
                            } else {
                                pill_border
                            })
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .text_size(p.fpx(12.))
                            .cursor_pointer()
                            .tooltip(tooltip_text("Traffic usage mode & limits", &p))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_popover(ActivePopover::TumMenu, cx);
                            }))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .child(svg_icon(
                                        "icons/arrow_down.svg",
                                        p.spx_f32(11.),
                                        if is_snail { bar_fg } else { p.accent },
                                    ))
                                    .child(div().text_color(bar_fg).child(dl_speed)),
                            )
                            .child(div().w(px(1.)).h(p.spx(12.)).bg(pill_border))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .child(svg_icon(
                                        "icons/arrow_up.svg",
                                        p.spx_f32(11.),
                                        if is_snail { bar_fg } else { p.success },
                                    ))
                                    .child(div().text_color(bar_fg).child(ul_speed)),
                            )
                            .child(div().w(px(1.)).h(p.spx(12.)).bg(pill_border))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1p5()
                                    .child(
                                        div()
                                            .w(p.spx(16.))
                                            .h(p.spx(16.))
                                            .rounded(p.radius(4., 2.))
                                            .bg(if is_snail {
                                                gpui::hsla(0.0, 0.0, 1.0, 0.22)
                                            } else {
                                                tum_badge_color
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(svg_icon(
                                                "icons/trending_up.svg",
                                                p.spx_f32(11.),
                                                if is_snail { p.snail_text } else { p.on_accent },
                                            )),
                                    )
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(bar_fg)
                                            .child(current_tum.label().to_uppercase()),
                                    )
                                    .child(svg_icon(
                                        "icons/arrow_drop_up.svg",
                                        p.spx_f32(10.),
                                        bar_fg,
                                    )),
                            ),
                    )
                    .when_some(prev, |el, lbl| {
                        el.child(
                            div()
                                .px_1p5()
                                .py(px(2.))
                                .rounded(p.radius(4., 2.))
                                .bg(pill_bg)
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .child(svg_icon(
                                    "icons/triangle_alert.svg",
                                    p.spx_f32(11.),
                                    p.warning,
                                ))
                                .child(
                                    div()
                                        .text_size(p.fpx(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.warning)
                                        .child(lbl),
                                ),
                        )
                    })
                    .child(
                        div()
                            .id("sb-qr-btn")
                            .h(p.spx(26.))
                            .px_2()
                            .rounded(p.radius(6., 2.))
                            .hover(move |s| s.bg(pill_bg))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .text_size(p.fpx(12.))
                            .text_color(bar_fg)
                            .cursor_pointer()
                            .tooltip(tooltip_text("Remote control pairing QR", &p))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_remote_qr_modal(cx);
                            }))
                            .child(svg_icon("icons/qr.svg", p.spx_f32(13.), bar_fg))
                            .child(s.remote),
                    )
                    .child(
                        div()
                            .id("sb-privacy-pill")
                            .h(p.spx(26.))
                            .px_2()
                            .rounded(p.radius(6., 2.))
                            .hover(move |s| s.bg(pill_bg))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .text_size(p.fpx(12.))
                            .text_color(if is_snail { bar_fg } else { p.text_muted })
                            .cursor_pointer()
                            .tooltip(tooltip_text("Privacy & local storage status", &p))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_toast(
                                    "Zero telemetry, zero ads, open local SQLite storage",
                                    false,
                                );
                                cx.notify();
                            }))
                            .child(svg_icon(
                                "icons/shield.svg",
                                p.spx_f32(13.),
                                if is_snail { bar_fg } else { p.success },
                            ))
                            .child(s.no_telemetry),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2p5()
                    .when_some(shutdown_badge_lbl, |el, s_lbl| {
                        el.child(badge_themed(s_lbl, p.amber, pill_bg, &p))
                    })
                    .child(div().text_size(p.fpx(12.)).text_color(bar_fg).child(
                        if active_cnt > 0 {
                            format!("{total_cnt} downloads · {active_cnt} active")
                        } else {
                            format!("{total_cnt} downloads")
                        },
                    ))
                    .when(
                        self.active_page == ActivePage::Downloads && self.selected_id.is_some(),
                        |el| {
                            el.child(
                                div()
                                    .id("sb-details-toggle")
                                    .h(p.spx(26.))
                                    .px_2()
                                    .rounded(p.radius(6., 2.))
                                    .bg(pill_bg)
                                    .border_1()
                                    .border_color(pill_border)
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .text_size(p.fpx(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(bar_fg)
                                    .cursor_pointer()
                                    .tooltip(tooltip_text(
                                        format_platform_shortcuts(
                                            "Toggle details inspector (Cmd+I)",
                                        ),
                                        &p,
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let next = !this.inspector_expanded;
                                        this.set_inspector_expanded(next, cx);
                                    }))
                                    .child(s.details)
                                    .child(svg_icon(
                                        if self.inspector_expanded {
                                            "icons/vertical_expand_arrow.svg"
                                        } else {
                                            "icons/vertical_collapse_arrow.svg"
                                        },
                                        p.spx_f32(12.),
                                        bar_fg,
                                    )),
                            )
                        },
                    ),
            )
            .into_any_element()
    }
}
