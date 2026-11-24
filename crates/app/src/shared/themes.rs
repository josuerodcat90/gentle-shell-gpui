//! Themes: the catalog (Gentle Shell's own Pi themes, the user's Pi themes,
//! popular terminal schemes), the active choice and its persistence.
//!
//! The catalog and the selection live in the [`Themes`] gpui global. Views read
//! colors through `ui::*` and the terminal through [`term`], which take no
//! `cx`, so the resolved colors of the active theme are mirrored in a
//! process-wide lock that [`select`] and [`init`] keep in step.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use gentle_core::{fonts, interface_size};
use gpui_kit::{App, Global, SharedString};
use serde_json::Value;

use crate::shared::backdrop::{self, Backdrop};
use crate::shared::ui;

/// The theme used when nothing else is chosen (and the fallback for unknown names).
pub const DEFAULT_THEME: &str = "Gentleman-Cute";

/// Gentle Shell's themes, embedded so the app looks right before (or
/// without) finding the installed `gentle-pi`; the installed files win.
const EMBEDDED: &[&str] = &[
    include_str!("../../themes/Gentleman-Cute.json"),
    include_str!("../../themes/Gentleman-Sexy.json"),
    include_str!("../../themes/Gentle.json"),
    include_str!("../../themes/Studio.json"),
    include_str!("../../themes/Studio-Light.json"),
    include_str!("../../themes/Clay.json"),
    include_str!("../../themes/Clay-Light.json"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `<gentle-pi>/themes/`.
    Gentle,
    /// `<agent dir>/themes/`.
    Pi,
    /// The app's own themes: `<data dir>/gentle-shell-gpui/themes/` and the
    /// examples it ships. They may carry a backdrop (`background`), which
    /// Pi's strict theme schema would reject, so they don't live in Pi's folder.
    App,
    /// Popular terminal schemes shipped with the app.
    Builtin,
}

/// One theme, as opaque `0xRRGGBB` colors. UI roles are resolved from it by
/// `ui::Palette::new`; the terminal reads `ansi`, `fg`, `bg`, `cursor`.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub source: Source,
    pub light: bool,
    /// True black that stays opaque at any transparency, so an OLED panel
    /// turns those pixels off (`ui::Palette::new` keeps every large surface black).
    pub oled: bool,
    /// Opaque surfaces at any transparency, in the theme's own colors:
    /// the theme file sets `"solid": true` (`ui::Palette::new` keeps every
    /// glass role alpha 1.0, like OLED's layers but not black).
    pub solid: bool,
    pub bg: u32,
    pub fg: u32,
    pub muted: u32,
    pub faint: u32,
    pub accent: u32,
    pub accent_active: u32,
    /// User messages, selected rows.
    pub raised: u32,
    /// The strong border.
    pub border: u32,
    /// Tool titles, headings.
    pub heading: u32,
    pub info: u32,
    pub success: u32,
    pub danger: u32,
    pub warning: u32,
    pub cursor: u32,
    pub selection: u32,
    /// Normal 0–7, bright 8–15.
    pub ansi: [u32; 16],
    /// Drawn behind the panes (app themes only).
    pub background: Option<Backdrop>,
    /// The file the theme was read from, when it has one.
    pub file: Option<PathBuf>,
}

// ── Color math ─────────────────────────────────────────────────────────────

/// Linear blend of two `0xRRGGBB` colors: `t = 0` is `a`, `t = 1` is `b`.
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let channel = |shift: u32| {
        let x = ((a >> shift) & 0xff) as f32;
        let y = ((b >> shift) & 0xff) as f32;
        ((x + (y - x) * t).round().clamp(0., 255.) as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// Relative luminance (0 black … 1 white), good enough to tell light from dark.
fn luminance(c: u32) -> f32 {
    let r = ((c >> 16) & 0xff) as f32 / 255.;
    let g = ((c >> 8) & 0xff) as f32 / 255.;
    let b = (c & 0xff) as f32 / 255.;
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// xterm's default 256-color table (Pi themes may name colors by index).
fn xterm256(index: u8) -> u32 {
    const BASE: [u32; 16] = [
        0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
        0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
    ];
    match index {
        0..=15 => BASE[index as usize],
        16..=231 => {
            let i = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v as u32 * 40 };
            (level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)
        }
        232..=255 => {
            let v = 8 + (index - 232) as u32 * 10;
            (v << 16) | (v << 8) | v
        }
    }
}

fn parse_hex(text: &str) -> Option<u32> {
    let hex = text.strip_prefix('#')?;
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok(),
        3 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            Some(((r * 0x11) << 16) | ((g * 0x11) << 8) | (b * 0x11))
        }
        _ => None,
    }
}

// ── Pi theme files ─────────────────────────────────────────────────────────

/// Pi's theme format (`theme-schema.json`): `vars` of hex colors or 256-color
/// indices, and `colors` whose values are a hex color, an index, a var name, or
/// `""` (terminal default). Terminal colors are not part of the format, so the
/// ANSI table comes from ANSI-named vars when present, else from the roles.
///
/// App themes may also be in claude-desktop-themes' shape (`bg`, `text` and
/// `accent` at the top) and carry a backdrop; `file` is where the theme was
/// read from, whose folder holds the backdrop's files.
pub fn parse_pi_theme(text: &str, source: Source, file: Option<&Path>) -> Option<Theme> {
    let value: Value = serde_json::from_str(text).ok()?;
    let name = value.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let vars = value.get("vars").cloned().unwrap_or(Value::Null);
    let colors = value.get("colors").cloned().unwrap_or(Value::Null);
    let export = value.get("export").cloned().unwrap_or(Value::Null);

    fn resolve(value: &Value, vars: &Value, depth: u8) -> Option<u32> {
        match value {
            Value::Number(n) => n.as_u64().filter(|i| *i < 256).map(|i| xterm256(i as u8)),
            Value::String(s) if s.starts_with('#') => parse_hex(s),
            Value::String(s) if !s.is_empty() && depth < 8 => {
                resolve(vars.get(s.as_str())?, vars, depth + 1)
            }
            _ => None,
        }
    }
    let color = |key: &str| colors.get(key).and_then(|v| resolve(v, &vars, 0));
    let var = |key: &str| vars.get(key).and_then(|v| resolve(v, &vars, 0));
    let top = |key: &str| {
        (source == Source::App)
            .then(|| value.get(key).and_then(|v| resolve(v, &vars, 0)))
            .flatten()
    };

    let fg_guess = color("text").or_else(|| top("text"));
    let bg = export
        .get("pageBg")
        .and_then(|v| resolve(v, &vars, 0))
        .or_else(|| var("bg"))
        .or_else(|| top("bg"));
    let light = match (bg, fg_guess) {
        (Some(bg), _) => luminance(bg) > 0.5,
        (None, Some(fg)) => luminance(fg) < 0.5,
        (None, None) => false,
    };
    let bg = bg.unwrap_or(if light { 0xffffff } else { 0x0e0e11 });
    let fg = fg_guess.unwrap_or(if light { 0x1f2328 } else { 0xe6e6e6 });
    let muted = color("muted").unwrap_or_else(|| mix(fg, bg, 0.35));
    let faint = color("dim").unwrap_or_else(|| mix(fg, bg, 0.55));
    let accent = color("accent").or_else(|| top("accent")).unwrap_or(fg);
    let accent_active = color("borderAccent")
        .filter(|c| *c != accent)
        .unwrap_or_else(|| mix(accent, fg, 0.3));
    let success = color("success").unwrap_or(0x8ccf7e);
    let danger = color("error").unwrap_or(0xe06c75);
    let warning = color("warning").unwrap_or(0xe5c07b);
    let keyword = color("syntaxKeyword").unwrap_or(accent);
    let operator = color("syntaxOperator").unwrap_or(muted);

    let mut ansi = derived_ansi(
        light,
        bg,
        fg,
        faint,
        danger,
        success,
        warning,
        color("syntaxFunction").unwrap_or(accent),
        keyword,
        operator,
    );
    const ANSI_VARS: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    for (i, base) in ANSI_VARS.iter().enumerate() {
        if let Some(c) = var(base) {
            ansi[i] = c;
        }
        let capitalized = format!("{}{}", base[..1].to_uppercase(), &base[1..]);
        if let Some(c) = var(&format!("bright{capitalized}")) {
            ansi[i + 8] = c;
        }
    }
    if let Some(c) = var("purple") {
        ansi[5] = c;
    }
    if let Some(table) = gentle_ansi(name) {
        ansi = table;
    }
    let info = var("blue").unwrap_or_else(|| color("syntaxFunction").unwrap_or(ansi[4]));

    // Only the app's themes may have one: Pi's schema rejects the key.
    let background = (source == Source::App)
        .then(|| {
            backdrop::model::parse(&value, file.and_then(Path::parent), |v| {
                resolve(v, &vars, 0)
            })
        })
        .flatten();

    Some(Theme {
        name: name.to_string(),
        source,
        light,
        oled: false,
        solid: value.get("solid").and_then(Value::as_bool).unwrap_or(false),
        bg,
        fg,
        muted,
        faint,
        accent,
        accent_active,
        raised: color("userMessageBg").unwrap_or_else(|| mix(bg, accent, 0.14)),
        border: color("border").unwrap_or_else(|| mix(bg, fg, 0.25)),
        heading: color("toolTitle")
            .or_else(|| color("mdHeading"))
            .unwrap_or(warning),
        info,
        success,
        danger,
        warning,
        cursor: accent_active,
        selection: color("selectedBg").unwrap_or_else(|| mix(bg, accent, 0.25)),
        ansi,
        background,
        file: file.map(Path::to_path_buf),
    })
}

