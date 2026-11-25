//! The in-app browser: a card that embeds the system's web view (WKWebView
//! on macOS, WebView2 on Windows, WebKitGTK on Linux), floating over the
//! window or docked in a column at its right. It shows Gentle Design's live
//! canvas and any other http(s) page, opened with `gentle-open`, `/web` or a
//! loopback URL printed by a tool. Pages an agent opens wait in the window's
//! instance tray (or open in the user's own browser when Configuration ›
//! Devices › Browser says so, see [`external`]); the native view (and its web
//! process) only exists while the card is shown. A lone floating card sits
//! at the bottom-right by default; multiple ones are tiled by the workspace.
//! Drag its header to move it, any edge or corner to resize it, or maximize
//! it; its dock button puts it in the column (the column's divider sets the
//! width), and later cards open the way the last one was left
//! ([`CardLayout`]). The canvas reloads itself as artboards change, so the
//! card shows the design in real time.
//!
//! The web view is a native view above everything GPUI draws, so it hides
//! while a screen covers the window. It stays up while the card is dragged:
//! AppKit keeps sending the drag to the view the mouse went down in.

use std::rc::Rc;

use gentle_core::browser::{CardLayout, LAYOUT_KEY, Profile};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::shared::floating::{self, CardState, DragKind, Mode};
use crate::shared::ui;

pub mod automation;
pub mod cli;
pub mod external;
mod surface;
pub use surface::WebSurface;

/// The `browser` tool every agent loads (`gentle-browser.ts`), written into
/// the app's extensions folder with this executable's path in it.
pub fn install_extension() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let source = include_str!("gentle-browser.ts").replace(
        "__GENTLE_APP__",
        &serde_json::Value::from(exe.to_string_lossy()).to_string(),
    );
    let path = crate::shared::data_dir::app()?
        .join("extensions")
        .join("gentle-browser.ts");
    crate::features::genui::write_if_changed(&path, &source)?;
    Some(path)
}

/// What the profile chooser shows (in place of the page).
enum Choosing {
    List,
    /// New profile…: its name field.
    Naming {
        input: Entity<InputState>,
        _enter: Subscription,
    },
    /// Clear website data…: the confirmation.
    Clearing,
}

/// The scheme the app serves its own isolated pages from (generative UI's
/// HTML), so they get an origin of their own and nothing from the web.
const SANDBOX_SCHEME: &str = "gentleui";

/// Answers a [`Sandbox`]'s requests by path; `None` is a 404. Called on the
/// web view's threads.
pub type ServePage = dyn Fn(&str) -> Option<SandboxPage> + Send + Sync;

/// A page the app serves itself, cut off from the network, the files and
/// the app: every request outside its own scheme is refused unless its
/// policy allows it, no popups, downloads or device permissions, no stored
/// data, no devtools in release builds. Its scripts reach the app only
/// through `window.ipc.postMessage`.
#[derive(Clone)]
pub struct Sandbox {
    pub serve: Arc<ServePage>,
    /// Every `window.ipc.postMessage` string, in order.
    pub ipc: async_channel::Sender<String>,
    /// Other URLs a frame may navigate to (the page's declared frame
    /// origins).
    pub frames: Rc<dyn Fn(&str) -> bool>,
}

/// One response of a [`Sandbox`]: an HTML document and its
/// Content-Security-Policy.
pub struct SandboxPage {
    pub html: String,
    pub csp: String,
}

/// `path` on the sandbox's origin. WebView2 maps custom schemes to
/// `http://<scheme>.localhost`.
pub fn sandbox_url(path: &str) -> String {
    if cfg!(windows) {
        format!("http://{SANDBOX_SCHEME}.localhost/{path}")
    } else {
        format!("{SANDBOX_SCHEME}://localhost/{path}")
    }
}

/// Where a page's requests for a new window (`target="_blank"`,
/// `window.open`) go: the card loads them itself.
type Popups = async_channel::Sender<String>;

