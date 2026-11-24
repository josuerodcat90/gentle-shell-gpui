//! Design system: the active theme (`themes.rs`, Gentleman-Cute by default)
//! as translucent glass, the gpui-component theme mapped onto it so every
//! stock button, input and menu matches, and shared building blocks
//! (cards, chips, key caps, section labels, entrance animations).
//!
//! Views use these instead of ad-hoc colors so every screen looks alike.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationElement, AnimationExt as _, App, BoxShadow, Div, ElementId, EntityId,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Rems,
    SharedString, Stateful, Styled, Window, canvas, div, ease_out_quint, point, px, rems, rgb,
};

use crate::shared::themes::{self, Glass, Theme, mix};

// ── Palette ────────────────────────────────────────────────────────────────

/// Every UI role, resolved from a [`Theme`]. Surfaces are the theme's
/// background tinted toward its text and accent, then made translucent so
/// the platform blur shows through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub light: bool,
    pub window_bg: Hsla,
    pub panel: Hsla,
    pub surface: Hsla,
    pub surface_hover: Hsla,
    pub raised: Hsla,
    pub well: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    /// Input borders: the strong border, made to meet 3:1 (WCAG AA for
    /// the boundary of a control) where the theme's own is softer.
    pub input: Hsla,
    pub fg: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub accent: Hsla,
    pub accent_active: Hsla,
    pub accent_deep: Hsla,
    pub on_accent: Hsla,
    pub champagne: Hsla,
    pub info: Hsla,
    pub success: Hsla,
    pub danger: Hsla,
    pub danger_hover: Hsla,
    pub danger_active: Hsla,
    pub warning: Hsla,
    /// The diagram palette's purple and teal, which the theme's roles don't
    /// cover: its `info` turned to those hues, so they keep the theme's
    /// lightness and saturation (pastel stays pastel) and never collide
    /// with the accent the way a pink terminal magenta would.
    pub purple: Hsla,
    pub teal: Hsla,
    pub selection: Hsla,
    pub popover: Hsla,
    pub screen: Hsla,
    pub scrollbar_hover: Hsla,
    pub backdrop: Hsla,
}

fn color(hex: u32, alpha: f32) -> Hsla {
    let mut c: Hsla = rgb(hex).into();
    c.a = alpha;
    c
}

/// `hex` turned to `degrees` of hue, its saturation and lightness kept.
fn with_hue(hex: u32, degrees: f32) -> Hsla {
    let mut c = color(hex, 1.);
    c.h = degrees / 360.;
    c
}

impl Palette {
    /// The theme's roles under `glass` (the Appearance sliders).
    ///
    /// Transparency fades the chrome (the window's ground: title bar, the
    /// gaps between panes) from its solid alpha toward a floor. The text
    /// areas (panes, sidebars, cards, code and terminal wells, selected
    /// rows) start from that same fade and are then held readable: never
    /// clearer than the alpha at which every text color still meets WCAG AA
    /// over a pure black *and* a pure white backdrop (whatever the desktop
    /// or the theme's picture shows lies between them, and a blur only
    /// averages it), and Opacity takes them from there (0) to opaque (1).
    ///
    /// Text colors are nudged first (toward white or black, whichever is
    /// the smaller change, by the least amount) wherever the theme's own
    /// pick would fall short on its backings at [`INK_COVERAGE`], so the
    /// floor never has to make a pane opaque.
    pub fn new(t: &Theme, glass: Glass) -> Self {
        use gentle_core::contrast::{AA_TEXT, AA_UI, Rgb, WORST_BACKDROPS, min_alpha, readable};

        let clear = glass.transparency.clamp(0., 1.);
        let opacity = glass.opacity.clamp(0., 1.);
        let fade = |solid: f32, floor: f32| solid - (solid - floor) * clear;
        let opaque = t.solid || t.oled;
        // Cards lean toward the text color with a hint of the accent.
        let tint = mix(t.fg, t.accent, 0.6);
        let tinted = |amount: f32| mix(t.bg, tint, amount);
        let accent_deep = mix(t.accent, t.bg, 0.2);

        // Each glass role's color, before alpha. OLED: every large surface
        // is true black (blur or a tint would light pixels that should be
        // off); cards and menus sit just above black so they read as layers.
        let black = |other: u32| if t.oled { 0x000000 } else { other };
        let window_c = black(mix(t.bg, t.fg, 0.03));
        let panel_c = black(tinted(0.045));
        let surface_c = if t.oled {
            mix(t.bg, t.raised, 0.5)
        } else {
            tinted(0.09)
        };
        let hover_c = if t.oled { t.raised } else { tinted(0.15) };
        let raised_c = t.raised;
        let well_c = black(if t.light {
            mix(t.bg, t.fg, 0.035)
        } else {
            tinted(0.02)
        });
        let popover_c = if t.oled {
            mix(t.bg, t.raised, 0.4)
        } else {
            tinted(0.065)
        };
        let screen_c = black(tinted(0.055));

        // Text colors, made to read on every backing text sits on. Glass
        // backings are taken at `INK_COVERAGE` over the worst backdrops, so
        // the theme's text keeps some glass under it, not just a wall.
        let worst = WORST_BACKDROPS.map(Rgb::hex);
        let coverage = if opaque { 1. } else { INK_COVERAGE };
        let mut backings: Vec<Rgb> = [popover_c, screen_c].map(Rgb::hex).to_vec();
        for c in [panel_c, surface_c, hover_c, raised_c, well_c] {
            backings.extend(worst.map(|under| Rgb::hex(c).over(coverage, under)));
        }
        let ink = |hex: u32, target: f64| readable(Rgb::hex(hex), &backings, target).to_hex();
        let fg = ink(t.fg, AA_TEXT);
        let muted = ink(t.muted, AA_TEXT);
        let accent = ink(t.accent, AA_TEXT);
        let faint = ink(t.faint, AA_UI);
        let info = ink(t.info, AA_UI);
        let success = ink(t.success, AA_UI);
        let danger = ink(t.danger, AA_UI);
        let warning = ink(t.warning, AA_UI);
        let input = ink(t.border, AA_UI);
        let on_accent = readable(
            Rgb::hex(if t.light {
                0xffffff
            } else {
                mix(t.bg, t.accent, 0.08)
            }),
            &[Rgb::hex(accent)],
            AA_TEXT,
        )
        .to_hex();

        // What each text area needs to read, over what is below it.
        let needs = [
            (fg, AA_TEXT),
            (muted, AA_TEXT),
            (accent, AA_TEXT),
            (faint, AA_UI),
            (info, AA_UI),
            (success, AA_UI),
            (danger, AA_UI),
            (warning, AA_UI),
            (input, AA_UI),
        ]
        .map(|(c, target)| (Rgb::hex(c), target));
        let window_a = if opaque {
            1.
        } else {
            fade(if t.light { 0.92 } else { 0.9 }, 0.3)
        };
        // A text area's alpha: the transparency's fade, held at the AA
        // floor, then lifted toward opaque by Opacity.
        let text_alpha = |c: u32, faded: f32, below: &[Rgb]| -> f32 {
            if opaque {
                return 1.;
            }
            let floor = min_alpha(Rgb::hex(c), below, &needs) as f32;
            let low = faded.max(floor);
            low + (1. - low) * opacity
        };
        // A pane sits on the window's ground over the desktop; a theme's
        // backdrop (sky, picture, scene) is drawn over that ground, so there
        // the pane sits right on whatever the backdrop shows.
        let under_window = if t.background.is_some() {
            worst
        } else {
            worst.map(|d| Rgb::hex(window_c).over(f64::from(window_a), d))
        };
        let panel_a = text_alpha(panel_c, fade(0.93, 0.16), &under_window);
        // Cards, wells and rows sit on a pane.
        let under_panel = under_window.map(|b| Rgb::hex(panel_c).over(f64::from(panel_a), b));
        let surface_a = text_alpha(surface_c, fade(0.94, 0.4), &under_panel);
        let hover_a = text_alpha(hover_c, fade(0.96, 0.55), &under_panel);
        let raised_a = text_alpha(raised_c, fade(0.96, 0.7), &under_panel);
        let well_a = text_alpha(well_c, fade(0.92, 0.45), &under_panel);

        let solid = |hex| color(hex, 1.);
        Self {
            light: t.light,
            window_bg: color(window_c, window_a),
            panel: color(panel_c, panel_a),
            surface: color(surface_c, surface_a),
            surface_hover: color(hover_c, hover_a),
            raised: color(raised_c, raised_a),
            well: color(well_c, well_a),
            border: if t.oled {
                solid(mix(t.bg, t.border, 0.6))
            } else {
                color(
                    tinted(0.2),
                    if t.solid {
                        1.
                    } else if t.light {
                        0.8
                    } else {
                        0.69
                    },
                )
            },
            border_strong: solid(t.border),
            input: solid(input),
            fg: solid(fg),
            muted: solid(muted),
            faint: solid(faint),
            accent: solid(accent),
            accent_active: solid(t.accent_active),
            accent_deep: solid(accent_deep),
            on_accent: solid(on_accent),
            champagne: solid(t.heading),
            info: solid(info),
            success: solid(success),
            danger: solid(danger),
            danger_hover: solid(mix(danger, 0xffffff, 0.2)),
            danger_active: solid(mix(danger, 0x000000, 0.1)),
            warning: solid(warning),
            purple: with_hue(info, 275.),
            teal: with_hue(info, 172.),
            selection: color(mix(t.selection, accent_deep, 0.5), 0.6),
            // Menus, the palette and screens float over content: solid, whatever
            // the transparency, so the conversation behind never bleeds through.
            popover: solid(popover_c),
            screen: solid(screen_c),
            scrollbar_hover: solid(mix(t.border, t.fg, 0.2)),
            backdrop: color(
                0x000000,
                if t.oled {
                    0.75
                } else if t.light {
                    0.25
                } else {
                    0.44
                },
            ),
        }
    }
}