/// A 16-color table from a theme's roles.
#[allow(clippy::too_many_arguments)]
fn derived_ansi(
    light: bool,
    bg: u32,
    fg: u32,
    faint: u32,
    red: u32,
    green: u32,
    yellow: u32,
    blue: u32,
    magenta: u32,
    cyan: u32,
) -> [u32; 16] {
    let (black, white) = if light {
        (mix(fg, bg, 0.1), mix(bg, fg, 0.25))
    } else {
        (mix(bg, fg, 0.12), mix(fg, bg, 0.05))
    };
    let bright = |c: u32| {
        if light {
            mix(c, 0x000000, 0.15)
        } else {
            mix(c, 0xffffff, 0.25)
        }
    };
    [
        black,
        red,
        green,
        yellow,
        blue,
        magenta,
        cyan,
        white,
        faint,
        bright(red),
        bright(green),
        bright(yellow),
        bright(blue),
        bright(magenta),
        bright(cyan),
        if light { fg } else { 0xffffff },
    ]
}

/// Hand-tuned terminal tables for Gentle Shell's own themes.
fn gentle_ansi(name: &str) -> Option<[u32; 16]> {
    Some(match name {
        "Gentleman-Cute" => [
            0x1b1219, 0xff718f, 0xb4e7c7, 0xf2b86d, 0xa9c7ee, 0xf095c8, 0xc4daf6, 0xf6eff3,
            0x76616b, 0xff94ab, 0xc9f0d7, 0xe0c27a, 0xc0d6f3, 0xffb1dd, 0xd8e7fa, 0xffffff,
        ],
        "Gentleman-Sexy" => [
            0x1b1219, 0xff718f, 0xb4e7c7, 0xf2b86d, 0xa9c7ee, 0xf43888, 0xc4daf6, 0xf6eff3,
            0x76616b, 0xff94ab, 0xc9f0d7, 0xe0c27a, 0xc49bff, 0xff4f9a, 0xd8e7fa, 0xffffff,
        ],
        "Gentle" => [
            0x161d2e, 0xcb7c94, 0xb7cc85, 0xdeba87, 0x7fb4ca, 0xc99ad6, 0xa3b5d6, 0xf3f6f9,
            0x8394a3, 0xe0a3b5, 0xd1e8a9, 0xe0c15a, 0x8fb8dd, 0xb99bf2, 0xc3d1e8, 0xffffff,
        ],
        "Studio" => [
            0x25252a, 0xe07a74, 0x7fb08a, 0xd6b46a, 0x7c9cbf, 0xa58abf, 0x7fb0a8, 0xc8c8cc,
            0x55555c, 0xeb928c, 0x98c5a2, 0xe2c68b, 0x9db7d5, 0xbda6d4, 0x9cc7bf, 0xececec,
        ],
        "Studio Light" => [
            0x3a3a3f, 0xb3423a, 0x3f7d4f, 0x9a6b12, 0x3f6a9a, 0x8f6a9e, 0x3f7d8a, 0x9a9aa0,
            0x8e8e93, 0xcc5f57, 0x599a68, 0xb3872f, 0x2f5580, 0x7a5588, 0x2f6572, 0x1f1f1f,
        ],
        "Clay" => [
            0x333230, 0xe0786b, 0x8fb37f, 0xd9b26a, 0x9fb3c8, 0xd8a0b4, 0xa8c8bd, 0xece9e1,
            0x75726b, 0xeb968a, 0xa8c79a, 0xe4c68f, 0xbcccd9, 0xe2b6c6, 0xc2dcd0, 0xf5f2ea,
        ],
        "Clay Light" => [
            0x3e3d39, 0xb0413a, 0x4f7a43, 0x94661a, 0x4a6d8c, 0xa15c74, 0x3f7d78, 0x7d7870,
            0x9a958b, 0xc9544c, 0x64945a, 0xb08124, 0x6a8aa6, 0xb8748c, 0x54938d, 0x2b2a27,
        ],
        _ => return None,
    })
}

/// Every theme file in `dir` (`*.json`, and for the app's own folder shared
/// `*.gentletheme` files too), sorted by name; unreadable files are skipped.
pub fn read_theme_dir(dir: &Path, source: Source) -> Vec<Theme> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut themes: Vec<Theme> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| {
                x == "json" || (source == Source::App && x == backdrop::SHARE_EXTENSION)
            })
        })
        .filter_map(|p| parse_pi_theme(&std::fs::read_to_string(&p).ok()?, source, Some(&p)))
        .collect();
    themes.sort_by_key(|t| t.name.to_lowercase());
    themes
}

// ── Popular schemes ────────────────────────────────────────────────────────

struct Scheme {
    name: &'static str,
    light: bool,
    bg: u32,
    fg: u32,
    cursor: u32,
    selection: u32,
    accent: u32,
    ansi: [u32; 16],
}

#[rustfmt::skip]
const SCHEMES: &[Scheme] = &[
    Scheme { name: "Dracula", light: false, bg: 0x282a36, fg: 0xf8f8f2, cursor: 0xf8f8f2, selection: 0x44475a, accent: 0xbd93f9,
        ansi: [0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
               0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff] },
    Scheme { name: "Nord", light: false, bg: 0x2e3440, fg: 0xd8dee9, cursor: 0xd8dee9, selection: 0x434c5e, accent: 0x88c0d0,
        ansi: [0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
               0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4] },
    Scheme { name: "Tokyo Night", light: false, bg: 0x1a1b26, fg: 0xc0caf5, cursor: 0xc0caf5, selection: 0x283457, accent: 0x7aa2f7,
        ansi: [0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
               0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5] },
    Scheme { name: "Catppuccin Mocha", light: false, bg: 0x1e1e2e, fg: 0xcdd6f4, cursor: 0xf5e0dc, selection: 0x585b70, accent: 0xcba6f7,
        ansi: [0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
               0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8] },
    Scheme { name: "Catppuccin Macchiato", light: false, bg: 0x24273a, fg: 0xcad3f5, cursor: 0xf4dbd6, selection: 0x5b6078, accent: 0xc6a0f6,
        ansi: [0x494d64, 0xed8796, 0xa6da95, 0xeed49f, 0x8aadf4, 0xf5bde6, 0x8bd5ca, 0xb8c0e0,
               0x5b6078, 0xed8796, 0xa6da95, 0xeed49f, 0x8aadf4, 0xf5bde6, 0x8bd5ca, 0xa5adcb] },
    Scheme { name: "Gruvbox Dark", light: false, bg: 0x282828, fg: 0xebdbb2, cursor: 0xebdbb2, selection: 0x504945, accent: 0xfe8019,
        ansi: [0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
               0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2] },
    Scheme { name: "One Dark", light: false, bg: 0x282c34, fg: 0xabb2bf, cursor: 0x528bff, selection: 0x3e4451, accent: 0x61afef,
        ansi: [0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
               0x5c6370, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xffffff] },
    Scheme { name: "Solarized Dark", light: false, bg: 0x002b36, fg: 0x839496, cursor: 0x93a1a1, selection: 0x073642, accent: 0x268bd2,
        ansi: [0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
               0x586e75, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3] },
    Scheme { name: "Rosé Pine", light: false, bg: 0x191724, fg: 0xe0def4, cursor: 0xebbcba, selection: 0x403d52, accent: 0xc4a7e7,
        ansi: [0x26233a, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
               0x6e6a86, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4] },
    Scheme { name: "Kanagawa", light: false, bg: 0x1f1f28, fg: 0xdcd7ba, cursor: 0xc8c093, selection: 0x2d4f67, accent: 0x7e9cd8,
        ansi: [0x090618, 0xc34043, 0x76946a, 0xc0a36e, 0x7e9cd8, 0x957fb8, 0x6a9589, 0xc8c093,
               0x727169, 0xe82424, 0x98bb6c, 0xe6c384, 0x7fb4ca, 0x938aa9, 0x7aa89f, 0xdcd7ba] },
    Scheme { name: "Everforest", light: false, bg: 0x2d353b, fg: 0xd3c6aa, cursor: 0xd3c6aa, selection: 0x543a48, accent: 0xa7c080,
        ansi: [0x475258, 0xe67e80, 0xa7c080, 0xdbbc7f, 0x7fbbb3, 0xd699b6, 0x83c092, 0xd3c6aa,
               0x859289, 0xe67e80, 0xa7c080, 0xdbbc7f, 0x7fbbb3, 0xd699b6, 0x83c092, 0xe4e1cd] },
    Scheme { name: "Monokai", light: false, bg: 0x272822, fg: 0xf8f8f2, cursor: 0xf8f8f0, selection: 0x49483e, accent: 0x66d9ef,
        ansi: [0x272822, 0xf92672, 0xa6e22e, 0xf4bf75, 0x66d9ef, 0xae81ff, 0xa1efe4, 0xf8f8f2,
               0x75715e, 0xf92672, 0xa6e22e, 0xf4bf75, 0x66d9ef, 0xae81ff, 0xa1efe4, 0xf9f8f5] },
    Scheme { name: "Ayu Dark", light: false, bg: 0x0b0e14, fg: 0xbfbdb6, cursor: 0xe6b450, selection: 0x273747, accent: 0xe6b450,
        ansi: [0x01060e, 0xea6c73, 0x7fd962, 0xf9af4f, 0x53bdfa, 0xcda1fa, 0x90e1c6, 0xc7c7c7,
               0x686868, 0xf07178, 0xaad94c, 0xffb454, 0x59c2ff, 0xd2a6ff, 0x95e6cb, 0xffffff] },
    Scheme { name: "Night Owl", light: false, bg: 0x011627, fg: 0xd6deeb, cursor: 0x80a4c2, selection: 0x1d3b53, accent: 0x82aaff,
        ansi: [0x011627, 0xef5350, 0x22da6e, 0xaddb67, 0x82aaff, 0xc792ea, 0x21c7a8, 0xffffff,
               0x575656, 0xef5350, 0x22da6e, 0xffeb95, 0x82aaff, 0xc792ea, 0x7fdbca, 0xffffff] },
    Scheme { name: "GitHub Dark", light: false, bg: 0x0d1117, fg: 0xe6edf3, cursor: 0x2f81f7, selection: 0x264f78, accent: 0x58a6ff,
        ansi: [0x484f58, 0xff7b72, 0x3fb950, 0xd29922, 0x58a6ff, 0xbc8cff, 0x39c5cf, 0xb1bac4,
               0x6e7681, 0xffa198, 0x56d364, 0xe3b341, 0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xffffff] },
    Scheme { name: "Material", light: false, bg: 0x263238, fg: 0xeeffff, cursor: 0xffcc00, selection: 0x37474f, accent: 0x80cbc4,
        ansi: [0x37474f, 0xff5370, 0xc3e88d, 0xffcb6b, 0x82aaff, 0xc792ea, 0x89ddff, 0xeeffff,
               0x546e7a, 0xff5370, 0xc3e88d, 0xffcb6b, 0x82aaff, 0xc792ea, 0x89ddff, 0xffffff] },
    Scheme { name: "Synthwave '84", light: false, bg: 0x262335, fg: 0xf0eff1, cursor: 0x72f1b8, selection: 0x463465, accent: 0xff7edb,
        ansi: [0x34294f, 0xfe4450, 0x72f1b8, 0xfede5d, 0x03edf9, 0xff7edb, 0x03edf9, 0xf0eff1,
               0x614d85, 0xfe4450, 0x72f1b8, 0xf97e72, 0x03edf9, 0xff7edb, 0x03edf9, 0xffffff] },
    Scheme { name: "GitHub Light", light: true, bg: 0xffffff, fg: 0x1f2328, cursor: 0x0969da, selection: 0xb6e3ff, accent: 0x0969da,
        ansi: [0x24292f, 0xcf222e, 0x116329, 0x4d2d00, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781,
               0x57606a, 0xa40e26, 0x1a7f37, 0x633c01, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f] },
    Scheme { name: "Catppuccin Latte", light: true, bg: 0xeff1f5, fg: 0x4c4f69, cursor: 0xdc8a78, selection: 0xacb0be, accent: 0x8839ef,
        ansi: [0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
               0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc] },
];