/// `builder` handing new-window requests to `popups`, when given, instead
/// of opening a window (a sandbox's own rule, set after, still denies them).
fn follow_popups<'a>(
    builder: wry::WebViewBuilder<'a>,
    popups: Option<Popups>,
) -> wry::WebViewBuilder<'a> {
    let Some(popups) = popups else {
        return builder;
    };
    builder.with_new_window_req_handler(move |url, _| {
        if url.starts_with("http://") || url.starts_with("https://") {
            let _ = popups.try_send(url);
        }
        wry::NewWindowResponse::Deny
    })
}

/// `builder` locked down for `sandbox` (see [`Sandbox`]).
fn isolate<'a>(builder: wry::WebViewBuilder<'a>, sandbox: &Sandbox) -> wry::WebViewBuilder<'a> {
    let serve = sandbox.serve.clone();
    let ipc = sandbox.ipc.clone();
    let frames = sandbox.frames.clone();
    let origin = sandbox_url("");
    builder
        .with_custom_protocol(SANDBOX_SCHEME.into(), move |_, request| {
            let path = request.uri().path().trim_start_matches('/');
            let response = wry::http::Response::builder().header("Cache-Control", "no-store");
            match serve(path) {
                Some(page) => response
                    .header("Content-Type", "text/html; charset=utf-8")
                    .header("Content-Security-Policy", page.csp)
                    .header("X-Content-Type-Options", "nosniff")
                    .body(page.html.into_bytes().into()),
                None => response.status(404).body(Vec::new().into()),
            }
            .unwrap_or_else(|_| wry::http::Response::new(Vec::new().into()))
        })
        .with_ipc_handler(move |request| {
            let _ = ipc.try_send(request.into_body());
        })
        .with_navigation_handler(move |url| {
            url.starts_with(&origin)
                || url == "about:blank"
                || url == "about:srcdoc"
                || frames(&url)
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false)
        .with_permission_handler(|_| wry::PermissionResponse::Deny)
        .with_incognito(true)
        .with_devtools(cfg!(debug_assertions))
}

/// A mouse-down reached GPUI rather than a page: give GPUI the keyboard
/// back if a page had taken it (see `macos::give_keyboard_to`).
pub fn reclaim_keyboard(window: &Window) {
    native::reclaim_keyboard(window);
}

/// The window's own popovers and menus that are open (the bell, the music
/// player, the instance tray, title-bar menus), by id. Native web views draw
/// above everything GPUI draws, so they step aside while any is open.
#[derive(Default)]
struct OpenPopovers(std::collections::HashSet<&'static str>);

impl Global for OpenPopovers {}

/// Popover `id` opened or closed: pass it as the popover's `on_open_change`.
pub fn popover_open(id: &'static str, open: bool, window: &mut Window, cx: &mut App) {
    let popovers = &mut cx.default_global::<OpenPopovers>().0;
    let changed = if open {
        popovers.insert(id)
    } else {
        popovers.remove(id)
    };
    if changed {
        window.refresh();
    }
}

/// A popover registered with [`popover_open`] is open.
pub fn popovers_open(cx: &App) -> bool {
    cx.try_global::<OpenPopovers>()
        .is_some_and(|popovers| !popovers.0.is_empty())
}

// The native web view, one module per system with the same interface:
// `host` (the window to build in, taken during a frame), `NativeView::{new,
// place, load_url, reload}`, plus `pump` for a toolkit loop GPUI doesn't run
// (`NEEDS_PUMP`).
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod native;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod native;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod native;

/// How often GTK's loop is pumped while a native view exists (Linux): about
/// a display frame while the app is in front, and only enough to keep pages
/// alive while it is in the background.
const PUMP_EVERY: Duration = Duration::from_millis(16);
const PUMP_IDLE: Duration = Duration::from_millis(100);

/// `bounds` (window coordinates) as `wry` takes them.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn logical_rect(bounds: Bounds<Pixels>) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(
            bounds.origin.x.as_f32() as f64,
            bounds.origin.y.as_f32() as f64,
        )
        .into(),
        size: wry::dpi::LogicalSize::new(
            bounds.size.width.as_f32().max(1.) as f64,
            bounds.size.height.as_f32().max(1.) as f64,
        )
        .into(),
    }
}