/// How much of a glass text area the ink is made to read over (with the
/// worst backdrop showing through the rest). A balance: lower keeps more
/// glass but recolors more of a theme (0.85 turned Solarized's blue and
/// Gruvbox's orange pale); 1 only fixes colors that fail on the theme's
/// own background but leaves the panes nearly opaque.
pub const INK_COVERAGE: f64 = 0.92;

/// Panes, sidebars and screens.
pub fn panel() -> Hsla {
    themes::palette(|p| p.panel)
}
/// Cards and fields.
pub fn surface() -> Hsla {
    themes::palette(|p| p.surface)
}
pub fn surface_hover() -> Hsla {
    themes::palette(|p| p.surface_hover)
}
/// User messages, selected rows.
pub fn raised() -> Hsla {
    themes::palette(|p| p.raised)
}
/// Code blocks and terminal wells, darker than a card.
pub fn well() -> Hsla {
    themes::palette(|p| p.well)
}
pub fn border() -> Hsla {
    themes::palette(|p| p.border)
}
pub fn border_strong() -> Hsla {
    themes::palette(|p| p.border_strong)
}
/// Borders of text fields: at least 3:1 against what is around them.
pub fn input() -> Hsla {
    themes::palette(|p| p.input)
}
pub fn fg() -> Hsla {
    themes::palette(|p| p.fg)
}
pub fn muted() -> Hsla {
    themes::palette(|p| p.muted)
}
pub fn faint() -> Hsla {
    themes::palette(|p| p.faint)
}
/// Interactive accent: links, focus, selected state.
pub fn accent() -> Hsla {
    themes::palette(|p| p.accent)
}
/// The brand color (the theme's accent; Gentle Shell's pink by default).
pub fn petal() -> Hsla {
    themes::palette(|p| p.accent)
}
pub fn petal_active() -> Hsla {
    themes::palette(|p| p.accent_active)
}
/// Tool titles, headings, inline code.
pub fn champagne() -> Hsla {
    themes::palette(|p| p.champagne)
}
/// Informational blue (running state, models).
pub fn info() -> Hsla {
    themes::palette(|p| p.info)
}
pub fn success() -> Hsla {
    themes::palette(|p| p.success)
}
pub fn danger() -> Hsla {
    themes::palette(|p| p.danger)
}
pub fn warning() -> Hsla {
    themes::palette(|p| p.warning)
}
pub fn purple() -> Hsla {
    themes::palette(|p| p.purple)
}
pub fn teal() -> Hsla {
    themes::palette(|p| p.teal)
}

/// A diagram element's named color in this theme's tones.
pub fn tint(tint: gentle_core::genui::style::Tint) -> Hsla {
    use gentle_core::genui::style::Tint;
    match tint {
        Tint::Blue => info(),
        Tint::Green => success(),
        Tint::Yellow => warning(),
        Tint::Red => danger(),
        Tint::Purple => purple(),
        Tint::Teal => teal(),
        Tint::Pink => accent(),
        Tint::Gold => champagne(),
        Tint::Gray => muted(),
    }
}

/// Opaque-ish glass for full-window screens and palettes.
pub fn screen_bg() -> Hsla {
    themes::palette(|p| p.screen)
}
/// Floating cards, menus and the task island: solid at any transparency, so
/// what is behind them never bleeds through.
pub fn popover_bg() -> Hsla {
    themes::palette(|p| p.popover)
}
/// The dimmed layer behind screens and dialogs.
pub fn backdrop() -> Hsla {
    themes::palette(|p| p.backdrop)
}

/// The monospace family the app uses when the user hasn't chosen one.
const DEFAULT_MONO: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "DejaVu Sans Mono"
};

/// The kit's default interface and monospace families.
static KIT_FAMILIES: OnceLock<(SharedString, SharedString)> = OnceLock::new();

/// The chosen code and terminal families, set by `apply_theme`; `None` is
/// the default. Render code has no `cx` to ask, so they live here.
static CODE_FONT: RwLock<Option<SharedString>> = RwLock::new(None);
static TERMINAL_FONT: RwLock<Option<SharedString>> = RwLock::new(None);

/// The code and terminal sizes in px at 100 % zoom (f32 bits), set by
/// `apply_theme`. Render code reads them without a `cx`.
static CODE_SIZE: AtomicU32 = AtomicU32::new(gentle_core::fonts::DEFAULT_CODE_SIZE.to_bits());
static TERMINAL_SIZE: AtomicU32 =
    AtomicU32::new(gentle_core::fonts::DEFAULT_TERMINAL_SIZE.to_bits());

/// Code text of `px_at_100` px (at the default code size and 100 % zoom) as
/// rems: it follows the chosen code size and the zoom (the rem), and at the
/// default code size it is exactly `rems(px_at_100 / 16.)`. Every
/// `ui::mono()` text is sized through it.
pub fn code_rems(px_at_100: f32) -> Rems {
    let code = f32::from_bits(CODE_SIZE.load(Ordering::Relaxed));
    rems(px_at_100 * (code / gentle_core::fonts::DEFAULT_CODE_SIZE) / REM_AT_100)
}

/// Linear scale factor from the chosen code size to the default. Canvas/diagram
/// mono text sized directly in px uses this so it follows the code size while
/// keeping its own zoom factor; at the default code size it is exactly `1.0`.
pub fn code_scale() -> f32 {
    f32::from_bits(CODE_SIZE.load(Ordering::Relaxed)) / gentle_core::fonts::DEFAULT_CODE_SIZE
}