fn from_scheme(s: &Scheme) -> Theme {
    let a = s.ansi;
    Theme {
        name: s.name.to_string(),
        source: Source::Builtin,
        light: s.light,
        oled: false,
        solid: false,
        bg: s.bg,
        fg: s.fg,
        muted: mix(s.fg, s.bg, 0.35),
        faint: mix(s.fg, s.bg, 0.55),
        accent: s.accent,
        accent_active: mix(s.accent, s.fg, 0.3),
        raised: mix(s.bg, s.accent, if s.light { 0.1 } else { 0.14 }),
        border: mix(s.bg, s.fg, 0.25),
        heading: a[3],
        info: a[4],
        success: a[2],
        danger: a[1],
        warning: mix(a[3], a[1], 0.4),
        cursor: s.cursor,
        selection: s.selection,
        ansi: a,
        background: None,
        file: None,
    }
}

/// OLED Black: Gentleman-Cute's rose on true black. Large surfaces are
/// `#000000` (pixels off), text is a soft pearl rather than pure white
/// (less glare and burn-in), and the accents are the brand's, unchanged.
pub const OLED_THEME: &str = "OLED Black";

fn oled_black() -> Theme {
    #[rustfmt::skip]
    let ansi = [
        0x1c171a, 0xff718f, 0xb4e7c7, 0xf2b86d, 0xa9c7ee, 0xf095c8, 0x9cd7d1, 0xd2cbd0,
        0x5c5058, 0xff8fa6, 0xc8f0d6, 0xf7cb8f, 0xc4daf6, 0xffb1dd, 0xb8e6e1, 0xf0e9ed,
    ];
    Theme {
        name: OLED_THEME.to_string(),
        source: Source::Builtin,
        light: false,
        oled: true,
        solid: false,
        bg: 0x000000,
        fg: 0xdcd4d9,
        muted: 0x958590,
        faint: 0x62555d,
        accent: 0xf095c8,
        accent_active: 0xffb1dd,
        raised: 0x1a0d15,
        border: 0x3a2230,
        heading: 0xe0c27a,
        info: 0xa9c7ee,
        success: 0xb4e7c7,
        danger: 0xff718f,
        warning: 0xf2b86d,
        cursor: 0xf095c8,
        selection: 0x28121e,
        ansi,
        background: None,
        file: None,
    }
}

/// OLED Black first, then the popular schemes.
pub fn builtin_themes() -> Vec<Theme> {
    std::iter::once(oled_black())
        .chain(SCHEMES.iter().map(from_scheme))
        .collect()
}

// ── Catalog and selection ──────────────────────────────────────────────────

fn embedded_gentle() -> Vec<Theme> {
    EMBEDDED
        .iter()
        .map(|text| {
            parse_pi_theme(text, Source::Gentle, None).expect("embedded Gentle theme parses")
        })
        .collect()
}

/// Themes with backdrops the app ships as examples (its `theme-examples`):
/// the user's or the agent's own app themes of the same name replace them.
const EXAMPLES: &[&str] = &[
    include_str!("../../themes/examples/aurora.json"),
    include_str!("../../themes/examples/petals.json"),
    include_str!("../../themes/examples/moonlit.json"),
    include_str!("../../themes/examples/koi-pond.json"),
    include_str!("../../themes/examples/sakura-dusk.json"),
    include_str!("../../themes/examples/neon-rain.json"),
    include_str!("../../themes/examples/jellyfish.json"),
    include_str!("../../themes/examples/lantern-night.json"),
    include_str!("../../themes/examples/fireflies.json"),
    include_str!("../../themes/examples/snow-cabin.json"),
    include_str!("../../themes/examples/star-voyage.json"),
    include_str!("../../themes/examples/honu-lagoon.json"),
    include_str!("../../themes/examples/cloud-drift.json"),
];

pub fn example_themes() -> Vec<Theme> {
    EXAMPLES
        .iter()
        .map(|text| parse_pi_theme(text, Source::App, None).expect("example theme parses"))
        .collect()
}

/// The example themes' files as shipped, for agents to read as references.
pub fn example_theme_texts() -> &'static [&'static str] {
    EXAMPLES
}

/// Gentle Shell's themes (installed files over the embedded copies), then the
/// user's Pi and app themes, then the example app themes and the popular
/// schemes. Names are unique, first wins.
pub fn load_catalog(gentle: Vec<Theme>, user: Vec<Theme>) -> Vec<Theme> {
    let mut catalog = embedded_gentle();
    for theme in gentle {
        match catalog.iter_mut().find(|t| same_name(&t.name, &theme.name)) {
            Some(slot) => *slot = theme,
            None => catalog.push(theme),
        }
    }
    for theme in user
        .into_iter()
        .chain(example_themes())
        .chain(builtin_themes())
    {
        if !catalog.iter().any(|t| same_name(&t.name, &theme.name)) {
            catalog.push(theme);
        }
    }
    catalog
}

/// The app's own themes folder: `<app data dir>/themes/` (`shared::data_dir`).
pub fn app_themes_dir() -> Option<PathBuf> {
    super::data_dir::app().map(|dir| dir.join("themes"))
}

/// The user's themes: Pi's folder, then the app's.
fn read_user_themes(agent: &Path) -> Vec<Theme> {
    let mut themes = read_theme_dir(&agent.join("themes"), Source::Pi);
    if let Some(dir) = app_themes_dir() {
        themes.extend(read_theme_dir(&dir, Source::App));
    }
    themes
}

/// A cheap fingerprint of a themes folder: each file with its size and
/// modification time, so a poll notices themes (and their scenes and
/// images) added, edited or removed. Folders (the app's `previews` and
/// `examples`) don't count: no theme lives in them.
fn dir_stamp(dir: &Path) -> Vec<(PathBuf, u64, Option<std::time::SystemTime>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stamp: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            Some((e.path(), meta.len(), meta.modified().ok()))
        })
        .collect();
    stamp.sort();
    stamp
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Persisted choice, then Pi's `settings.json` `theme`, then Gentleman-Cute;
/// a name the catalog doesn't have falls through to the next.
pub fn pick_default(catalog: &[Theme], persisted: Option<&str>, pi: Option<&str>) -> usize {
    [persisted, pi, Some(DEFAULT_THEME)]
        .into_iter()
        .flatten()
        .find_map(|name| catalog.iter().position(|t| same_name(&t.name, name)))
        .unwrap_or(0)
}

/// `<app data dir>/settings.json` (`shared::data_dir`).
pub fn store_path() -> Option<PathBuf> {
    super::data_dir::app().map(|dir| dir.join("settings.json"))
}