/// The page's background before it paints: the card body's color.
fn page_background() -> (u8, u8, u8, u8) {
    let well = Rgba::from(ui::well());
    let byte = |c: f32| (c.clamp(0., 1.) * 255.) as u8;
    (byte(well.r), byte(well.g), byte(well.b), 255)
}

/// Called once with a page's picture as PNG bytes (on the main thread).
type Shot = Box<dyn FnOnce(Result<Vec<u8>, String>) + Send>;

/// Any picture a web view hands over (TIFF on macOS), as PNG bytes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn png_from(bytes: &[u8]) -> Result<Vec<u8>, String> {
    png_from_image(image::load_from_memory(bytes).map_err(|error| error.to_string())?)
}

/// Straight RGBA pixels as PNG bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn png_from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>, String> {
    let image = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "the picture's size doesn't match its pixels".to_string())?;
    png_from_image(image::DynamicImage::ImageRgba8(image))
}

#[cfg_attr(windows, allow(dead_code))]
fn png_from_image(image: image::DynamicImage) -> Result<Vec<u8>, String> {
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(out.into_inner())
}

/// Where a profile keeps its website data on Windows and Linux
/// (`<data dir>/browser-profiles/<folder>`); macOS names WebKit data stores
/// by [`Profile::store_id`] instead.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn profile_dir(profile: &Profile) -> Option<std::path::PathBuf> {
    Some(
        crate::shared::data_dir::app()?
            .join("browser-profiles")
            .join(profile.folder()),
    )
}

/// What Gentle Shell reported about its design canvas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanvasNotice {
    /// The canvas shows `url`; `refreshed` when it was already open.
    Open {
        url: String,
        refreshed: bool,
    },
    /// A new `open` of the URL already shown. Gentle Shell sends `open` only
    /// while no page shows the canvas (a connected page gets `refresh`), so a
    /// browser tab outside the app was closed and is opened again; the card
    /// inside the app still holds the page and is left as it is.
    Reopen {
        url: String,
    },
    Stopped,
}

/// The canvas notice in a `notify` message or a `gentle_design` tool result
/// text: "Design canvas: <url>", "Design canvas refreshed: <url>" or "Design
/// canvas stopped.". Only the canvas server's loopback URLs count.
pub fn canvas_notice(text: &str) -> Option<CanvasNotice> {
    if text.trim_start().starts_with("Design canvas stopped.") {
        return Some(CanvasNotice::Stopped);
    }
    for (prefix, refreshed) in [
        ("Design canvas refreshed: ", true),
        ("Design canvas: ", false),
    ] {
        let Some(at) = text.find(prefix) else {
            continue;
        };
        let url = text[at + prefix.len()..]
            .split_whitespace()
            .next()
            .unwrap_or_default();
        if is_loopback(url) {
            return Some(CanvasNotice::Open {
                url: url.to_string(),
                refreshed,
            });
        }
    }
    None
}

fn is_loopback(url: &str) -> bool {
    ["http://127.0.0.1:", "http://localhost:"]
        .iter()
        .any(|prefix| url.starts_with(prefix) && url.len() > prefix.len())
}

/// The design a canvas URL shows (`/<token>/d/<slug>/`), for the title.
fn design_slug(url: &str) -> Option<&str> {
    let (_, path) = url.split_once("/d/")?;
    path.split('/').next().filter(|slug| !slug.is_empty())
}

/// A generic web page's title: the host, without the scheme or path.
fn host_label(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if host.is_empty() {
        "Web".to_string()
    } else {
        host.to_string()
    }
}