/// The rem at 100 % zoom: the interface text size, which only Interface size
/// (the zoom) changes.
const REM_AT_100: f32 = gentle_core::fonts::DEFAULT_INTERFACE_SIZE;

/// The terminal font size in px for a window whose rem size is `rem`: the
/// chosen terminal size times the interface zoom.
pub fn terminal_px(px_at_100: f32, rem: Pixels) -> Pixels {
    let terminal = f32::from_bits(TERMINAL_SIZE.load(Ordering::Relaxed));
    let zoom = f32::from(rem) / REM_AT_100;
    px(px_at_100 * (terminal / gentle_core::fonts::DEFAULT_TERMINAL_SIZE) * zoom)
}

fn chosen_or_default(slot: &RwLock<Option<SharedString>>) -> SharedString {
    slot.read()
        .ok()
        .and_then(|chosen| chosen.clone())
        .unwrap_or_else(default_mono)
}

pub(crate) fn default_mono() -> SharedString {
    SharedString::new_static(DEFAULT_MONO)
}

/// The kit's interface family before any app font override.
pub(crate) fn default_interface(cx: &App) -> SharedString {
    KIT_FAMILIES.get().map_or_else(
        || cx.theme().font_family.clone(),
        |(family, _)| family.clone(),
    )
}

fn store_font(slot: &RwLock<Option<SharedString>>, family: Option<SharedString>) {
    if let Ok(mut chosen) = slot.write() {
        *chosen = family;
    }
}

/// The monospace family for code, paths and numbers in the interface.
pub fn mono() -> SharedString {
    chosen_or_default(&CODE_FONT)
}

/// The family of terminal panes: its own choice, else the code font.
pub fn terminal_mono() -> SharedString {
    chosen_or_default(&TERMINAL_FONT)
}

/// Map the gpui-component theme onto the active palette so stock components
/// match. Switches gpui-component's light/dark mode with the theme first.
/// The interface size sets the font size, which gpui-component's root makes
/// every window's rem size: everything sized in rems follows it. The chosen
/// fonts replace the kit's families; with none chosen they stay as they are.
pub fn apply_theme(cx: &mut App) {
    use gpui_kit::component::{Theme as KitTheme, ThemeMode};
    let p = themes::palette(|p| *p);
    let scale = f32::from(cx.global::<themes::Themes>().interface_size) / 100.;
    let fonts = cx.global::<themes::Themes>().fonts.clone();
    store_font(&CODE_FONT, fonts.code.clone());
    store_font(&TERMINAL_FONT, fonts.terminal_family().cloned());
    use themes::FontSlot;
    let code_size = fonts.size(FontSlot::Code);
    CODE_SIZE.store(code_size.to_bits(), Ordering::Relaxed);
    TERMINAL_SIZE.store(fonts.size(FontSlot::Terminal).to_bits(), Ordering::Relaxed);
    let mode = if p.light {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    if KitTheme::global(cx).is_dark() == p.light {
        KitTheme::change(mode, None, cx);
    }
    let clear = color(0x000000, 0.);
    KitTheme::update(cx, |theme| {
        // The kit's own families, taken before the first override.
        let (family, mono_family) = KIT_FAMILIES
            .get_or_init(|| (theme.font_family.clone(), theme.mono_font_family.clone()));
        theme.font_family = fonts.interface.clone().unwrap_or_else(|| family.clone());
        theme.mono_font_family = fonts.code.clone().unwrap_or_else(|| mono_family.clone());
        let c = &mut theme.colors;
        c.background = p.window_bg;
        c.foreground = p.fg;
        c.border = p.border;
        c.input = p.input;
        c.ring = p.accent;
        c.caret = p.accent_active;
        c.selection = p.selection;
        c.muted = p.surface;
        c.muted_foreground = p.muted;
        c.accent = p.surface_hover;
        c.accent_foreground = p.fg;
        c.popover = p.popover;
        c.popover_foreground = p.fg;
        c.list = clear;
        c.list_hover = p.surface_hover;
        c.list_active = p.raised;
        c.list_active_border = p.accent;
        c.primary = p.accent;
        c.primary_hover = p.accent_active;
        c.primary_active = p.accent_deep;
        c.primary_foreground = p.on_accent;
        // Buttons read their own tokens, not `primary`/`secondary`.
        c.button_primary = p.accent;
        c.button_primary_hover = p.accent_active;
        c.button_primary_active = p.accent_deep;
        c.button_primary_foreground = p.on_accent;
        c.button = p.surface;
        c.button_hover = p.surface_hover;
        c.button_active = p.raised;
        c.button_foreground = p.fg;
        c.button_secondary = p.surface;
        c.button_secondary_hover = p.surface_hover;
        c.button_secondary_active = p.raised;
        c.button_secondary_foreground = p.fg;
        c.button_danger = p.danger;
        c.button_danger_hover = p.danger_hover;
        c.button_danger_active = p.danger_active;
        c.button_danger_foreground = p.on_accent;
        c.secondary = p.surface;
        c.secondary_hover = p.surface_hover;
        c.secondary_active = p.raised;
        c.secondary_foreground = p.fg;
        c.danger = p.danger;
        c.danger_hover = p.danger_hover;
        c.danger_active = p.danger_active;
        c.danger_foreground = p.on_accent;
        c.success = p.success;
        c.warning = p.warning;
        c.info = p.info;
        c.link = p.accent;
        c.link_hover = p.accent_active;
        c.scrollbar = clear;
        c.scrollbar_thumb = p.border_strong;
        c.scrollbar_thumb_hover = p.scrollbar_hover;
        c.tab_bar = clear;
        c.tab = clear;
        c.tab_active = p.raised;
        c.tab_foreground = p.muted;
        c.tab_active_foreground = p.fg;
        c.title_bar = clear;
        // Tern has no rule under the title bar; panes start right below it.
        c.title_bar_border = clear;
        c.overlay = color(0x000000, if p.light { 0.3 } else { 0.48 });
        c.switch = p.border_strong;
        c.progress_bar = p.accent;
        c.slider_bar = p.accent;
        c.skeleton = p.surface_hover;
        c.window_border = p.border_strong;
        c.sidebar = p.panel;
        c.sidebar_border = p.border;
        c.sidebar_foreground = p.fg;
        c.sidebar_accent = p.surface_hover;
        c.sidebar_accent_foreground = p.fg;
        c.sidebar_primary = p.accent;
        c.sidebar_primary_foreground = p.on_accent;
        // 16 px at 100 %: the rem every `rems(px_at_100 / 16.)` is written for.
        theme.font_size = px(REM_AT_100 * scale);
        theme.mono_font_size = px(code_size * scale);
        theme.radius = px(8. * scale);
        theme.radius_lg = px(12. * scale);
    });
}

// ── Building blocks ────────────────────────────────────────────────────────

/// The standard card: tool calls, rail sections, settings groups.
pub fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .rounded_xl()
        .bg(surface())
        .border_1()
        .border_color(border())
}

/// A darker inset for code, command output and diffs.
pub fn well_box() -> Div {
    div()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(well())
        .border_1()
        .border_color(border())
        .font_family(mono())
        .text_size(code_rems(12.))
}

/// Small pill: language, model, status.
pub fn chip(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .px_1p5()
        .py(px(1.))
        .rounded_md()
        .text_size(rems(10.5 / 16.))
        .text_color(color)
        .bg(color.opacity(0.12))
        .border_1()
        .border_color(color.opacity(0.22))
        .child(text.into())
}