fn read_settings(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn read_theme_key(path: &Path) -> Option<String> {
    let value = read_settings(path)?;
    let name = value.get("theme")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The persisted choice in the app's settings file.
pub fn read_persisted(path: &Path) -> Option<String> {
    read_theme_key(path)
}

/// The persisted Appearance sliders. Each key is read on its own and
/// clamped to 0–1; a missing or unreadable one (a settings file from
/// before the slider existed) takes its default.
pub fn read_glass(path: &Path) -> Glass {
    let settings = read_settings(path);
    let key = |name: &str, default: f32| {
        settings
            .as_ref()
            .and_then(|s| s.get(name)?.as_f64())
            .map_or(default, |v| (v as f32).clamp(0., 1.))
    };
    let d = Glass::DEFAULT;
    Glass {
        transparency: key(Glass::TRANSPARENCY, d.transparency),
        blur: key(Glass::BLUR, d.blur),
        opacity: key(Glass::OPACITY, d.opacity),
    }
}

/// The persisted interface size in percent (`interfaceScale` holds the
/// factor, `1.25` for 125 %), rounded and kept in range.
pub fn read_interface_size(path: &Path) -> Option<u16> {
    interface_size::from_factor(read_key(path, "interfaceScale")?.as_f64()?)
}

/// Pi's `settings.json` `theme` in the agent dir.
pub fn read_pi_setting(agent: &Path) -> Option<String> {
    read_theme_key(&agent.join("settings.json"))
}

/// `key` of the app's settings file, if the file and the key exist.
pub(crate) fn read_key(path: &Path, key: &str) -> Option<Value> {
    read_settings(path)?.get(key).cloned()
}

/// The settings file's keys, to change some and write it back: none when the
/// file doesn't exist yet, an error when it exists but can't be read or isn't
/// a JSON object, so a save never replaces settings it couldn't read.
fn settings_to_update(path: &Path) -> std::io::Result<serde_json::Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(err) => return Err(err),
    };
    match serde_json::from_str(&text) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} isn't a JSON object", path.display()),
        )),
        Err(err) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: {err}", path.display()),
        )),
    }
}

/// Write the settings file through a temporary file and a rename, so a crash
/// mid-write leaves the old file whole.
fn write_settings(path: &Path, object: serde_json::Map<String, Value>) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let text =
        serde_json::to_string_pretty(&Value::Object(object)).map_err(std::io::Error::other)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let temp = dir.join(format!("settings.json.{}-{nanos}.tmp", std::process::id()));
    let written = std::fs::write(&temp, text + "\n").and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Set `key` in the app's settings file, keeping every other key.
pub(crate) fn write_key(path: &Path, key: &str, new: Value) -> std::io::Result<()> {
    write_keys(path, [(key, new)])
}

/// Set several keys in the app's settings file in one write, keeping every
/// other key.
fn write_keys<'a>(
    path: &Path,
    keys: impl IntoIterator<Item = (&'a str, Value)>,
) -> std::io::Result<()> {
    let mut object = settings_to_update(path)?;
    for (key, new) in keys {
        object.insert(key.to_string(), new);
    }
    write_settings(path, object)
}

/// Set `theme` in the app's settings file, keeping every other key.
pub fn write_persisted(path: &Path, name: &str) -> std::io::Result<()> {
    write_key(path, "theme", Value::String(name.to_string()))
}

/// Set the three Appearance sliders in the app's settings file, keeping
/// every other key.
pub fn write_glass(path: &Path, glass: Glass) -> std::io::Result<()> {
    // Two decimals: the sliders' step, and no float noise in the file.
    let rounded = |v: f32| Value::from((f64::from(v.clamp(0., 1.)) * 100.).round() / 100.);
    write_keys(
        path,
        [
            (Glass::TRANSPARENCY, rounded(glass.transparency)),
            (Glass::BLUR, rounded(glass.blur)),
            (Glass::OPACITY, rounded(glass.opacity)),
        ],
    )
}

/// Set `interfaceScale` (the factor) in the app's settings file, keeping
/// every other key.
pub fn write_interface_size(path: &Path, percent: u16) -> std::io::Result<()> {
    write_key(
        path,
        "interfaceScale",
        Value::from(interface_size::factor(percent)),
    )
}

/// A font the user can choose; each has its own `settings.json` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontSlot {
    /// Text of the interface.
    Interface,
    /// Monospace text of the interface: cards, editor, diffs.
    Code,
    /// Terminal panes; follows the code font when unset.
    Terminal,
}

impl FontSlot {
    pub const ALL: [FontSlot; 3] = [FontSlot::Interface, FontSlot::Code, FontSlot::Terminal];
    /// Whether the slot has a size of its own. The interface has none: its
    /// text follows Interface size (the zoom), which also sets the rem.
    pub fn has_size(self) -> bool {
        self != FontSlot::Interface
    }

    /// The key in the app's settings file.
    pub fn key(self) -> &'static str {
        match self {
            FontSlot::Interface => "interfaceFont",
            FontSlot::Code => "codeFont",
            FontSlot::Terminal => "terminalFont",
        }
    }

    /// The key of the slot's size in the app's settings file. The
    /// interface's is legacy: never read, and removed on the next write.
    pub fn size_key(self) -> &'static str {
        match self {
            FontSlot::Interface => "interfaceFontSize",
            FontSlot::Code => "codeFontSize",
            FontSlot::Terminal => "terminalFontSize",
        }
    }

    /// The size, in px at 100 % zoom, the app used before sizes were chosen;
    /// the interface's is the rem at 100 %, and never changes.
    pub fn default_size(self) -> f32 {
        match self {
            FontSlot::Interface => fonts::DEFAULT_INTERFACE_SIZE,
            FontSlot::Code => fonts::DEFAULT_CODE_SIZE,
            FontSlot::Terminal => fonts::DEFAULT_TERMINAL_SIZE,
        }
    }
}

/// The persisted family for `slot`, trimmed; absent, blank or not a string
/// is the default.
pub fn read_font(path: &Path, slot: FontSlot) -> Option<String> {
    match read_key(path, slot.key())? {
        Value::String(name) => fonts::normalize(&name),
        _ => None,
    }
}

/// The persisted families and sizes (families not yet checked against the
/// installed ones); invalid values are the defaults.
pub fn read_fonts(path: &Path) -> Fonts {
    let mut fonts = Fonts::default();
    for slot in FontSlot::ALL {
        fonts.set(slot, read_font(path, slot).map(SharedString::from));
        if !slot.has_size() {
            continue;
        }
        let size = read_key(path, slot.size_key())
            .and_then(|v| v.as_f64())
            .and_then(fonts::normalize_size)
            .unwrap_or_else(|| slot.default_size());
        fonts.set_size(slot, size);
    }
    fonts
}

/// Write every font family and size in one read-modify-write, keeping every
/// other key. Defaults remove their keys, and so does the interface's
/// legacy size (it has no size of its own).
pub fn write_fonts(path: &Path, fonts: &Fonts) -> std::io::Result<()> {
    let mut object = settings_to_update(path)?;
    for slot in FontSlot::ALL {
        match fonts.get(slot).and_then(|f| fonts::normalize(f)) {
            Some(name) => object.insert(slot.key().to_string(), Value::String(name)),
            None => object.remove(slot.key()),
        };
        match fonts
            .size_ref(slot)
            .and_then(|s| fonts::normalize_size(f64::from(s)))
        {
            Some(size) => object.insert(slot.size_key().to_string(), Value::from(size)),
            None => object.remove(slot.size_key()),
        };
    }
    write_settings(path, object)
}

// ── Active theme ───────────────────────────────────────────────────────────

/// The terminal's view of the active theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermColors {
    pub ansi: [u32; 16],
    pub fg: u32,
    pub bg: u32,
    pub dim_fg: u32,
    pub cursor: u32,
}

impl TermColors {
    pub fn new(theme: &Theme) -> Self {
        Self {
            ansi: theme.ansi,
            fg: theme.fg,
            bg: theme.bg,
            dim_fg: theme.muted,
            cursor: theme.cursor,
        }
    }
}

struct Resolved {
    palette: ui::Palette,
    term: TermColors,
    /// The theme's `solid` flag, for effects that follow the theme (the
    /// start banner tints itself in solid themes).
    solid: bool,
}

impl Resolved {
    fn new(theme: &Theme, glass: Glass) -> Self {
        Self {
            palette: ui::Palette::new(theme, glass),
            term: TermColors::new(theme),
            solid: theme.solid,
        }
    }
}

/// Window transparency until the user moves the slider: clear enough for the
/// desktop blur to show through every pane, as in Tern. Linux compositors
/// rarely blur behind a window (GPUI's X11 backend never does, Wayland only
/// with a blur protocol), so the desktop would read through sharp: there the
/// glass starts nearly solid.
pub const DEFAULT_TRANSPARENCY: f32 = if cfg!(target_os = "linux") { 0.15 } else { 0.6 };

/// The Appearance sliders, each 0–1 (shown as 0–100 %).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glass {
    /// How much shows through the glass: the chrome (title bar, the gaps
    /// between panes) follows it fully; text areas only down to the
    /// clearest they can be and still meet AA (`ui::Palette::new`).
    pub transparency: f32,
    /// How soft the theme's own background picture is (`backdrop`): 0 is
    /// sharp. Themes without a picture or scene have nothing to blur.
    pub blur: f32,
    /// How opaque the areas that always hold text are, from the clearest
    /// that still meets AA (0) to opaque (1).
    pub opacity: f32,
}

impl Glass {
    pub const TRANSPARENCY: &str = "transparency";
    pub const BLUR: &str = "blur";
    pub const OPACITY: &str = "opacity";

    /// Out of the box: AA holds at any Opacity (the floor sees to it), so
    /// the default only adds a margin over the bare minimum; the theme's
    /// picture stays as its author drew it.
    pub const DEFAULT: Self = Self {
        transparency: DEFAULT_TRANSPARENCY,
        blur: 0.,
        opacity: 0.2,
    };
}

