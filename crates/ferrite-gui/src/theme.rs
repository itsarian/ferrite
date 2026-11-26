use ferrite_core::{ThemeMode, UiStyle};
use gpui::{Hsla, Pixels, px, rgb, rgba};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Visual palette for Ferrite (`Modern` & `Classic` in Light & Dark modes).
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct ThemePalette {
    pub is_dark: bool,
    pub ui_style: UiStyle,
    pub overall_zoom_pct: u16,
    pub fonts_zoom_pct: u16,
    // Surface & foreground scale (100 = primary canvas -> 1000 = highest contrast foreground)
    pub bg_100: Hsla,
    pub bg_200: Hsla,
    pub bg_300: Hsla,
    pub bg_400: Hsla,
    pub bg_500: Hsla,
    pub bg_600: Hsla,
    pub bg_700: Hsla,
    pub bg_800: Hsla,
    pub bg_900: Hsla,
    pub bg_1000: Hsla,

    // Semantic convenience aliases used across views & widgets
    pub bg_app: Hsla,
    pub bg_sidebar: Hsla,
    pub bg_surface: Hsla,
    pub bg_elevated: Hsla,
    pub bg_row_alt: Hsla,
    pub bg_row_hover: Hsla,
    pub bg_row_selected: Hsla,
    pub bg_input: Hsla,
    pub border: Hsla,
    pub border_focus: Hsla,
    pub separator: Hsla,
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_muted: Hsla,
    pub text_subtle: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_soft: Hsla,
    pub on_accent: Hsla,
    pub success: Hsla,
    pub amber: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub info: Hsla,

    // Special areas & Snail switch tokens
    pub dialog_special_area: Hsla,
    pub snail_bg: Hsla,
    pub snail_border: Hsla,
    pub snail_text: Hsla,
    pub snail_on_bg: Hsla,
    pub snail_on_border: Hsla,
    pub snail_on_fg: Hsla,
    pub snail_off_track: Hsla,
    pub snail_off_border: Hsla,
    pub snail_off_thumb: Hsla,
    pub snail_off_thumb_text: Hsla,
    pub tooltip_bg: Hsla,
    pub tooltip_fg: Hsla,
    pub modal_backdrop: Hsla,
}