/// A vertical resize handle for sidebar edges, as wide as the window body's
/// `gap_1p5` (6 px at 100 %) so it fills the gap beside a sidebar. It shows no
/// visible line until hover, when a 2×40 px pill lights up in the accent color.
pub fn resize_handle(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id.into())
        .group("resize-handle")
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(rems(6. / 16.))
        .h_full()
        .cursor_col_resize()
        .child(
            div()
                .w(px(2.))
                .h(px(40.))
                .rounded_full()
                .bg(border_strong())
                .opacity(0.0)
                .group_hover("resize-handle", |s| s.opacity(1.0).bg(petal())),
        )
}

/// A provider's dot color in model pickers: stable per provider name, from
/// the theme's tones.
pub fn provider_color(provider: &str) -> Hsla {
    let palette = [petal(), info(), success(), champagne(), warning()];
    let hash = provider
        .bytes()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
    palette[hash % palette.len()]
}

/// Prefix used by common Cmd shortcuts on macOS and Ctrl shortcuts elsewhere.
///
/// This matches the `key!()` bindings in `app/mod.rs`. Shortcuts that differ
/// beyond their primary modifier should use [`platform_shortcut`] instead.
pub const fn shortcut_prefix() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    }
}

/// Display a Cmd/Ctrl shortcut, such as `⌘K` or `Ctrl+K`.
pub fn shortcut(key: &str) -> String {
    format!("{}{key}", shortcut_prefix())
}

/// Display a Cmd/Ctrl+Shift shortcut, such as `⌘⇧T` or `Ctrl+Shift+T`.
pub fn shortcut_shift(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘⇧{key}")
    } else {
        format!("Ctrl+Shift+{key}")
    }
}

/// A shortcut with genuinely different bindings on each platform.
///
/// For example, tiled panes use `⌘D` but `Ctrl+Shift+D` off macOS.
/// Being `const` also allows static hints in other modules to use it.
pub const fn platform_shortcut(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
}

/// A key cap for shortcut hints (`esc`, `↵`, `⌘`). Use [`key_button`] when the
/// hint stands for one action the user could click.
pub fn kbd(text: impl Into<SharedString>) -> Div {
    div()
        .min_w(rems(20. / 16.))
        .h(rems(20. / 16.))
        .px_1p5()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(border_strong())
        .bg(surface())
        .font_family(mono())
        .text_size(code_rems(10.5))
        .text_color(muted())
        .child(text.into())
}

/// A clickable [`kbd`]: same cap, with a pointer and a hover; the caller adds
/// `.on_click(...)` doing what the key does.
pub fn key_button(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    use gpui_kit::InteractiveElement as _;
    kbd(text)
        .id(id)
        .cursor_pointer()
        .hover(|d| d.text_color(fg()).border_color(petal().opacity(0.6)))
}

/// Uppercase micro heading for sections.
pub fn section_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(rems(10. / 16.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(faint())
        .child(text.into().to_uppercase())
}

/// The full-window screen chrome every overlay shares: dimmed backdrop,
/// centered glass panel with a header (icon, title, subtitle, trailing).
/// `width` is in px at 100 % and grows with the interface size.
pub fn screen(width: Option<f32>) -> (Div, Div) {
    let backdrop = div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(rems(32. / 16.))
        .child(scrim("screen-scrim"));
    let panel = div()
        .flex()
        .flex_col()
        .max_h_full()
        .when_some(width, |d, w| d.w(rems(w / 16.)))
        .when(width.is_none(), |d| d.size_full().max_w(rems(1280. / 16.)))
        .rounded_2xl()
        .border_1()
        .border_color(border_strong())
        .bg(screen_bg())
        .shadow_2xl()
        .overflow_hidden();
    (backdrop, panel)
}

/// Header row for [`screen`] panels.
pub fn screen_header() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .px_5()
        .py_3()
        .gap_3()
        .border_b_1()
        .border_color(border())
}

/// A text field frame for `Input::new(..).appearance(false)`: same height,
/// radius and border everywhere.
pub fn field() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(rems(34. / 16.))
        .px_3()
        .rounded_lg()
        .bg(well())
        .border_1()
        .border_color(input())
}

/// The todo island's floating card: solid, a soft outline, a deep shadow and
/// round corners. Composer menus (model, permissions, chip cards) open in it
/// with `Popover::appearance(false)`, so they match the island instead of
/// the stock popover frame. It sets the UI font and text color itself, as
/// that frame did: a menu opened from a monospace bar must not inherit it.
/// Rows inside should round to [`ISLAND_ROW`].
pub fn island(cx: &App) -> Div {
    div()
        .font_family(cx.theme().font_family.clone())
        .text_color(fg())
        .bg(popover_bg())
        .rounded(rems(18. / 16.))
        .border_1()
        .border_color(border_strong().opacity(0.8))
        .overflow_hidden()
        .shadow(vec![BoxShadow {
            color: backdrop().opacity(0.34),
            offset: point(px(0.), px(10.)),
            blur_radius: px(30.),
            spread_radius: px(2.),
            inset: false,
        }])
}

/// A tooltip that stands off the UI behind it: the stock tooltip with the
/// island's outline and a small soft shadow, so a glass popover surface does
/// not blend into what it covers. Use it with `.tooltip(|window, cx|
/// ui::tooltip(text).build(window, cx))`.
pub fn tooltip(text: impl Into<SharedString>) -> Tooltip {
    Tooltip::new(text.into())
        .border_color(border_strong().opacity(0.8))
        .shadow(vec![BoxShadow {
            color: backdrop().opacity(0.3),
            offset: point(px(0.), px(4.)),
            blur_radius: px(14.),
            spread_radius: px(0.),
            inset: false,
        }])
}

/// Corner radius (px at 100 %) of a row inside an [`island`] with 6 px
/// padding, so the row's corners follow the card's:
/// `.rounded(rems(ISLAND_ROW / 16.))`.
pub const ISLAND_ROW: f32 = 12.;

/// The inset search field for an island picker. The caller owns input state and focus.
pub fn search_well(search: &gpui_kit::Entity<InputState>) -> Div {
    div().p(rems(6. / 16.)).child(
        h_flex()
            .items_center()
            .gap_2()
            .h(rems(36. / 16.))
            .px_3()
            .rounded(rems(ISLAND_ROW / 16.))
            .bg(well())
            .child(Icon::new(IconName::Search).xsmall().text_color(faint()))
            .child(div().flex_1().child(Input::new(search).appearance(false))),
    )
}

/// An island picker row. Highlight follows the cursor, while the check marks
/// the applied choice. The caller supplies the row content and click handler.
pub fn picker_row(
    id: impl Into<ElementId>,
    highlighted: bool,
    chosen: bool,
    icon: Option<IconName>,
    content: impl IntoElement,
) -> Stateful<Div> {
    use gpui_kit::InteractiveElement as _;
    h_flex()
        .id(id)
        .w_full()
        .gap_2()
        .px_3()
        .py_1p5()
        .rounded(rems(ISLAND_ROW / 16.))
        .cursor_pointer()
        .hover(|s| s.bg(surface_hover()))
        .when(highlighted, |d| d.bg(surface_hover()))
        .children(icon.map(|icon| Icon::new(icon).xsmall().text_color(muted())))
        .child(content)
        .when(chosen, |d| {
            d.child(Icon::new(IconName::Check).xsmall().text_color(accent()))
        })
}