/// The resolved active theme; Gentleman-Cute until [`init`] runs.
static ACTIVE: LazyLock<RwLock<Resolved>> = LazyLock::new(|| {
    let cute = embedded_gentle()
        .into_iter()
        .find(|t| t.name == DEFAULT_THEME)
        .expect("Gentleman-Cute is embedded");
    RwLock::new(Resolved::new(&cute, Glass::DEFAULT))
});

/// Read the active UI palette.
pub fn palette<R>(read: impl FnOnce(&ui::Palette) -> R) -> R {
    let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    read(&active.palette)
}

/// Read the active terminal colors.
pub fn term<R>(read: impl FnOnce(&TermColors) -> R) -> R {
    let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    read(&active.term)
}

/// Whether the active theme renders solid, non-glass surfaces (`"solid":
/// true` in its file).
pub fn solid() -> bool {
    let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    active.solid
}

fn install(theme: &Theme, glass: Glass) {
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = Resolved::new(theme, glass);
}

/// The catalog, the active theme, the Appearance sliders and the interface
/// size, for pickers.
pub struct Themes {
    pub catalog: Vec<Theme>,
    pub active: usize,
    pub glass: Glass,
    /// The interface size in percent (`gentle_core::interface_size`).
    pub interface_size: u16,
    /// The fonts in use: only families that are installed.
    pub fonts: Fonts,
}

/// The effective font choices; `None` is the default of each surface.
/// Family and size are independent per slot: only the terminal *family*
/// follows the code font when unset, never its size.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Fonts {
    pub interface: Option<SharedString>,
    pub code: Option<SharedString>,
    pub terminal: Option<SharedString>,
    /// Sizes in px at 100 % zoom; `None` is the slot's default. The
    /// interface has no size of its own (see [`FontSlot::has_size`]).
    pub code_size: Option<f32>,
    pub terminal_size: Option<f32>,
}

impl Fonts {
    pub fn get(&self, slot: FontSlot) -> Option<&SharedString> {
        match slot {
            FontSlot::Interface => self.interface.as_ref(),
            FontSlot::Code => self.code.as_ref(),
            FontSlot::Terminal => self.terminal.as_ref(),
        }
    }

    /// Choose the family of `slot`; `None` is the default.
    pub fn set(&mut self, slot: FontSlot, family: Option<SharedString>) {
        *self.slot_mut(slot) = family;
    }

    /// The terminal family: its own, else the code font's.
    pub fn terminal_family(&self) -> Option<&SharedString> {
        self.terminal.as_ref().or(self.code.as_ref())
    }

    /// The size of `slot` in px at 100 % zoom (its default when unset).
    pub fn size(&self, slot: FontSlot) -> f32 {
        self.size_ref(slot).unwrap_or_else(|| slot.default_size())
    }

    /// Choose the size of `slot`. Out-of-range sizes clamp to the limits;
    /// NaN and the slot's default size both mean the default. The interface
    /// has no size of its own, so it ignores this.
    pub fn set_size(&mut self, slot: FontSlot, size: f32) {
        let size = fonts::clamp_size(f64::from(size)).filter(|s| *s != slot.default_size());
        if let Some(stored) = self.size_mut(slot) {
            *stored = size;
        }
    }

    fn size_ref(&self, slot: FontSlot) -> Option<f32> {
        match slot {
            FontSlot::Interface => None,
            FontSlot::Code => self.code_size,
            FontSlot::Terminal => self.terminal_size,
        }
    }

    fn size_mut(&mut self, slot: FontSlot) -> Option<&mut Option<f32>> {
        match slot {
            FontSlot::Interface => None,
            FontSlot::Code => Some(&mut self.code_size),
            FontSlot::Terminal => Some(&mut self.terminal_size),
        }
    }

    fn slot_mut(&mut self, slot: FontSlot) -> &mut Option<SharedString> {
        match slot {
            FontSlot::Interface => &mut self.interface,
            FontSlot::Code => &mut self.code,
            FontSlot::Terminal => &mut self.terminal,
        }
    }
}

impl Global for Themes {}

impl Themes {
    pub fn active_theme(&self) -> &Theme {
        &self.catalog[self.active]
    }
}

/// Load the catalog, pick the starting theme and apply it. The installed
/// `gentle-pi` themes are located off the main thread (finding the launcher
/// can take a login shell) and merged in when found.
pub fn init(cx: &mut App) {
    let agent = pi_rpc::resolve_homes().agent;
    let store = store_path();
    let persisted = store.as_deref().and_then(read_persisted);
    let glass = store.as_deref().map_or(Glass::DEFAULT, read_glass);
    let interface_size = store
        .as_deref()
        .and_then(read_interface_size)
        .unwrap_or(interface_size::DEFAULT);
    let mut chosen = store.as_deref().map(read_fonts).unwrap_or_default();
    let mut names = None;
    for slot in FontSlot::ALL {
        if let Some(stored) = chosen.get(slot).cloned() {
            let names = names.get_or_insert_with(|| cx.text_system().all_font_names());
            let found =
                fonts::installed(&stored, names).map(|name| SharedString::from(name.to_string()));
            chosen.set(slot, found);
        }
    }
    let pi = read_pi_setting(&agent);
    let catalog = load_catalog(Vec::new(), read_user_themes(&agent));
    let active = pick_default(&catalog, persisted.as_deref(), pi.as_deref());
    install(&catalog[active], glass);
    cx.set_global(Themes {
        catalog,
        active,
        glass,
        interface_size,
        fonts: chosen,
    });
    ui::apply_theme(cx);
    backdrop::init(cx);

    cx.spawn(async move |cx| {
        let gentle = cx
            .background_executor()
            .spawn(async move {
                let cwd = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                pi_rpc::resolve_backend(&cwd)
                    .ok()
                    .and_then(|spec| pi_rpc::gentle_pi_root(&spec))
                    .map(|root| read_theme_dir(&root.join("themes"), Source::Gentle))
                    .unwrap_or_default()
            })
            .await;
        if gentle.is_empty() {
            return;
        }
        cx.update(|cx| merge_gentle(gentle, persisted, pi, cx));
    })
    .detach();
    watch_user_themes(agent, cx);
}

/// How often the user's theme folders are checked for new or edited themes
/// (an agent writing one, say).
const THEME_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Re-read Pi's and the app's theme folders whenever their files change, so
/// a theme shows up in the picker (and an edited active theme applies)
/// without a restart, and keep the previews of the app's themes current
/// (`backdrop::preview`), so the agent that wrote one can look at it.
fn watch_user_themes(agent: PathBuf, cx: &mut App) {
    let app_dir = app_themes_dir();
    let dirs: Vec<PathBuf> = std::iter::once(agent.join("themes"))
        .chain(app_dir.clone())
        .collect();
    cx.spawn(async move |cx| {
        fn stamp(dirs: &[PathBuf]) -> Vec<Vec<(PathBuf, u64, Option<std::time::SystemTime>)>> {
            dirs.iter().map(|d| dir_stamp(d)).collect()
        }
        // Previews are drawn off the UI thread, after the themes apply.
        let preview = |user: Vec<Theme>, mut drawn: HashMap<String, Theme>| {
            let dir = app_dir.clone();
            async move {
                if let Some(dir) = dir {
                    backdrop::preview::refresh(&user, &dir, &mut drawn);
                }
                drawn
            }
        };
        let agent_now = agent.clone();
        let user = cx
            .background_executor()
            .spawn(async move { read_user_themes(&agent_now) })
            .await;
        let mut drawn = cx
            .background_executor()
            .spawn(preview(user, HashMap::new()))
            .await;
        let mut last = stamp(&dirs);
        loop {
            cx.background_executor().timer(THEME_POLL).await;
            crate::shared::diagnostics::tick();
            let (dirs_now, agent_now) = (dirs.clone(), agent.clone());
            let last_seen = last.clone();
            // Only a change re-reads (and re-parses) the themes.
            let changed = cx
                .background_executor()
                .spawn(async move {
                    let now = stamp(&dirs_now);
                    (now != last_seen).then(|| (now, read_user_themes(&agent_now)))
                })
                .await;
            let Some((now, user)) = changed else {
                continue;
            };
            last = now;
            cx.update(|cx| replace_user_themes(user.clone(), cx));
            drawn = cx.background_executor().spawn(preview(user, drawn)).await;
        }
    })
    .detach();
}

/// Swap the user's themes in, keeping the active one by name (re-applying
/// it when its file changed, or falling back to the default when it's gone).
fn replace_user_themes(user: Vec<Theme>, cx: &mut App) {
    let themes = cx.global::<Themes>();
    let before = themes.active_theme().clone();
    let (glass, interface_size) = (themes.glass, themes.interface_size);
    let chosen = themes.fonts.clone();
    let gentle: Vec<Theme> = themes
        .catalog
        .iter()
        .filter(|t| t.source == Source::Gentle)
        .cloned()
        .collect();
    let catalog = load_catalog(gentle, user);
    let active = catalog
        .iter()
        .position(|t| same_name(&t.name, &before.name))
        .unwrap_or_else(|| pick_default(&catalog, None, None));
    let changed = catalog[active] != before;
    if changed {
        install(&catalog[active], glass);
    }
    cx.set_global(Themes {
        catalog,
        active,
        glass,
        interface_size,
        fonts: chosen,
    });
    if changed {
        ui::apply_theme(cx);
    }
    cx.refresh_windows();
}