/// The card's place when it opens: bottom-right, at the size the user last
/// gave a floating card. Tiling scales it.
pub fn opening_frame(cx: &App) -> floating::Frame {
    let layout = card_layout(cx);
    floating::Frame {
        right: 16.,
        bottom: 16.,
        w: layout.width,
        h: layout.height,
    }
}

// ── How cards open ──────────────────────────────────────────────────────────

struct Layout(CardLayout);

impl Global for Layout {}

/// Docked or floating, the docked column's width and a floating card's size,
/// as the user last left them.
pub fn card_layout(cx: &App) -> CardLayout {
    cx.try_global::<Layout>()
        .map(|layout| layout.0)
        .unwrap_or_default()
}

/// Remember how cards open; `save` writes it to `settings.json` too (a
/// divider drag saves once, when it ends).
pub fn set_card_layout(layout: CardLayout, save: bool, cx: &mut App) {
    cx.set_global(Layout(layout));
    if save
        && let Some(path) = crate::shared::themes::store_path()
        && let Err(err) = crate::shared::themes::write_key(&path, LAYOUT_KEY, layout.to_json())
    {
        eprintln!("gentle-shell-gpui: could not save the browser card's layout: {err}");
    }
}

// ── The panel ───────────────────────────────────────────────────────────────

pub enum BrowserEvent {
    /// The card's button: send it to the instance tray.
    Hide,
    /// Docked or floated: the window lays its cards out again.
    Layout,
}

// ── Profiles the user named ──────────────────────────────────────────────────

/// The `settings.json` key listing the profiles made with New profile.
const PROFILES_KEY: &str = "browserProfiles";

struct NamedProfiles(Vec<String>);

impl Global for NamedProfiles {}

/// Read the profiles the user made, how cards open, and where pages outside
/// the app open.
pub fn init(cx: &mut App) {
    external::init(cx);
    let layout = crate::shared::themes::store_path()
        .and_then(|path| crate::shared::themes::read_key(&path, LAYOUT_KEY))
        .map(|value| CardLayout::from_json(&value))
        .unwrap_or_default();
    cx.set_global(Layout(layout));
    let names = crate::shared::themes::store_path()
        .and_then(|path| crate::shared::themes::read_key(&path, PROFILES_KEY))
        .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
        .unwrap_or_default();
    cx.set_global(NamedProfiles(names));
}

fn named_profiles(cx: &App) -> Vec<String> {
    cx.try_global::<NamedProfiles>()
        .map(|p| p.0.clone())
        .unwrap_or_default()
}

/// Remember a profile the user named, for every card's menu.
fn add_named_profile(name: &str, cx: &mut App) {
    let mut names = named_profiles(cx);
    if names.iter().any(|n| n == name) {
        return;
    }
    names.push(name.to_string());
    if let Some(path) = crate::shared::themes::store_path()
        && let Err(err) =
            crate::shared::themes::write_key(&path, PROFILES_KEY, serde_json::json!(names))
    {
        eprintln!("gentle-shell-gpui: could not save the browser profiles: {err}");
    }
    cx.set_global(NamedProfiles(names));
}

pub struct BrowserPanel {
    url: String,
    /// What the header calls this page. `None` derives it from the URL (the
    /// host), so any web app can use the card, not only Gentle Design.
    label: Option<String>,
    /// The name an agent opened it with (`gentle-browser open --name`).
    name: Option<String>,
    /// An agent drives it: the header says so, and the page keeps running
    /// out of sight until the user takes it back.
    driven: bool,
    /// The workspace's folder, offered as the Repository profile.
    repository: Option<String>,
    /// The profile chooser over the page, while it is open.
    choosing: Option<Choosing>,
    /// A screen covers the window (see [`Self::set_covered`]).
    screen_covered: bool,
    /// Docked in the window's right column, or floating over the window.
    mode: Mode,
    card: CardState,
    /// The page itself.
    surface: Entity<WebSurface>,
}

impl EventEmitter<BrowserEvent> for BrowserPanel {}