/// Segmented toggle (tabs inside a well): one shared look for every
/// "This project / All", "Agents / Activity", "Pro / Max" switch.
pub fn segmented(
    id: &'static str,
    labels: &[&'static str],
    selected: Option<usize>,
    on_select: impl Fn(&usize, &mut gpui_kit::Window, &mut App) + 'static,
) -> Div {
    use gpui_kit::{InteractiveElement as _, StatefulInteractiveElement as _};
    let on_select = std::rc::Rc::new(on_select);
    div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .p(px(2.))
        .gap(px(2.))
        .rounded_lg()
        .bg(well())
        .border_1()
        .border_color(border())
        .children(labels.iter().enumerate().map(|(i, label)| {
            let active = Some(i) == selected;
            let on_select = on_select.clone();
            div()
                .id((id, i))
                .px_2p5()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .text_xs()
                .whitespace_nowrap()
                .when(active, |d| d.bg(raised()).text_color(fg()))
                .when(!active, |d| {
                    d.text_color(muted()).hover(|d| d.text_color(fg()))
                })
                .on_click(move |_, window, cx| on_select(&i, window, cx))
                .child(*label)
        }))
}

// ── Motion ─────────────────────────────────────────────────────────────────

/// A gentle spring with a ~3% overshoot, for anything that opens or unfolds.
pub fn spring(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    let c1 = 0.7;
    let c3 = c1 + 1.;
    1. + c3 * (t - 1.).powi(3) + c1 * (t - 1.).powi(2)
}

/// Fade and rise into place once, when first rendered. It slides with
/// `relative().top()`, like [`pop_in`], so the rows around it don't move.
pub fn appear(id: impl Into<ElementId>, element: Div) -> AnimationElement<Div> {
    element.with_animation(
        id,
        Animation::new(Duration::from_millis(220)).with_easing(ease_out_quint()),
        |el, t| el.opacity(t).relative().top(px((1.0 - t) * 6.0)),
    )
}

/// [`appear`] after `delay`, rising `rise` px: staggered entrances (the
/// welcome screen's intro, then its steps one by one).
pub fn appear_after(
    id: impl Into<ElementId>,
    delay: Duration,
    rise: f32,
    element: Div,
) -> AnimationElement<Div> {
    const RUN: f32 = 420.;
    let wait = delay.as_millis() as f32;
    element.with_animation(
        id,
        Animation::new(Duration::from_millis((wait + RUN) as u64)),
        move |el, t| {
            let local = ((t * (wait + RUN) - wait) / RUN).clamp(0., 1.);
            // ease-out-quint, applied here so the wait stays linear.
            let eased = 1. - (1. - local).powi(5);
            el.opacity(eased).relative().top(px((1. - eased) * rise))
        },
    )
}

/// Screens, dialogs and menus open with the same soft spring: they rise a
/// little, settle with a gentle overshoot, and fade in without a hard pop.
pub fn pop_in(id: impl Into<ElementId>, element: Div) -> AnimationElement<Div> {
    element.with_animation(id, Animation::new(Duration::from_millis(340)), |el, t| {
        // The spring runs in the animator: GPUI's easing must stay in 0..=1.
        let s = spring(t);
        // `top` on a relative element slides visually without nudging the
        // siblings it shares a flex row or column with.
        el.opacity(s.min(1.)).relative().top(px((1. - s) * 12.))
    })
}

/// Fade a popover's content in just behind its surface, without changing the
/// surface's measured size (so the positioner never jumps).
pub fn popover_in(id: impl Into<ElementId>, element: Div) -> AnimationElement<Div> {
    element.with_animation(id, Animation::new(Duration::from_millis(300)), |el, t| {
        el.opacity(spring(t).min(1.))
    })
}

/// The dimmed sheet behind a screen. It fades in with the panel's spring.
pub fn scrim(id: impl Into<ElementId>) -> AnimationElement<Div> {
    div().absolute().inset_0().bg(backdrop()).with_animation(
        id,
        Animation::new(Duration::from_millis(220)).with_easing(ease_out_quint()),
        |el, t| el.opacity(t),
    )
}

// ── Live indicators ────────────────────────────────────────────────────────
//
// Pulses, spinners and shimmers run for a whole agent turn. A `with_animation`
// asks for a frame at the display's rate (120 Hz on ProMotion), and every frame
// re-renders the view it sits in and its ancestors, the whole conversation
// included. These draw from one app clock instead and ask their view for the
// next frame `SLOW_FRAME` after it last drew: ~30 fps, smooth for slow motion,
// a quarter of the work. A view that draws for another reason meanwhile (a
// streamed answer, a sidebar pulse) has already moved its indicators along, so
// the frame waits again rather than drawing on top: indicators and streaming
// share frames instead of adding up. The conversation's own indicators go
// further: they draw on a live overlay ([`crate::shared::overlay`]), so their
// frames redraw the overlay and not the conversation under it.

/// How often live indicators redraw.
const SLOW_FRAME: Duration = Duration::from_millis(33);

/// Seconds since the first live indicator drew; every indicator shares it.
pub(crate) fn live_clock() -> f32 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f32()
}

/// Where a loop of `period` seconds is now, 0..1.
fn live_phase(period: f32) -> f32 {
    (live_clock() / period).fract()
}

/// `window.request_animation_frame()`, counted by the resource diagnostics
/// under the caller's line. Views call this rather than GPUI's directly.
#[track_caller]
pub(crate) fn request_animation_frame(window: &Window) {
    crate::shared::diagnostics::frame_at(std::panic::Location::caller());
    window.request_animation_frame();
}

/// Redraw the view being drawn after [`SLOW_FRAME`], once however many
/// indicators it holds. Nothing with reduced motion.
#[track_caller]
pub(crate) fn request_slow_frame(window: &mut Window, cx: &mut App) {
    request_frame_from(std::panic::Location::caller(), SLOW_FRAME, window, cx);
}

/// Redraw the view being drawn after `delay`, once however many elements
/// ask. Nothing with reduced motion.
#[track_caller]
pub(crate) fn request_frame_in(delay: Duration, window: &mut Window, cx: &mut App) {
    request_frame_from(std::panic::Location::caller(), delay, window, cx);
}

/// [`request_frame_in`], counted under `at`. Every draw of the view moves its
/// pending frame to `delay` after that draw. Each delay keeps its own frame,
/// so a clock's second doesn't hold back a spinner's step.
fn request_frame_from(
    at: &'static std::panic::Location<'static>,
    delay: Duration,
    window: &mut Window,
    cx: &mut App,
) {
    type Pending = std::collections::HashMap<(EntityId, Duration), std::time::Instant>;
    thread_local! {
        /// Frames on the way, by view and delay, with when the view last
        /// asked (drew).
        static PENDING: std::cell::RefCell<Pending> = Default::default();
    }
    if cx.reduce_motion() {
        return;
    }
    crate::shared::diagnostics::frame_at(at);
    let key = (window.current_view(), delay);
    let now = std::time::Instant::now();
    if PENDING.with(|pending| pending.borrow_mut().insert(key, now).is_some()) {
        return;
    }
    cx.spawn(async move |cx| {
        let mut wait = delay;
        loop {
            cx.background_executor().timer(wait).await;
            let since = PENDING.with(|pending| pending.borrow().get(&key).map(|at| at.elapsed()));
            match since {
                Some(since) if since < delay => wait = delay - since,
                _ => break,
            }
        }
        PENDING.with(|pending| pending.borrow_mut().remove(&key));
        cx.update(|cx| cx.notify(key.0));
    })
    .detach();
}