impl ThemePalette {
    pub fn from_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => {
                if let Ok(override_theme) = std::env::var("FERRITE_THEME")
                    && override_theme.eq_ignore_ascii_case("dark")
                {
                    return Self::dark();
                }
                Self::light()
            }
            ThemeMode::Dark => {
                if let Ok(override_theme) = std::env::var("FERRITE_THEME")
                    && override_theme.eq_ignore_ascii_case("light")
                {
                    return Self::light();
                }
                Self::dark()
            }
            ThemeMode::System => Self::system_default(),
        }
    }

    pub fn from_config(
        mode: ThemeMode,
        ui_style: UiStyle,
        overall_zoom_pct: u16,
        fonts_zoom_pct: u16,
    ) -> Self {
        Self::from_mode(mode).with_appearance(ui_style, overall_zoom_pct, fonts_zoom_pct)
    }

    pub fn with_appearance(
        mut self,
        ui_style: UiStyle,
        overall_zoom_pct: u16,
        fonts_zoom_pct: u16,
    ) -> Self {
        self.ui_style = ui_style;
        self.overall_zoom_pct = overall_zoom_pct.clamp(75, 150);
        self.fonts_zoom_pct = fonts_zoom_pct.clamp(75, 160);
        if ui_style == UiStyle::Classic {
            if self.is_dark {
                self.bg_app = rgb(0x18191b).into();
                self.bg_200 = rgb(0x222427).into();
                self.bg_300 = rgb(0x2d3034).into();
                self.border = rgb(0x454950).into();
                self.separator = rgb(0x32353b).into();
            } else {
                self.bg_app = rgb(0xfafafa).into();
                self.bg_200 = rgb(0xe7eaee).into();
                self.bg_300 = rgb(0xdde2e8).into();
                self.border = rgb(0xb4becb).into();
                self.separator = rgb(0xd2d8e0).into();
            }
        }
        self
    }

    pub fn ui_scale(&self) -> f32 {
        (self.overall_zoom_pct as f32 / 100.0).clamp(0.75, 1.50)
    }

    pub fn font_scale(&self) -> f32 {
        (self.fonts_zoom_pct as f32 / 100.0).clamp(0.75, 1.60)
    }

    pub fn spx_f32(&self, base: f32) -> f32 {
        (base * self.ui_scale()).round()
    }

    pub fn fpx_f32(&self, base: f32) -> f32 {
        (base * self.font_scale()).round()
    }

    pub fn spx(&self, base: f32) -> Pixels {
        px(self.spx_f32(base))
    }

    pub fn fpx(&self, base: f32) -> Pixels {
        px(self.fpx_f32(base))
    }

    pub fn is_classic(&self) -> bool {
        self.ui_style == UiStyle::Classic
    }

    pub fn radius(&self, modern_px: f32, classic_px: f32) -> Pixels {
        let base = if self.is_classic() {
            classic_px
        } else {
            modern_px
        };
        px((base * self.ui_scale()).round().max(1.0))
    }

    /// Cached system Dark/Light mode detection (avoids spawning `defaults`/`gsettings` per frame).
    pub fn system_default() -> Self {
        if let Ok(override_theme) = std::env::var("FERRITE_THEME") {
            if override_theme.eq_ignore_ascii_case("dark") {
                return Self::dark();
            } else if override_theme.eq_ignore_ascii_case("light") {
                return Self::light();
            }
        }
        static CACHED_DARK: OnceLock<AtomicBool> = OnceLock::new();
        let is_dark = CACHED_DARK
            .get_or_init(|| AtomicBool::new(detect_system_dark_blocking()))
            .load(Ordering::Relaxed);
        if is_dark { Self::dark() } else { Self::light() }
    }

    pub fn dark() -> Self {
        Self {
            is_dark: true,
            ui_style: UiStyle::Modern,
            overall_zoom_pct: 100,
            fonts_zoom_pct: 100,
            bg_100: rgb(0x111318).into(),
            bg_200: rgb(0x171a21).into(),
            bg_300: rgb(0x1f242e).into(),
            bg_400: rgb(0x2b3240).into(),
            bg_500: rgb(0x475166).into(),
            bg_600: rgb(0x6e788d).into(),
            bg_700: rgb(0x949eb3).into(),
            bg_800: rgb(0xb4bdcf).into(),
            bg_900: rgb(0xdde3ee).into(),
            bg_1000: rgb(0xf1f5f9).into(),

            bg_app: rgb(0x111318).into(),
            bg_sidebar: rgb(0x15181f).into(),
            bg_surface: rgb(0x1c212b).into(),
            bg_elevated: rgb(0x252c38).into(),
            bg_row_alt: rgb(0x14171d).into(),
            bg_row_hover: rgb(0x1c212c).into(),
            bg_row_selected: rgba(0x38bdf822).into(),
            bg_input: rgb(0x171b23).into(),
            border: rgb(0x2b3240).into(),
            border_focus: rgb(0x38bdf8).into(),
            separator: rgb(0x212631).into(),
            text_primary: rgb(0xf1f5f9).into(),
            text_secondary: rgb(0xd4dce8).into(),
            text_muted: rgb(0x94a3b8).into(),
            text_subtle: rgb(0x64748b).into(),
            accent: rgb(0x38bdf8).into(),
            accent_hover: rgb(0x56cbf9).into(),
            accent_soft: rgba(0x38bdf820).into(),
            on_accent: rgb(0x0b121c).into(),
            success: rgb(0x2dd4bf).into(),
            amber: rgb(0xf59e0b).into(),
            warning: rgb(0xf59e0b).into(),
            danger: rgb(0xf43f5e).into(),
            info: rgb(0x38bdf8).into(),

            dialog_special_area: rgb(0x171b23).into(),
            snail_bg: rgb(0x78551a).into(),
            snail_border: rgb(0xd99b26).into(),
            snail_text: rgb(0xfff4db).into(),
            snail_on_bg: rgb(0xfef0d2).into(),
            snail_on_border: rgb(0x78551a).into(),
            snail_on_fg: rgb(0x78551a).into(),
            snail_off_track: rgb(0x1f242e).into(),
            snail_off_border: rgb(0x2b3240).into(),
            snail_off_thumb: rgb(0x94a3b8).into(),
            snail_off_thumb_text: rgb(0x111318).into(),
            tooltip_bg: rgb(0x1f242e).into(),
            tooltip_fg: rgb(0xf1f5f9).into(),
            modal_backdrop: gpui::hsla(0.61, 0.25, 0.04, 0.68),
        }
    }

    pub fn light() -> Self {
        Self {
            is_dark: false,
            ui_style: UiStyle::Modern,
            overall_zoom_pct: 100,
            fonts_zoom_pct: 100,
            bg_100: rgb(0xfcfdfe).into(),
            bg_200: rgb(0xf1f5f9).into(),
            bg_300: rgb(0xe6ecf4).into(),
            bg_400: rgb(0xcbd5e1).into(),
            bg_500: rgb(0x94a3b8).into(),
            bg_600: rgb(0x64748b).into(),
            bg_700: rgb(0x475569).into(),
            bg_800: rgb(0x334155).into(),
            bg_900: rgb(0x1e293b).into(),
            bg_1000: rgb(0x0f172a).into(),

            bg_app: rgb(0xfcfdfe).into(),
            bg_sidebar: rgb(0xf1f5f9).into(),
            bg_surface: rgb(0xeef2f7).into(),
            bg_elevated: rgb(0xe2e8f0).into(),
            bg_row_alt: rgb(0xf8fafc).into(),
            bg_row_hover: rgb(0xedf2f8).into(),
            bg_row_selected: rgba(0x0284c71a).into(),
            bg_input: rgb(0xffffff).into(),
            border: rgb(0xcbd5e1).into(),
            border_focus: rgb(0x0284c7).into(),
            separator: rgb(0xe2e8f0).into(),
            text_primary: rgb(0x0f172a).into(),
            text_secondary: rgb(0x334155).into(),
            text_muted: rgb(0x64748b).into(),
            text_subtle: rgb(0x94a3b8).into(),
            accent: rgb(0x0284c7).into(),
            accent_hover: rgb(0x0369a1).into(),
            accent_soft: rgba(0x0284c716).into(),
            on_accent: rgb(0xffffff).into(),
            success: rgb(0x0d9488).into(),
            amber: rgb(0xb45309).into(),
            warning: rgb(0xb45309).into(),
            danger: rgb(0xe11d48).into(),
            info: rgb(0x0284c7).into(),

            dialog_special_area: rgb(0xf1f5f9).into(),
            snail_bg: rgb(0x926012).into(),
            snail_border: rgb(0x926012).into(),
            snail_text: rgb(0xfff3d6).into(),
            snail_on_bg: rgb(0xfef0d2).into(),
            snail_on_border: rgb(0x926012).into(),
            snail_on_fg: rgb(0x926012).into(),
            snail_off_track: rgb(0xffffff).into(),
            snail_off_border: rgb(0x94a3b8).into(),
            snail_off_thumb: rgb(0x94a3b8).into(),
            snail_off_thumb_text: rgb(0xf8fafc).into(),
            tooltip_bg: rgb(0x1e293b).into(),
            tooltip_fg: rgb(0xf8fafc).into(),
            modal_backdrop: gpui::hsla(0.6, 0.15, 0.1, 0.36),
        }
    }
}

fn detect_system_dark_blocking() -> bool {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
        {
            return String::from_utf8_lossy(&out.stdout)
                .trim()
                .eq_ignore_ascii_case("dark");
        }
        false
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(out) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains("prefer-light")
                || (stdout.contains("default") && !stdout.contains("prefer-dark"))
            {
                return false;
            } else if stdout.contains("prefer-dark") {
                return true;
            }
        }
        true
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        true
    }
}

pub fn parse_hex_color(hex: &str) -> Hsla {
    let clean = hex.trim().trim_start_matches('#');
    if let Ok(v) = u32::from_str_radix(clean, 16) {
        rgb(v).into()
    } else {
        rgb(0x3f80e0).into()
    }
}