impl BrowserPanel {
    pub fn new(url: String, label: Option<String>, cx: &mut Context<Self>) -> Self {
        let surface = cx.new(|cx| {
            let mut surface = WebSurface::new(url.clone(), None);
            surface.follow_popups(cx);
            surface
        });
        Self {
            url,
            label,
            name: None,
            driven: false,
            repository: None,
            choosing: None,
            screen_covered: false,
            mode: if card_layout(cx).docked {
                Mode::Docked
            } else {
                Mode::Floating
            },
            card: CardState::new(opening_frame(cx)),
            surface,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Dock the card or float it; later cards open the same way.
    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        if self.card.maximized() {
            self.card.toggle_maximized();
        }
        let layout = CardLayout {
            docked: mode == Mode::Docked,
            ..card_layout(cx)
        };
        set_card_layout(layout, true, cx);
        cx.emit(BrowserEvent::Layout);
        cx.notify();
    }

    pub fn surface(&self) -> &Entity<WebSurface> {
        &self.surface
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn set_name(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.name = name;
        cx.notify();
    }

    pub fn set_repository(&mut self, repository: Option<String>) {
        self.repository = repository;
    }

    pub fn is_driven(&self) -> bool {
        self.driven
    }

    /// An agent starts or stops driving the page.
    pub fn set_driven(&mut self, driven: bool, cx: &mut Context<Self>) {
        if self.driven != driven {
            self.driven = driven;
            self.surface
                .update(cx, |surface, cx| surface.set_live(driven, cx));
            cx.notify();
        }
    }

    pub fn profile(&self, cx: &App) -> Profile {
        self.surface.read(cx).profile().clone()
    }

    pub fn set_profile(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.surface
            .update(cx, |surface, cx| surface.set_profile(profile, cx));
        cx.notify();
    }

    /// Follow the page's own navigation (an agent's link), for the header.
    pub fn set_url(&mut self, url: String, cx: &mut Context<Self>) {
        if self.url != url {
            self.url = url;
            cx.notify();
        }
    }

    /// Put the card in a stage slot; see [`crate::shared::floating::CardState`].
    pub fn set_frame(&mut self, frame: floating::Frame, cx: &mut Context<Self>) {
        self.card.set_frame(frame);
        cx.notify();
    }

    /// Whether the card is showing Gentle Design's canvas (so a canvas
    /// `Stopped` notice may close it) rather than a generic web app.
    pub fn is_design(&self) -> bool {
        self.label.is_some()
    }

    /// Show `url`, or reload when it is already showing (the canvas was
    /// refreshed).
    pub fn navigate(&mut self, url: String, label: Option<String>, cx: &mut Context<Self>) {
        self.surface
            .update(cx, |surface, cx| surface.navigate(url.clone(), cx));
        self.url = url;
        self.label = label;
        cx.notify();
    }

    /// A screen opened or closed over the window.
    pub fn set_covered(&mut self, covered: bool, cx: &mut Context<Self>) {
        self.screen_covered = covered;
        self.cover(cx);
    }

    /// The page's native view hides under a screen and under the profile
    /// chooser (it would draw over both).
    fn cover(&mut self, cx: &mut Context<Self>) {
        let covered = self.screen_covered || self.choosing.is_some();
        self.surface
            .update(cx, |surface, cx| surface.set_covered(covered, cx));
    }

    fn choose(&mut self, choosing: Option<Choosing>, cx: &mut Context<Self>) {
        self.choosing = choosing;
        self.cover(cx);
        cx.notify();
    }

    /// The profiles the chooser offers, current one included.
    fn profiles(&self, cx: &App) -> Vec<Profile> {
        let mut profiles = vec![Profile::Shared];
        if let Some(root) = &self.repository {
            profiles.push(Profile::Repository(root.clone()));
        }
        profiles.push(Profile::Agent);
        profiles.push(Profile::Private);
        profiles.extend(named_profiles(cx).into_iter().map(Profile::Named));
        let current = self.profile(cx);
        if !profiles.contains(&current) {
            profiles.push(current);
        }
        profiles
    }

    fn pick_profile(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.set_profile(profile, cx);
        self.choose(None, cx);
    }

    fn start_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Profile name"));
        input.update(cx, |state, cx| state.focus(window, cx));
        let subscription = cx.subscribe_in(&input, window, |this, input, event, _, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let name = input.read(cx).value().trim().to_string();
                if !name.is_empty() {
                    add_named_profile(&name, cx);
                    this.pick_profile(Profile::Named(name), cx);
                }
            }
        });
        self.choose(
            Some(Choosing::Naming {
                input,
                _enter: subscription,
            }),
            cx,
        );
    }

    fn clear_data(&mut self, cx: &mut Context<Self>) {
        self.surface
            .update(cx, |surface, cx| surface.clear_data(cx));
        self.choose(None, cx);
    }

    /// The card went to the instance tray: drop the native view, and the web
    /// process behind it, so a hidden page costs nothing. Showing it again
    /// builds a fresh view on [`Self::url`].
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.surface.update(cx, |surface, cx| surface.hide(cx));
    }

    pub fn show(&mut self, cx: &mut Context<Self>) {
        self.surface.update(cx, |surface, cx| surface.show(cx));
    }

    /// What the header and the tray call this page.
    pub fn title(&self) -> String {
        match (&self.label, design_slug(&self.url)) {
            (Some(_), Some(slug)) => format!("Design · {slug}"),
            (Some(label), None) => label.clone(),
            (None, _) => host_label(&self.url),
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.surface.update(cx, |surface, cx| surface.reload(cx));
    }

    fn toggle_maximized(&mut self, cx: &mut Context<Self>) {
        self.card.toggle_maximized();
        cx.notify();
    }

    fn start_drag(&mut self, kind: DragKind, start: Point<Pixels>, cx: &mut Context<Self>) {
        self.card.start(kind, start);
        cx.notify();
    }

    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.card.drag_to(position) {
            cx.notify();
        }
    }

    /// A drag ended; a resize makes that the size later cards open at.
    fn end_drag(&mut self, cx: &mut Context<Self>) {
        let resized = self.card.resizing();
        if self.card.end_drag() {
            if resized {
                let (width, height) = self.card.size();
                let layout = CardLayout {
                    width,
                    height,
                    ..card_layout(cx)
                };
                set_card_layout(layout, true, cx);
            }
            cx.notify();
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.title();
        let icon = if self.label.is_some() {
            IconName::Palette
        } else {
            IconName::Globe
        };
        let address = self
            .url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .to_string();
        let url = self.url.clone();
        let maximized = self.card.maximized();
        let docked = self.mode == Mode::Docked;
        let profile_label = self.profile(cx).label();
        h_flex()
            .id("browser-header")
            .h(rems(floating::HEADER / 16.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            // A docked card stays in its column: no moving, no maximizing.
            .when(!docked, |header| {
                header.cursor(CursorStyle::OpenHand).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        if event.click_count == 2 {
                            this.toggle_maximized(cx);
                        } else {
                            this.start_drag(DragKind::Move, event.position, cx);
                        }
                    }),
                )
            })
            .child(Icon::new(icon).small().text_color(ui::petal()))
            .child(
                // In a narrow card the title gives way before the buttons.
                div()
                    .min_w_0()
                    .text_size(rems(12.5 / 16.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ui::fg())
                    .truncate()
                    .child(title),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(10.5))
                    .text_color(ui::faint())
                    .child(address),
            )
            .child(
                // Buttons keep their mouse-down from starting a drag.
                h_flex()
                    .flex_none()
                    .gap_0p5()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when(self.driven, |row| {
                        row.child(
                            Button::new("browser-take-over")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::Bot).text_color(ui::accent()))
                                .label("Take over")
                                .tooltip("An agent is driving this page · take it back")
                                .on_click(cx.listener(|this, _, _, cx| this.set_driven(false, cx))),
                        )
                    })
                    .child(
                        Button::new("browser-profile")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::User))
                            .label(profile_label.0)
                            .tooltip("Where this page keeps its cookies and sign-ins")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let next = if this.choosing.is_some() {
                                    None
                                } else {
                                    Some(Choosing::List)
                                };
                                this.choose(next, cx);
                            })),
                    )
                    .child(
                        Button::new("browser-reload")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::RotateCw))
                            .tooltip("Reload")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .child(
                        Button::new("browser-external")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::ArrowUpRight))
                            .tooltip("Open in your browser")
                            .on_click(move |_, _, cx| external::open(&url, cx)),
                    )
                    .child(
                        Button::new("browser-dock")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(if docked {
                                IconName::PictureInPicture2
                            } else {
                                IconName::PanelRight
                            }))
                            .tooltip(if docked { "Float" } else { "Dock to the right" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_mode(
                                    if docked { Mode::Floating } else { Mode::Docked },
                                    cx,
                                )
                            })),
                    )
                    .when(!docked, |row| {
                        row.child(
                            Button::new("browser-maximize")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(if maximized {
                                    IconName::Minimize2
                                } else {
                                    IconName::Maximize2
                                }))
                                .tooltip(if maximized { "Restore" } else { "Maximize" })
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_maximized(cx))),
                        )
                    })
                    .child(
                        Button::new("browser-close")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::Minus))
                            .tooltip("Hide · it stays in the top bar")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(BrowserEvent::Hide))),
                    ),
            )
    }

    fn body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .mx(px(floating::GRIP))
            .mb(px(floating::GRIP))
            .rounded(px(floating::BODY_RADIUS))
            .bg(ui::well())
            .overflow_hidden()
            .map(|body| match &self.choosing {
                Some(choosing) => body.child(self.chooser(choosing, cx)),
                None => body.child(self.surface.clone()),
            })
    }

    /// The profile chooser, drawn in place of the page (its native view
    /// would cover a menu).
    fn chooser(&self, choosing: &Choosing, cx: &mut Context<Self>) -> AnyElement {
        let note = |text: &'static str| {
            div()
                .text_size(ui::code_rems(11.))
                .text_color(ui::faint())
                .child(text)
        };
        let pane = v_flex().size_full().p_3().gap_1().items_start();
        match choosing {
            Choosing::List => {
                let current = self.profile(cx);
                let rows = self
                    .profiles(cx)
                    .into_iter()
                    .enumerate()
                    .map(|(ix, profile)| {
                        let (name, kind) = profile.label();
                        let selected = profile == current;
                        Button::new(("browser-profile-pick", ix))
                            .ghost()
                            .small()
                            .icon(Icon::new(if selected {
                                IconName::Check
                            } else {
                                IconName::User
                            }))
                            .label(if kind.is_empty() {
                                name
                            } else {
                                format!("{name} · {kind}")
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pick_profile(profile.clone(), cx)
                            }))
                    });
                pane.child(note(
                    "Pages with the same profile share cookies and sign-ins. Agents open theirs in Agent.",
                ))
                .children(rows)
                .child(
                    Button::new("browser-profile-new")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::Plus))
                        .label("New profile…")
                        .on_click(cx.listener(|this, _, window, cx| this.start_naming(window, cx))),
                )
                .child(
                    Button::new("browser-profile-clear")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::Trash))
                        .label("Clear website data…")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.choose(Some(Choosing::Clearing), cx)
                        })),
                )
                .into_any_element()
            }
            Choosing::Naming { input, .. } => pane
                .child(note("Name the new profile; press Return to use it."))
                .child(Input::new(input).small())
                .child(
                    h_flex().pt_1().child(
                        Button::new("browser-profile-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.choose(Some(Choosing::List), cx)),
                            ),
                    ),
                )
                .into_any_element(),
            Choosing::Clearing => {
                let (name, _) = self.profile(cx).label();
                pane.child(
                    div()
                        .text_sm()
                        .text_color(ui::fg())
                        .child(format!("Clear website data in {name}?")),
                )
                .child(note(
                    "Sites using it sign you out and forget their settings. Pages using it reload.",
                ))
                .child(
                    h_flex()
                        .pt_1()
                        .gap_1()
                        .child(
                            Button::new("browser-clear-ok")
                                .danger()
                                .small()
                                .label("Clear")
                                .on_click(cx.listener(|this, _, _, cx| this.clear_data(cx))),
                        )
                        .child(
                            Button::new("browser-clear-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.choose(Some(Choosing::List), cx)
                                })),
                        ),
                )
                .into_any_element()
            }
        }
    }

    /// The bands along the card's edges and corners that resize it.
    fn grips(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        floating::grips(&self.card, cx, "browser-grip", |this, kind, start, cx| {
            this.start_drag(kind, start, cx)
        })
    }
}