thread_local! {
    /// Set while [`still`] builds: indicators built then ask for no frames.
    static STILL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Builds with `build` indicators that ask for no frames: a still copy
/// that only holds an indicator's place (see [`crate::shared::overlay`]).
pub(crate) fn still<R>(build: impl FnOnce() -> R) -> R {
    let was = STILL.with(|still| still.replace(true));
    let built = build();
    STILL.with(|still| still.set(was));
    built
}

/// A zero-size element that keeps its view redrawing every `delay` while
/// it's on screen, counted under the line that placed the indicator.
/// Nothing when built [`still`].
#[track_caller]
fn frames_every(delay: Duration) -> impl IntoElement {
    let at = std::panic::Location::caller();
    let still = STILL.with(std::cell::Cell::get);
    canvas(
        move |_, window, cx| {
            if !still {
                request_frame_from(at, delay, window, cx)
            }
        },
        |_, _, _, _| {},
    )
    .size_0()
}

/// Keeps a clock counting whole seconds current: one redraw a second,
/// for rows where nothing else moves.
#[track_caller]
pub(crate) fn each_second() -> impl IntoElement {
    frames_every(Duration::from_secs(1))
}

/// A soft breathing pulse for live indicators.
#[track_caller]
pub fn pulse(element: Div) -> Div {
    let breath = gpui_kit::pulsating_between(0.35, 1.0)(live_phase(1.4));
    element.opacity(breath).child(frames_every(SLOW_FRAME))
}

/// A loading spinner (the Loader icon turning), for rows that stay on screen
/// while work runs.
#[track_caller]
pub fn spinner(size: impl Into<gpui_kit::component::Size>, color: Hsla) -> Div {
    div()
        .flex_none()
        .child(
            gpui_kit::component::Icon::new(gpui_kit::component::IconName::Loader)
                .with_size(size)
                .text_color(color)
                .transform(gpui_kit::Transformation::rotate(gpui_kit::percentage(
                    live_phase(0.8),
                ))),
        )
        .child(frames_every(SLOW_FRAME))
}

/// Markdown the agent writes, after Tern's: more air between blocks than
/// the reader's default, so a glance finds where each part starts, and
/// code spans (paths, commands, names) in their own tint, so they stand
/// out of the prose as landmarks. The tint darkens on a light theme, where
/// the theme's teal is too pale to read. Their backing is solid (the
/// popover color with a touch of the tint): the panes are glass, and a
/// translucent wash over a busy theme backdrop lets the code get lost.
/// It goes through the highlight every code span takes, table cells
/// included.
pub fn prose_style() -> gpui_kit::component::text::TextViewStyle {
    gpui_kit::component::text::TextViewStyle::default()
        .paragraph_gap(rems(1.))
        .inline_code(gpui_kit::HighlightStyle {
            color: Some(code_tint()),
            background_color: Some(code_backing()),
            ..Default::default()
        })
        .code_block(prose_code_block())
}

/// The solid backing of the agent's code spans (see [`prose_style`]),
/// path spans included.
pub fn code_backing() -> Hsla {
    solid_mix(popover_bg(), code_tint(), 0.14)
}

/// A fenced block in the agent's Markdown, after Tern's: rounded on a
/// hairline, its language and Copy along the top (the block's actions,
/// see `SessionView::markdown_view`), lines with room to read plain text.
fn prose_code_block() -> gpui_kit::StyleRefinement {
    gpui_kit::StyleRefinement::default()
        .pt(rems(36. / 16.))
        .rounded_lg()
        .border_color(border())
        .line_height(gpui_kit::relative(1.7))
}

/// The code spans' tint: the theme's teal, darker on a light theme.
fn code_tint() -> Hsla {
    let teal = teal();
    if popover_bg().l > 0.5 {
        Hsla {
            l: teal.l * 0.55,
            s: (teal.s * 1.2).min(1.),
            ..teal
        }
    } else {
        teal
    }
}

/// List markers (bullets, numbers) in the agent's Markdown: tinted, so a
/// list's items are found at a glance down its edge.
pub fn prose_list_style() -> gpui_kit::StyleRefinement {
    gpui_kit::StyleRefinement::default().text_color(info())
}

/// `base` moved `t` of the way to `tint`, fully opaque.
fn solid_mix(base: Hsla, tint: Hsla, t: f32) -> Hsla {
    let (a, b) = (gpui_kit::Rgba::from(base), gpui_kit::Rgba::from(tint));
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    gpui_kit::Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: 1.,
    }
    .into()
}

/// `text` with a soft highlight sweeping across it, from `base` to
/// `highlight` and back.
#[track_caller]
pub fn shimmer(text: impl Into<SharedString>, base: Hsla, highlight: Hsla) -> Div {
    const SPREAD: f32 = 0.3;
    let text: SharedString = text.into();
    let count = text.chars().count().max(1) as f32;
    // The band enters from the left edge and leaves past the right one.
    let center = -SPREAD + live_phase(2.0) * (1. + 2. * SPREAD);
    let mut runs = Vec::new();
    for (i, (at, ch)) in text.char_indices().enumerate() {
        let x = (i as f32 + 0.5) / count;
        let glow = (1. - ((x - center) / SPREAD).abs()).max(0.);
        let glow = glow * glow * (3. - 2. * glow);
        let color = Hsla {
            h: if glow > 0.5 { highlight.h } else { base.h },
            s: base.s + (highlight.s - base.s) * glow,
            l: base.l + (highlight.l - base.l) * glow,
            a: base.a + (highlight.a - base.a) * glow,
        };
        runs.push((
            at..at + ch.len_utf8(),
            gpui_kit::HighlightStyle {
                color: Some(color),
                ..Default::default()
            },
        ));
    }
    div()
        .min_w_0()
        .text_color(base)
        .child(gpui_kit::StyledText::new(text).with_highlights(runs))
        .child(frames_every(SLOW_FRAME))
}

/// A short bright streak orbiting a rounded box clockwise, its tail fading
/// out, for work in progress (round the composer's action button while the
/// agent works). It fills its bounds; `radius` is their corner radius,
/// clamped to half the shorter side, so a large one runs a circle.
#[track_caller]
pub fn orbit(color: Hsla, radius: Rems) -> Div {
    let phase = live_phase(2.4);
    div()
        .size_full()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let radius = radius.to_pixels(window.rem_size());
                    paint_orbit(bounds, radius, phase, color, window);
                },
            )
            .size_full(),
        )
        .child(frames_every(SLOW_FRAME))
}

/// A five-second orbit with a pause on each side, for compaction in progress.
#[track_caller]
pub(crate) fn paused_orbit(color: Hsla, radius: Rems) -> Div {
    let t = live_phase(5.);
    let ease = |x: f32| x * x * (3. - 2. * x);
    let phase = if t < 0.2 {
        0.
    } else if t < 0.55 {
        0.5 * ease((t - 0.2) / 0.35)
    } else if t < 0.75 {
        0.5
    } else {
        0.5 + 0.5 * ease((t - 0.75) / 0.25)
    };
    div()
        .size_full()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    let radius = radius.to_pixels(window.rem_size());
                    paint_orbit(
                        bounds,
                        radius,
                        if cx.reduce_motion() { 0. } else { phase },
                        color,
                        window,
                    );
                },
            )
            .size_full(),
        )
        .child(frames_every(SLOW_FRAME))
}