/// Add a shared theme (a `.gentletheme` file's text) to the app's themes
/// folder and switch to it. Its name, or why it couldn't be added.
pub fn install_shared(text: &str, cx: &mut App) -> Result<String, String> {
    let theme = parse_pi_theme(text, Source::App, None)
        .ok_or("That isn't a theme file: it needs at least a \"name\".")?;
    let dir = app_themes_dir().ok_or("There's no data folder to keep themes in.")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!(
        "{}.{}",
        backdrop::model::file_stem(&theme.name),
        backdrop::SHARE_EXTENSION
    ));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    replace_user_themes(read_user_themes(&pi_rpc::resolve_homes().agent), cx);
    let index = cx
        .global::<Themes>()
        .catalog
        .iter()
        .position(|t| t.file.as_deref() == Some(path.as_path()));
    if let Some(index) = index {
        select(index, cx);
    }
    Ok(theme.name)
}

/// `theme` as one file to share (`<stem>.gentletheme`): its file with the
/// scene and image put inside. App themes only; `None` for the others.
pub fn share_text(theme: &Theme) -> Option<Result<(String, String), String>> {
    if theme.source != Source::App {
        return None;
    }
    let stem = backdrop::model::file_stem(&theme.name);
    let text = match &theme.file {
        Some(file) => {
            let text = std::fs::read_to_string(file).map_err(|e| e.to_string());
            let dir = file.parent().unwrap_or(Path::new("."));
            text.and_then(|text| backdrop::model::bundle(&text, dir))
        }
        // An example: its scene is built in, so the file is already whole.
        None => EXAMPLES
            .iter()
            .find(|text| {
                parse_pi_theme(text, Source::App, None).is_some_and(|t| t.name == theme.name)
            })
            .map(|text| text.to_string())
            .ok_or_else(|| "This theme has no file.".to_string()),
    };
    Some(text.map(|text| (stem, text)))
}

/// Swap the installed Gentle themes in, re-applying when the active one changed.
fn merge_gentle(gentle: Vec<Theme>, persisted: Option<String>, pi: Option<String>, cx: &mut App) {
    let themes = cx.global::<Themes>();
    let before = themes.active_theme().clone();
    let (glass, interface_size) = (themes.glass, themes.interface_size);
    let chosen = themes.fonts.clone();
    let explicit =
        themes.active != pick_default(&themes.catalog, persisted.as_deref(), pi.as_deref());
    let user: Vec<Theme> = themes
        .catalog
        .iter()
        .filter(|t| matches!(t.source, Source::Pi | Source::App))
        .cloned()
        .collect();
    let catalog = load_catalog(gentle, user);
    // A choice made this session stays; otherwise re-resolve, since a
    // persisted name may only exist in the installed files.
    let active = if explicit {
        catalog
            .iter()
            .position(|t| same_name(&t.name, &before.name))
            .unwrap_or(0)
    } else {
        pick_default(&catalog, persisted.as_deref(), pi.as_deref())
    };
    let changed = catalog[active] != before;
    if changed {
        install(&catalog[active], glass);
    }
    cx.set_global(Themes {
        catalog,
        active,
        glass,
        interface_size,
        fonts: chosen,
    });
    if changed {
        ui::apply_theme(cx);
    }
    cx.refresh_windows();
}

/// Apply the catalog theme at `index` everywhere and persist the choice.
pub fn select(index: usize, cx: &mut App) {
    let themes = cx.global_mut::<Themes>();
    let Some(theme) = themes.catalog.get(index).cloned() else {
        return;
    };
    themes.active = index;
    let glass = themes.glass;
    install(&theme, glass);
    ui::apply_theme(cx);
    cx.refresh_windows();
    if let Some(path) = store_path() {
        let name = theme.name.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = write_persisted(&path, &name);
            })
            .detach();
    }
}

/// Apply the Appearance sliders to every window (re-tinting the glass and
/// re-blurring the theme's picture); `persist` writes them to the settings
/// file (the sliders persist on release, not on every drag).
pub fn set_glass(glass: Glass, persist: bool, cx: &mut App) {
    let glass = Glass {
        transparency: glass.transparency.clamp(0., 1.),
        blur: glass.blur.clamp(0., 1.),
        opacity: glass.opacity.clamp(0., 1.),
    };
    let themes = cx.global_mut::<Themes>();
    let before = themes.glass;
    themes.glass = glass;
    // Blur only changes the backdrop, not the palette.
    if (before.transparency, before.opacity) != (glass.transparency, glass.opacity) {
        let theme = themes.active_theme().clone();
        install(&theme, glass);
        ui::apply_theme(cx);
    }
    cx.refresh_windows();
    if persist && let Some(path) = store_path() {
        cx.background_executor()
            .spawn(async move {
                let _ = write_glass(&path, glass);
            })
            .detach();
    }
}

/// Resize the interface in every window to `percent` (kept in range) and
/// persist it: `ui::apply_theme` turns it into the rem size.
pub fn set_interface_size(percent: u16, cx: &mut App) {
    let percent = interface_size::clamp(percent);
    let themes = cx.global_mut::<Themes>();
    if themes.interface_size == percent {
        return;
    }
    themes.interface_size = percent;
    ui::apply_theme(cx);
    cx.refresh_windows();
    if let Some(path) = store_path() {
        cx.background_executor()
            .spawn(async move {
                let _ = write_interface_size(&path, percent);
            })
            .detach();
    }
}