impl Render for BrowserPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.mode == Mode::Docked {
            // The window puts it in its right column and sizes it; the
            // column's divider changes the width.
            return v_flex()
                .size_full()
                .rounded_xl()
                .bg(ui::panel())
                .overflow_hidden()
                .child(self.header(cx))
                .child(self.body(cx))
                .into_any_element();
        }
        self.card.fit();
        let card = ui::island(cx)
            .absolute()
            .flex()
            .flex_col()
            .occlude()
            .shadow(floating::shadow(self.card.dragging()))
            .map(|card| floating::position(&self.card, card))
            .child(self.header(cx))
            .child(self.body(cx))
            .children(self.grips(cx));
        let drag_layer = floating::drag_layer(
            &self.card,
            cx,
            "browser-drag",
            |this, position, cx| this.drag_to(position, cx),
            |this, cx| this.end_drag(cx),
        );
        div()
            .absolute()
            .inset_0()
            .child(floating::area_canvas(&self.card))
            .child(card)
            .when_some(drag_layer, |root, layer| root.child(layer))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{CanvasNotice, canvas_notice, design_slug, host_label};

    #[test]
    fn canvas_notices_come_from_gentle_design_messages() {
        let open = canvas_notice("Design canvas: http://127.0.0.1:51234/tok/d/login/");
        assert_eq!(
            open,
            Some(CanvasNotice::Open {
                url: "http://127.0.0.1:51234/tok/d/login/".into(),
                refreshed: false
            })
        );
        // The browser-failed warning still carries the URL on its first line.
        let warned = canvas_notice(
            "Design canvas: http://localhost:4000/tok/\nThe browser could not be opened. Open the URL manually.",
        );
        assert_eq!(
            warned,
            Some(CanvasNotice::Open {
                url: "http://localhost:4000/tok/".into(),
                refreshed: false
            })
        );
        assert_eq!(
            canvas_notice("Design canvas refreshed: http://127.0.0.1:1/t/d/x/"),
            Some(CanvasNotice::Open {
                url: "http://127.0.0.1:1/t/d/x/".into(),
                refreshed: true
            })
        );
        assert_eq!(
            canvas_notice("Design canvas stopped."),
            Some(CanvasNotice::Stopped)
        );
        // Anything else, or a URL that isn't the loopback canvas, is ignored.
        assert_eq!(canvas_notice("No design canvas is running."), None);
        assert_eq!(canvas_notice("Design canvas: https://example.com/"), None);
        assert_eq!(canvas_notice("Design canvas: http://127.0.0.1:"), None);
        assert_eq!(
            design_slug("http://127.0.0.1:1/tok/d/login/"),
            Some("login")
        );
        assert_eq!(design_slug("http://127.0.0.1:1/tok/"), None);
        assert_eq!(host_label("http://localhost:3000/app"), "localhost:3000");
        assert_eq!(host_label("https://example.com/"), "example.com");
    }
}