/// [`orbit`]'s streak: its head `phase` of the way round the border, the
/// tail drawn as pieces of rising opacity.
fn paint_orbit(
    bounds: gpui_kit::Bounds<Pixels>,
    radius: Pixels,
    phase: f32,
    color: Hsla,
    window: &mut Window,
) {
    use std::f32::consts::{FRAC_PI_2, PI};
    const STROKE: f32 = 1.5;
    const PIECES: usize = 16;
    // On the border's own line.
    let inset = STROKE / 2.;
    let w = f32::from(bounds.size.width) - 2. * inset;
    let h = f32::from(bounds.size.height) - 2. * inset;
    if w <= 0. || h <= 0. {
        return;
    }
    let r = (f32::from(radius) - inset).clamp(0., w.min(h) / 2.);
    let (ox, oy) = (
        f32::from(bounds.origin.x) + inset,
        f32::from(bounds.origin.y) + inset,
    );
    // The perimeter, clockwise from the top edge's start: edge, corner, ...
    let (a, b, q) = (w - 2. * r, h - 2. * r, FRAC_PI_2 * r);
    let total = 2. * a + 2. * b + 4. * q;
    let arc = |cx: f32, cy: f32, start: f32, s: f32| {
        let t = start + if q > 0. { s / q * FRAC_PI_2 } else { 0. };
        point(px(ox + cx + r * t.cos()), px(oy + cy + r * t.sin()))
    };
    let at = |s: f32| {
        let mut s = s.rem_euclid(total);
        if s < a {
            return point(px(ox + r + s), px(oy));
        }
        s -= a;
        if s < q {
            return arc(w - r, r, -FRAC_PI_2, s);
        }
        s -= q;
        if s < b {
            return point(px(ox + w), px(oy + r + s));
        }
        s -= b;
        if s < q {
            return arc(w - r, h - r, 0., s);
        }
        s -= q;
        if s < a {
            return point(px(ox + w - r - s), px(oy + h));
        }
        s -= a;
        if s < q {
            return arc(r, h - r, FRAC_PI_2, s);
        }
        s -= q;
        if s < b {
            return point(px(ox), px(oy + h - r - s));
        }
        s -= b;
        arc(r, r, PI, s.min(q))
    };
    let length = (total * 0.2).min(180.);
    let head = phase * total;
    let piece = length / PIECES as f32;
    for i in 0..PIECES {
        let from = head - length + i as f32 * piece;
        let mut path = gpui_kit::PathBuilder::stroke(px(STROKE));
        path.move_to(at(from));
        for k in 1..=4 {
            path.line_to(at(from + piece * k as f32 / 4.));
        }
        if let Ok(path) = path.build() {
            let fade = (i + 1) as f32 / PIECES as f32;
            window.paint_path(path, color.opacity(fade * fade));
        }
    }
}

/// `~/a/b/leaf` split into a muted prefix and the leaf, for path labels.
pub fn split_path(path: &std::path::Path) -> (String, String) {
    let display = match dirs::home_dir() {
        Some(home) if path.starts_with(&home) => {
            let rest = path.strip_prefix(&home).unwrap_or(path);
            if rest.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display())
            }
        }
        _ => path.display().to_string(),
    };
    match display.rfind(['/', '\\']) {
        Some(i) if i + 1 < display.len() => {
            (display[..=i].to_string(), display[i + 1..].to_string())
        }
        _ => (String::new(), display),
    }
}

/// The last `max` lines of `text`, plus how many were hidden.
pub fn tail(text: &str, max: usize) -> (String, usize) {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let hidden = lines.len().saturating_sub(max);
    (lines[hidden..].join("\n"), hidden)
}