/// Serial of the latest `apply_fonts`; a queued write that is no longer the
/// latest is skipped, since the newer one carries the complete state.
static FONTS_WRITE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FONTS_WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Apply `fonts` to the whole app and persist them: one global refresh and
/// one grouped write, however many slots changed. A no-op when nothing
/// changed. The caller passes only installed families.
pub fn apply_fonts(fonts: Fonts, cx: &mut App) {
    use std::sync::atomic::Ordering;
    let themes = cx.global_mut::<Themes>();
    if themes.fonts == fonts {
        return;
    }
    themes.fonts = fonts.clone();
    ui::apply_theme(cx);
    cx.refresh_windows();
    if let Some(path) = store_path() {
        let serial = FONTS_WRITE.fetch_add(1, Ordering::SeqCst) + 1;
        cx.background_executor()
            .spawn(async move {
                let _guard = FONTS_WRITING.lock().unwrap_or_else(|e| e.into_inner());
                if FONTS_WRITE.load(Ordering::SeqCst) == serial {
                    let _ = write_fonts(&path, &fonts);
                }
            })
            .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "gentle-themes-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_gentle_theme_json() {
        let cute = parse_pi_theme(EMBEDDED[0], Source::Gentle, None).unwrap();
        assert_eq!(cute.name, "Gentleman-Cute");
        assert!(!cute.light);
        assert!(!cute.solid);
        assert_eq!(cute.bg, 0x060407);
        assert_eq!(cute.fg, 0xf6eff3);
        assert_eq!(cute.muted, 0xa78e9b);
        assert_eq!(cute.faint, 0x76616b);
        assert_eq!(cute.accent, 0xf095c8);
        assert_eq!(cute.accent_active, 0xffb1dd);
        assert_eq!(cute.raised, 0x2a1523);
        assert_eq!(cute.border, 0x563040);
        assert_eq!(cute.heading, 0xe0c27a);
        assert_eq!(cute.info, 0xa9c7ee);
        assert_eq!(cute.success, 0xb4e7c7);
        assert_eq!(cute.danger, 0xff718f);
        assert_eq!(cute.warning, 0xf2b86d);
        assert_eq!(cute.ansi[1], 0xff718f);

        // Gentle: accent is a var reference; ANSI vars feed the info color.
        let gentle = parse_pi_theme(EMBEDDED[2], Source::Gentle, None).unwrap();
        assert_eq!(gentle.name, "Gentle");
        assert_eq!(gentle.accent, 0x7fb4ca);
        assert_eq!(gentle.info, 0x7fb4ca);
        assert_eq!(gentle.bg, 0x06080f);
    }

    #[test]
    fn parses_the_studio_themes() {
        let studio = parse_pi_theme(EMBEDDED[3], Source::Gentle, None).unwrap();
        assert_eq!(studio.name, "Studio");
        assert!(studio.solid);
        assert!(!studio.light);
        assert_eq!(studio.bg, 0x1b1b1d);
        assert_eq!(studio.fg, 0xececec);
        assert_eq!(studio.muted, 0xa1a1a6);
        assert_eq!(studio.faint, 0x6e6e73);
        assert_eq!(studio.accent, 0x7c9cbf);
        assert_eq!(studio.accent_active, 0x9db7d5);
        assert_eq!(studio.raised, 0x2a2a2e);
        assert_eq!(studio.border, 0x3a3a3f);
        assert_eq!(studio.heading, 0xe6e1d6);
        assert_eq!(studio.success, 0x7fb08a);
        assert_eq!(studio.warning, 0xd6b46a);
        assert_eq!(studio.danger, 0xe07a74);
        assert_eq!(studio.selection, 0x34404d);
        assert_eq!(studio.info, 0x9db7d5);

        let light = parse_pi_theme(EMBEDDED[4], Source::Gentle, None).unwrap();
        assert_eq!(light.name, "Studio Light");
        assert!(light.solid);
        assert!(light.light);
        assert_eq!(light.bg, 0xf7f6f3);
        assert_eq!(light.fg, 0x1f1f1f);
        assert_eq!(light.muted, 0x5f5f63);
        assert_eq!(light.faint, 0x8e8e93);
        assert_eq!(light.accent, 0x3f6a9a);
        assert_eq!(light.accent_active, 0x2f5580);
        assert_eq!(light.raised, 0xebe9e4);
        assert_eq!(light.border, 0xd6d3cc);
        assert_eq!(light.heading, 0x2b2b2b);
        assert_eq!(light.success, 0x3f7d4f);
        assert_eq!(light.warning, 0x9a6b12);
        assert_eq!(light.danger, 0xb3423a);
        assert_eq!(light.selection, 0xcdd9e6);
        assert_eq!(light.info, 0x3f6a9a);

        // Both show under "Gentle Shell" exactly once, under their own names.
        let catalog = load_catalog(Vec::new(), Vec::new());
        for name in ["Studio", "Studio Light"] {
            assert_eq!(
                catalog.iter().filter(|t| t.name == name).count(),
                1,
                "{name} should appear once"
            );
        }
        assert_eq!(
            parse_pi_theme(EMBEDDED[3], Source::Gentle, None)
                .unwrap()
                .ansi,
            gentle_ansi("Studio").unwrap()
        );
        assert_eq!(
            parse_pi_theme(EMBEDDED[4], Source::Gentle, None)
                .unwrap()
                .ansi,
            gentle_ansi("Studio Light").unwrap()
        );
    }

    #[test]
    fn parses_the_clay_themes() {
        let clay = parse_pi_theme(EMBEDDED[5], Source::Gentle, None).unwrap();
        assert_eq!(clay.name, "Clay");
        assert!(clay.solid);
        assert!(!clay.light);
        assert_eq!(clay.bg, 0x262624);
        assert_eq!(clay.fg, 0xece9e1);
        assert_eq!(clay.muted, 0xa8a49b);
        assert_eq!(clay.faint, 0x75726b);
        assert_eq!(clay.accent, 0xd4825f);
        assert_eq!(clay.accent_active, 0xe39a79);
        assert_eq!(clay.raised, 0x30302e);
        assert_eq!(clay.border, 0x3e3d39);
        assert_eq!(clay.heading, 0xf0ebe0);
        assert_eq!(clay.success, 0x8fb37f);
        assert_eq!(clay.warning, 0xd9b26a);
        assert_eq!(clay.danger, 0xe0786b);
        assert_eq!(clay.selection, 0x4a3a31);
        assert_eq!(clay.info, 0xe39a79);

        let light = parse_pi_theme(EMBEDDED[6], Source::Gentle, None).unwrap();
        assert_eq!(light.name, "Clay Light");
        assert!(light.solid);
        assert!(light.light);
        assert_eq!(light.bg, 0xf6f3ec);
        assert_eq!(light.fg, 0x2b2a27);
        assert_eq!(light.muted, 0x6b675f);
        assert_eq!(light.faint, 0x9a958b);
        assert_eq!(light.accent, 0xa95334);
        assert_eq!(light.accent_active, 0x9c4a2e);
        assert_eq!(light.raised, 0xece7dc);
        assert_eq!(light.border, 0xdcd5c7);
        assert_eq!(light.heading, 0x23221f);
        assert_eq!(light.success, 0x4f7a43);
        assert_eq!(light.warning, 0x94661a);
        assert_eq!(light.danger, 0xb0413a);
        assert_eq!(light.selection, 0xefd9cc);
        assert_eq!(light.info, 0x9c4a2e);

        // Both show under "Gentle Shell" exactly once, under their own names.
        let catalog = load_catalog(Vec::new(), Vec::new());
        for name in ["Clay", "Clay Light"] {
            assert_eq!(
                catalog.iter().filter(|t| t.name == name).count(),
                1,
                "{name} should appear once"
            );
        }
        assert_eq!(
            parse_pi_theme(EMBEDDED[5], Source::Gentle, None)
                .unwrap()
                .ansi,
            gentle_ansi("Clay").unwrap()
        );
        assert_eq!(
            parse_pi_theme(EMBEDDED[6], Source::Gentle, None)
                .unwrap()
                .ansi,
            gentle_ansi("Clay Light").unwrap()
        );
    }

    #[test]
    fn parses_indices_hex_and_ansi_vars() {
        let text = r##"{
            "name": "Mine",
            "vars": { "base": "#ffffff", "red": "#aa0000", "brightRed": 196, "ink": "base" },
            "colors": { "text": 16, "accent": "#0af", "muted": "", "error": "red" },
            "export": { "pageBg": "ink" }
        }"##;
        let theme = parse_pi_theme(text, Source::Pi, None).unwrap();
        assert!(theme.light);
        assert_eq!(theme.bg, 0xffffff);
        assert_eq!(theme.fg, 0x000000);
        assert_eq!(theme.accent, 0x00aaff);
        assert_eq!(theme.danger, 0xaa0000);
        assert_eq!(theme.ansi[1], 0xaa0000);
        assert_eq!(theme.ansi[9], 0xff0000);
        assert!(parse_pi_theme("{\"vars\":{}}", Source::Pi, None).is_none());
        assert!(parse_pi_theme("not json", Source::Pi, None).is_none());
    }

    #[test]
    fn default_selection_precedence() {
        let catalog = load_catalog(Vec::new(), Vec::new());
        let at = |name: &str| catalog.iter().position(|t| t.name == name).unwrap();
        assert_eq!(
            pick_default(&catalog, Some("Nord"), Some("Dracula")),
            at("Nord")
        );
        assert_eq!(pick_default(&catalog, None, Some("dracula")), at("Dracula"));
        assert_eq!(pick_default(&catalog, None, None), at("Gentleman-Cute"));
    }

    #[test]
    fn unknown_names_fall_back() {
        let catalog = load_catalog(Vec::new(), Vec::new());
        let at = |name: &str| catalog.iter().position(|t| t.name == name).unwrap();
        assert_eq!(
            pick_default(&catalog, Some("Gone"), Some("Dracula")),
            at("Dracula")
        );
        assert_eq!(
            pick_default(&catalog, Some("Gone"), Some("dark")),
            at("Gentleman-Cute")
        );
    }

    #[test]
    fn catalog_merges_without_duplicates() {
        let mut installed = parse_pi_theme(EMBEDDED[0], Source::Gentle, None).unwrap();
        installed.accent = 0x123456;
        let mut user = from_scheme(&SCHEMES[0]);
        user.source = Source::Pi;
        user.name = "dracula".into();
        let catalog = load_catalog(vec![installed], vec![user]);
        assert_eq!(catalog[0].accent, 0x123456);
        assert_eq!(
            catalog
                .iter()
                .filter(|t| same_name(&t.name, "Dracula"))
                .count(),
            1
        );
        assert_eq!(
            catalog
                .iter()
                .find(|t| same_name(&t.name, "Dracula"))
                .unwrap()
                .source,
            Source::Pi
        );
        // OLED Black, 17 dark + 2 light, minus the Dracula the user's file shadows.
        assert_eq!(
            catalog
                .iter()
                .filter(|t| t.source == Source::Builtin)
                .count(),
            19
        );
        assert!(catalog.iter().filter(|t| t.light).count() >= 2);
    }

    #[test]
    fn persisting_keeps_other_keys() {
        let dir = scratch("persist");
        let path = dir.join("nested").join("settings.json");
        write_persisted(&path, "Nord").unwrap();
        assert_eq!(read_persisted(&path).as_deref(), Some("Nord"));
        std::fs::write(&path, r#"{"keep": 1, "theme": "Nord"}"#).unwrap();
        write_persisted(&path, "Dracula").unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["keep"], 1);
        assert_eq!(value["theme"], "Dracula");
        // Theme and the Appearance sliders share the file without clobbering
        // each other.
        let glass = Glass {
            transparency: 0.456_7,
            blur: 0.3,
            opacity: 0.8,
        };
        write_glass(&path, glass).unwrap();
        write_persisted(&path, "Nord").unwrap();
        assert_eq!(read_persisted(&path).as_deref(), Some("Nord"));
        let read = read_glass(&path);
        assert_eq!(
            (read.transparency, read.blur, read.opacity),
            (0.46, 0.3, 0.8)
        );
        // Out-of-range values clamp; a file from before Blur and Opacity
        // existed keeps its transparency and takes their defaults.
        std::fs::write(&path, r#"{"transparency": 7}"#).unwrap();
        let read = read_glass(&path);
        assert_eq!(read.transparency, 1.);
        assert_eq!(
            (read.blur, read.opacity),
            (Glass::DEFAULT.blur, Glass::DEFAULT.opacity)
        );
        assert_eq!(read_glass(&dir.join("missing.json")), Glass::DEFAULT);
        // The interface size shares it too, stored as a factor.
        assert_eq!(read_interface_size(&path), None);
        write_interface_size(&path, 125).unwrap();
        assert_eq!(read_glass(&path).transparency, 1.);
        assert_eq!(read_interface_size(&path), Some(125));
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["interfaceScale"], 1.25);
        std::fs::write(&path, r#"{"interfaceScale": "big"}"#).unwrap();
        assert_eq!(read_interface_size(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A settings file that doesn't parse (a hand edit with a trailing comma)
    /// is left as it is: the save fails instead of replacing every setting
    /// with the one being written.
    #[test]
    fn saving_a_setting_leaves_a_settings_file_that_does_not_parse() {
        let dir = scratch("unparseable");
        let path = dir.join("settings.json");
        let before = "{\n  \"theme\": \"Clay\",\n  \"onboarding\": {\"done\": true},\n}\n";
        std::fs::write(&path, before).unwrap();

        let glass = Glass {
            transparency: 0.5,
            blur: 0.5,
            opacity: 0.5,
        };
        assert!(write_glass(&path, glass).is_err());
        assert!(write_fonts(&path, &Fonts::default()).is_err());

        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Valid JSON that isn't an object is left as it is too.
    #[test]
    fn saving_a_setting_leaves_a_settings_file_that_is_not_an_object() {
        let dir = scratch("not-object");
        let path = dir.join("settings.json");
        std::fs::write(&path, "[\"theme\"]\n").unwrap();

        assert!(write_persisted(&path, "Nord").is_err());

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[\"theme\"]\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A save leaves no temporary file next to the settings.
    #[test]
    fn saving_a_setting_leaves_only_the_settings_file() {
        let dir = scratch("only-settings");
        let path = dir.join("settings.json");
        write_persisted(&path, "Nord").unwrap();
        write_fonts(&path, &Fonts::default()).unwrap();

        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["settings.json"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fonts_roundtrip_through_one_grouped_write_keeping_other_keys() {
        let dir = scratch("fonts-group");
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"theme": "cute", "codeFontSize": 99}"#).unwrap();
        let mut fonts = Fonts::default();
        fonts.set(FontSlot::Code, Some("Fira Code".into()));
        fonts.set_size(FontSlot::Code, 15.);
        fonts.set_size(FontSlot::Terminal, 11.5);
        write_fonts(&path, &fonts).unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["theme"], "cute");
        assert_eq!(value["codeFont"], "Fira Code");
        assert_eq!(value["codeFontSize"], 15.0);
        assert_eq!(value["terminalFontSize"], 11.5);
        assert!(value.get("interfaceFontSize").is_none());
        assert!(value.get("interfaceFont").is_none());
        let back = read_fonts(&path);
        assert_eq!(back.size(FontSlot::Code), 15.);
        assert_eq!(back.size(FontSlot::Terminal), 11.5);
        assert_eq!(
            back.get(FontSlot::Code).map(|s| s.as_ref()),
            Some("Fira Code")
        );
        // Back to the defaults removes the keys again.
        write_fonts(&path, &Fonts::default()).unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!({"theme": "cute"}));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_interface_has_no_size_of_its_own_and_the_legacy_key_goes_away() {
        let dir = scratch("fonts-interface-size");
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"keep": 1, "interfaceFontSize": 20}"#).unwrap();
        let mut fonts = read_fonts(&path);
        assert_eq!(fonts.size(FontSlot::Interface), 16.);
        fonts.set_size(FontSlot::Interface, 20.);
        assert_eq!(fonts.size(FontSlot::Interface), 16.);
        assert_eq!(fonts, Fonts::default());
        write_fonts(&path, &fonts).unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!({"keep": 1}));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_persisted_sizes_fall_back_to_the_defaults() {
        let dir = scratch("fonts-bad-size");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"interfaceFontSize": "big", "codeFontSize": -4, "terminalFontSize": 500}"#,
        )
        .unwrap();
        let fonts = read_fonts(&path);
        assert_eq!(fonts.size(FontSlot::Interface), 16.);
        assert_eq!(fonts.size(FontSlot::Code), 13.);
        assert_eq!(fonts.size(FontSlot::Terminal), 13.);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sizes_are_independent_and_only_the_family_is_inherited() {
        let mut fonts = Fonts::default();
        fonts.set_size(FontSlot::Code, 20.);
        assert_eq!(fonts.size(FontSlot::Interface), 16.);
        assert_eq!(fonts.size(FontSlot::Terminal), 13.);
        fonts.set(FontSlot::Code, Some("Fira Code".into()));
        assert_eq!(
            fonts.terminal_family(),
            Some(&SharedString::from("Fira Code"))
        );
        assert_eq!(fonts.size(FontSlot::Terminal), 13.);
        fonts.set(FontSlot::Terminal, Some("Menlo".into()));
        assert_eq!(fonts.terminal_family(), Some(&SharedString::from("Menlo")));
        // Clamping and NaN.
        fonts.set_size(FontSlot::Code, 400.);
        assert_eq!(fonts.size(FontSlot::Code), 32.);
        fonts.set_size(FontSlot::Code, f32::NAN);
        assert_eq!(fonts.size(FontSlot::Code), 13.);
        // Equal to the default is the same as unset (so drafts compare equal).
        fonts.set_size(FontSlot::Code, 13.);
        assert_eq!(fonts.code_size, Fonts::default().code_size);
    }

    #[test]
    fn fonts_persist_per_slot_and_default_removes_the_key() {
        let dir = scratch("fonts");
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"keep": 1, "interfaceScale": 1.25}"#).unwrap();
        for slot in FontSlot::ALL {
            assert_eq!(read_font(&path, slot), None);
        }
        let mut fonts = Fonts::default();
        fonts.set(
            FontSlot::Terminal,
            Some("  JetBrainsMono Nerd Font ".into()),
        );
        fonts.set(FontSlot::Code, Some("Fira Code".into()));
        write_fonts(&path, &fonts).unwrap();
        assert_eq!(
            read_font(&path, FontSlot::Terminal).as_deref(),
            Some("JetBrainsMono Nerd Font")
        );
        assert_eq!(
            read_font(&path, FontSlot::Code).as_deref(),
            Some("Fira Code")
        );
        assert_eq!(read_font(&path, FontSlot::Interface), None);
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["keep"], 1);
        assert_eq!(value["interfaceScale"], 1.25);
        assert_eq!(value["terminalFont"], "JetBrainsMono Nerd Font");
        fonts.set(FontSlot::Code, None);
        fonts.set(FontSlot::Terminal, Some("   ".into()));
        write_fonts(&path, &fonts).unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(value.get("codeFont").is_none());
        assert!(value.get("terminalFont").is_none());
        assert_eq!(value["keep"], 1);
        std::fs::write(&path, r#"{"interfaceFont": 7, "codeFont": "  "}"#).unwrap();
        assert_eq!(read_font(&path, FontSlot::Interface), None);
        assert_eq!(read_font(&path, FontSlot::Code), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_user_theme_dir_and_pi_setting() {
        let dir = scratch("dir");
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::write(
            dir.join("themes").join("b.json"),
            r##"{"name":"Zeta","colors":{"text":"#eeeeee"}}"##,
        )
        .unwrap();
        std::fs::write(
            dir.join("themes").join("a.json"),
            r##"{"name":"Alpha","colors":{"text":"#eeeeee"}}"##,
        )
        .unwrap();
        std::fs::write(dir.join("themes").join("bad.json"), "{").unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"theme":"Zeta"}"#).unwrap();
        let themes = read_theme_dir(&dir.join("themes"), Source::Pi);
        assert_eq!(
            themes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Alpha", "Zeta"]
        );
        assert_eq!(read_pi_setting(&dir).as_deref(), Some("Zeta"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mix_blends_channels() {
        assert_eq!(mix(0x000000, 0xffffff, 0.5), 0x808080);
        assert_eq!(mix(0x102030, 0x102030, 0.7), 0x102030);
        assert_eq!(mix(0xff0000, 0x0000ff, 0.0), 0xff0000);
    }

    #[test]
    fn app_themes_read_their_backdrop_and_pi_themes_ignore_it() {
        let text = r##"{
            "name": "Petals",
            "vars": {"pink": "#ffc9dd", "bg": "#120a10"},
            "colors": {"accent": "#f4a6c6", "text": "#f7edf2"},
            "background": {"scene": "petals", "sky": ["pink", "#fff", "nope", 7]}
        }"##;
        let theme = parse_pi_theme(text, Source::App, None).unwrap();
        let backdrop = theme.background.unwrap();
        assert_eq!(backdrop.scene.unwrap().name, "petals");
        // Vars and hex resolve, an unknown name is skipped, an index is xterm's.
        assert_eq!(backdrop.sky, [0xffc9dd, 0xffffff, xterm256(7)]);

        // The same file in Pi's folder: Pi's schema has no `background`.
        assert_eq!(
            parse_pi_theme(text, Source::Pi, None).unwrap().background,
            None
        );
    }

    #[test]
    fn app_themes_may_use_claude_desktop_themes_colors() {
        let text = r##"{"id": "koi", "name": "Koi", "bg": "#11332d", "text": "#e4f2ea",
            "accent": "#ef8a4a", "wallpaper": "koi-pond"}"##;
        let theme = parse_pi_theme(text, Source::App, None).unwrap();
        assert_eq!(
            (theme.bg, theme.fg, theme.accent),
            (0x11332d, 0xe4f2ea, 0xef8a4a)
        );
        assert!(!theme.light);
        // An unknown wallpaper is no scene; a built-in one is found by name.
        assert_eq!(theme.background.unwrap().scene.unwrap().name, "koi-pond");
        // Pi themes keep Pi's format only.
        assert_ne!(parse_pi_theme(text, Source::Pi, None).unwrap().bg, 0x11332d);
    }

    #[test]
    fn example_themes_ship_with_scenes_and_yield_to_user_ones() {
        let examples = example_themes();
        assert!(examples.len() >= 10);
        assert!(
            examples
                .iter()
                .all(|t| t.background.as_ref().is_some_and(|b| b.scene.is_some()))
        );
        // None hides behind (or hides) a popular scheme's name.
        for example in &examples {
            assert!(
                !builtin_themes()
                    .iter()
                    .any(|t| same_name(&t.name, &example.name))
            );
        }

        // A user's app theme with an example's name replaces it.
        let mut mine = examples[0].clone();
        mine.background = None;
        let catalog = load_catalog(Vec::new(), vec![mine]);
        let found: Vec<_> = catalog
            .iter()
            .filter(|t| same_name(&t.name, &examples[0].name))
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].background, None);
    }
}