pub fn format_duration(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_settles_with_a_soft_overshoot() {
        assert!(spring(0.).abs() < 1e-6);
        assert!((spring(1.) - 1.).abs() < 1e-6);
        let peak = (0..=100)
            .map(|i| spring(i as f32 / 100.))
            .fold(0., f32::max);
        assert!(peak > 1.0 && peak < 1.05, "peak was {peak}");
    }

    #[test]
    fn shortcuts_follow_the_platform_key_bindings() {
        let (simple, shifted, special, prefix) = if cfg!(target_os = "macos") {
            ("⌘K", "⌘⇧T", "⌘D", "⌘")
        } else {
            ("Ctrl+K", "Ctrl+Shift+T", "Ctrl+Shift+D", "Ctrl+")
        };
        assert_eq!(shortcut("K"), simple);
        assert_eq!(shortcut_shift("T"), shifted);
        assert_eq!(platform_shortcut("⌘D", "Ctrl+Shift+D"), special);
        assert_eq!(shortcut_prefix(), prefix);
    }

    #[test]
    fn tail_keeps_last_lines() {
        assert_eq!(tail("a\nb\nc\n", 2), ("b\nc".to_string(), 1));
        assert_eq!(tail("a", 5), ("a".to_string(), 0));
    }

    /// The glass at `transparency`, Opacity 0 (the clearest it gets).
    fn clear_at(transparency: f32) -> Glass {
        Glass {
            transparency,
            blur: 0.,
            opacity: 0.,
        }
    }

    #[test]
    fn palette_follows_the_theme() {
        let catalog = crate::shared::themes::load_catalog(Vec::new(), Vec::new());
        let find = |name: &str| catalog.iter().find(|t| t.name == name).unwrap();
        let cute = Palette::new(find("Gentleman-Cute"), clear_at(0.));
        assert!(!cute.light);
        assert_eq!(cute.fg, Hsla::from(rgb(0xf6eff3)));
        assert_eq!(cute.border_strong, Hsla::from(rgb(0x563040)));
        let latte = Palette::new(find("Catppuccin Latte"), clear_at(0.));
        assert!(latte.light);
        assert!(latte.fg.l < latte.window_bg.l);
    }

    #[test]
    fn transparency_fades_glass_but_not_text_or_modals() {
        let catalog = crate::shared::themes::load_catalog(Vec::new(), Vec::new());
        let cute = catalog.iter().find(|t| t.name == "Gentleman-Cute").unwrap();
        let (solid, clear) = (
            Palette::new(cute, clear_at(0.)),
            Palette::new(cute, clear_at(1.)),
        );
        for (a, b) in [
            (solid.window_bg, clear.window_bg),
            (solid.panel, clear.panel),
            (solid.surface, clear.surface),
        ] {
            assert!(b.a < a.a, "{b:?} should be clearer than {a:?}");
        }
        assert_eq!(solid.fg, clear.fg);
        assert_eq!(solid.accent, clear.accent);
        // Menus, the palette and screens stay solid even at full transparency.
        assert_eq!(clear.popover.a, 1.);
        assert_eq!(clear.screen.a, 1.);
        // Out-of-range values clamp instead of going negative.
        assert_eq!(Palette::new(cute, clear_at(3.)), clear);
    }

    #[test]
    fn chrome_follows_transparency_and_text_areas_hold_their_floor() {
        let catalog = crate::shared::themes::load_catalog(Vec::new(), Vec::new());
        let cute = catalog.iter().find(|t| t.name == "Gentleman-Cute").unwrap();
        let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
        let fade = |solid: f32, floor: f32, clear: f32| solid - (solid - floor) * clear;
        // The chrome is the transparency's fade, nothing else.
        let p = Palette::new(cute, clear_at(0.6));
        assert!(close(p.window_bg.a, fade(0.9, 0.3, 0.6)));
        assert!(close(p.border.a, 0.69));
        // Text areas never go clearer than the fade, nor than their floor:
        // fully transparent, the pane still keeps most of its glass.
        let clear = Palette::new(cute, clear_at(1.));
        assert!(clear.panel.a >= fade(0.93, 0.16, 1.));
        assert!(clear.panel.a > clear.window_bg.a, "{:?}", clear.panel);
        // Opacity lifts them from that floor to opaque, step by step.
        let mut last = 0.;
        for opacity in [0., 0.25, 0.5, 0.75, 1.] {
            let p = Palette::new(
                cute,
                Glass {
                    opacity,
                    ..clear_at(1.)
                },
            );
            assert!(p.panel.a > last, "opacity {opacity}: {:?}", p.panel);
            last = p.panel.a;
            // The chrome doesn't follow Opacity.
            assert_eq!(p.window_bg, clear.window_bg);
        }
        assert_eq!(last, 1.);
        // Blur is the backdrop's business: the palette doesn't change.
        let blurred = Glass {
            blur: 1.,
            ..clear_at(0.6)
        };
        assert_eq!(Palette::new(cute, blurred), p);
    }

    /// Every text and UI color of `p` against every backing it sits on, as
    /// composited over `backdrop`: `(role, ratio, target)`.
    fn contrasts(p: &Palette, backdrop: u32, pictured: bool) -> Vec<(&'static str, f64, f64)> {
        use gentle_core::contrast::{AA_TEXT, AA_UI, Rgb, ratio};
        let split = |c: Hsla| {
            let c = gpui_kit::Rgba::from(c);
            (Rgb([c.r, c.g, c.b].map(f64::from)), f64::from(c.a))
        };
        let over = |c: Hsla, below: Rgb| {
            let (c, a) = split(c);
            c.over(a, below)
        };
        // Panes sit on the window's ground, or right on a theme's backdrop;
        // cards, wells and rows on a pane.
        let ground = if pictured {
            Rgb::hex(backdrop)
        } else {
            over(p.window_bg, Rgb::hex(backdrop))
        };
        let pane = over(p.panel, ground);
        let mut backings = vec![pane, split(p.popover).0, split(p.screen).0];
        for role in [p.surface, p.surface_hover, p.raised, p.well] {
            backings.push(over(role, pane));
        }
        let roles = [
            ("fg", p.fg, AA_TEXT),
            ("muted", p.muted, AA_TEXT),
            ("accent", p.accent, AA_TEXT),
            ("faint", p.faint, AA_UI),
            ("info", p.info, AA_UI),
            ("success", p.success, AA_UI),
            ("danger", p.danger, AA_UI),
            ("warning", p.warning, AA_UI),
            ("input", p.input, AA_UI),
        ];
        let mut out: Vec<_> = roles
            .iter()
            .map(|&(name, c, target)| {
                let worst = backings
                    .iter()
                    .map(|&b| ratio(split(c).0, b))
                    .fold(f64::INFINITY, f64::min);
                (name, worst, target)
            })
            .collect();
        // Labels on accent buttons.
        out.push((
            "on_accent",
            ratio(split(p.on_accent).0, split(p.accent).0),
            AA_TEXT,
        ));
        out
    }

    #[test]
    fn every_builtin_theme_meets_aa_over_any_backdrop() {
        // Text (fg, muted, links in the accent) at 4.5:1; faint hints,
        // status colors and input borders at 3:1; over a black and a white
        // backdrop (the desktop or the theme's picture lies between them),
        // at the defaults and at the clearest the sliders allow.
        let catalog = crate::shared::themes::load_catalog(Vec::new(), Vec::new());
        let mut failures = Vec::new();
        for theme in &catalog {
            for glass in [Glass::DEFAULT, clear_at(1.), clear_at(0.)] {
                let p = Palette::new(theme, glass);
                for backdrop in gentle_core::contrast::WORST_BACKDROPS {
                    let pictured = theme.background.is_some();
                    for (role, ratio, target) in contrasts(&p, backdrop, pictured) {
                        if ratio < target {
                            failures.push(format!(
                                "{} {role} {ratio:.2} < {target} over #{backdrop:06x} at {glass:?}",
                                theme.name
                            ));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn ink_that_already_reads_is_left_alone() {
        // Gentleman-Cute's own text colors read on their glass: no nudge.
        let catalog = crate::shared::themes::load_catalog(Vec::new(), Vec::new());
        let cute = catalog.iter().find(|t| t.name == "Gentleman-Cute").unwrap();
        let p = Palette::new(cute, Glass::DEFAULT);
        assert_eq!(p.fg, Hsla::from(rgb(cute.fg)));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_default_keeps_glass_readable_without_desktop_blur() {
        // Linux compositors rarely blur behind a window (X11 never does), so
        // until the user moves the slider the desktop must not read through.
        let catalog = crate::shared::themes::load_catalog(Vec::new(), Vec::new());
        let cute = catalog.iter().find(|t| t.name == "Gentleman-Cute").unwrap();
        let p = Palette::new(cute, Glass::DEFAULT);
        for surface in [p.window_bg, p.panel, p.surface] {
            assert!(
                surface.a >= 0.8,
                "{surface:?} lets the desktop show through"
            );
        }
    }

    #[test]
    fn solid_surfaces_stay_opaque_at_any_transparency() {
        // A theme asking for solid surfaces (the way Studio does) keeps every
        // glass role alpha 1.0 whatever the transparency, in its own colors.
        let text = r##"{
            "name": "Solid Check",
            "solid": true,
            "vars": {"bg": "#1b1b1d"},
            "colors": {"text": "#ececec", "userMessageBg": "#2a2a2e"}
        }"##;
        let theme = crate::shared::themes::parse_pi_theme(
            text,
            crate::shared::themes::Source::Gentle,
            None,
        )
        .unwrap();
        assert!(theme.solid);
        let raised = Hsla::from(rgb(0x2a2a2e));
        for clear in [0., 0.6, 1.] {
            let p = Palette::new(&theme, clear_at(clear));
            for surface in [p.window_bg, p.panel, p.surface, p.well, p.screen, p.popover] {
                assert_eq!(surface.a, 1., "at transparency {clear}");
            }
            // The theme's own colors, not OLED's black.
            assert_eq!(p.raised, raised, "at transparency {clear}");
            assert!(p.panel != Hsla::from(rgb(0x000000)));
        }
        // Nothing left fades: the whole palette is the same at any setting.
        let all = Glass {
            transparency: 1.,
            blur: 1.,
            opacity: 1.,
        };
        assert_eq!(
            Palette::new(&theme, clear_at(0.)),
            Palette::new(&theme, all)
        );
    }

    #[test]
    fn oled_surfaces_stay_true_black_at_any_transparency() {
        let catalog = crate::shared::themes::load_catalog(Vec::new(), Vec::new());
        let oled = catalog
            .iter()
            .find(|t| t.name == crate::shared::themes::OLED_THEME)
            .unwrap();
        let black = Hsla::from(rgb(0x000000));
        for clear in [0., 0.6, 1.] {
            let p = Palette::new(oled, clear_at(clear));
            for surface in [p.window_bg, p.panel, p.well, p.screen] {
                assert_eq!(surface, black, "at transparency {clear}");
            }
            // Layers above black stay opaque too, so nothing behind them glows.
            assert_eq!((p.surface.a, p.popover.a, p.raised.a), (1., 1., 1.));
        }
    }

    #[test]
    fn code_rems_scales_with_the_chosen_code_size() {
        use std::sync::atomic::Ordering;

        struct Guard(u32);
        impl Drop for Guard {
            fn drop(&mut self) {
                CODE_SIZE.store(self.0, Ordering::Relaxed);
            }
        }

        let guard = Guard(CODE_SIZE.load(Ordering::Relaxed));
        let close = |a: f32, b: f32| (a - b).abs() < 1e-5;

        CODE_SIZE.store(
            gentle_core::fonts::DEFAULT_CODE_SIZE.to_bits(),
            Ordering::Relaxed,
        );
        assert!(close(code_scale(), 1.0));
        assert_eq!(code_rems(12.), rems(12. / 16.));
        assert_eq!(code_rems(16.), rems(1.0));

        CODE_SIZE.store(26_f32.to_bits(), Ordering::Relaxed);
        assert!(close(
            code_scale(),
            26. / gentle_core::fonts::DEFAULT_CODE_SIZE
        ));
        assert_eq!(code_rems(12.), rems(24. / 16.));

        drop(guard);
    }
}
