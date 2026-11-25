//! Where the pages agents open go, and which web browser opens a page
//! outside the app: the two choices in Configuration › Devices › Browser,
//! saved in the app's `settings.json` ([`PAGES_KEY`], [`BROWSER_KEY`]), and
//! the catalog of browsers the app looks for, where each one installs per
//! OS and the command that opens a URL in it. Looking on disk and starting
//! the browser is the app's job (`platform/browsers.rs`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ides::{Base, Lookup, Os, Spot, app_lookups};

/// Key of the app's `settings.json` for [`AgentPages`].
pub const PAGES_KEY: &str = "agentPages";
/// Key of the app's `settings.json` for the chosen browser's [`WebBrowser::id`];
/// missing or unknown means the system's default browser.
pub const BROWSER_KEY: &str = "externalBrowser";

/// Where a page an agent opens (`gentle-open`, a dev server's URL, the
/// design canvas) goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentPages {
    /// The app's own browser card, waiting in the instance tray (what the
    /// app has always done).
    #[default]
    InApp,
    /// A browser outside the app: the chosen one, or the system's default.
    Browser,
}

impl AgentPages {
    pub const ALL: [AgentPages; 2] = [AgentPages::InApp, AgentPages::Browser];

    /// What the settings file calls it.
    pub fn id(self) -> &'static str {
        match self {
            AgentPages::InApp => "app",
            AgentPages::Browser => "browser",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentPages::InApp => "Inside the app",
            AgentPages::Browser => "In my browser",
        }
    }

    /// The saved value; anything else is the default.
    pub fn from_json(value: &Value) -> AgentPages {
        match value.as_str() {
            Some("browser") => AgentPages::Browser,
            _ => AgentPages::InApp,
        }
    }

    /// The choice from the settings file's [`PAGES_KEY`], when it has one:
    /// without it (never chosen) pages stay in the app.
    pub fn from_setting(saved: Option<&Value>) -> AgentPages {
        saved.map_or(AgentPages::InApp, AgentPages::from_json)
    }

    /// Whether pages agents open go to a browser outside the app.
    pub fn outside(self) -> bool {
        self == AgentPages::Browser
    }

    pub fn to_json(self) -> Value {
        Value::from(self.id())
    }
}

/// Gentle Shell's switch for its design canvas (gentle-shell#3517): `0`
/// keeps `gentle_design open` from launching the system browser, while the
/// canvas still starts and its `design_canvas` `open` event still carries
/// the URL. Shells without the switch ignore it and open the browser.
pub const DESIGN_OPEN_BROWSER_ENV: &str = "GENTLE_DESIGN_OPEN_BROWSER";

/// What an agent tab's Gentle Shell starts with about the design canvas:
/// [`DESIGN_OPEN_BROWSER_ENV`]`=0` whenever the app shows the canvas itself
/// from its URL: in its card while agents' pages show inside the app, or
/// in the chosen browser while they go there (the app opens it once), so
/// Gentle Shell's own system-browser tab would be a second one. Nothing only
/// where the app would show nothing: pages inside the app on Linux under
/// Wayland (`wayland_display` set, which GPUI then picks), which has no
/// child windows, so the card can't draw and the canvas stays in the browser
/// Gentle Shell opens. Read when the tab's backend starts.
pub fn design_launch_env(
    pages: AgentPages,
    os: Os,
    wayland_display: Option<&std::ffi::OsStr>,
) -> Option<(String, String)> {
    let wayland = os == Os::Linux && wayland_display.is_some_and(|display| !display.is_empty());
    (pages.outside() || !wayland).then(|| (DESIGN_OPEN_BROWSER_ENV.to_string(), "0".to_string()))
}

/// The design canvas's opens in a browser outside the app (agents' pages on
/// **In my browser**). Each new open of the canvas opens a tab, even at a
/// URL opened before: Gentle Shell reports an open only while no page shows
/// the canvas, so the earlier tab was closed. One open can reach the app
/// twice on an older Shell (its `notify` text and its `gentle_design`
/// result), so the same URL again within [`Self::SAME_OPEN`] is that same
/// open and opens nothing. A refresh is never an open.
#[derive(Debug, Default)]
pub struct OutsideCanvas {
    last: Option<(String, std::time::Instant)>,
}

impl OutsideCanvas {
    /// How far apart the two reports of one open can arrive.
    pub const SAME_OPEN: std::time::Duration = std::time::Duration::from_secs(3);

    /// Whether an open of the canvas at `url`, reported at `now`, opens a
    /// browser tab; it is remembered when it does.
    pub fn opens(&mut self, url: &str, now: std::time::Instant) -> bool {
        let same_open = self.last.as_ref().is_some_and(|(last, at)| {
            last == url && now.saturating_duration_since(*at) < Self::SAME_OPEN
        });
        if !same_open {
            self.last = Some((url.to_string(), now));
        }
        !same_open
    }
}

/// A web browser the app can open a page in.
#[derive(Debug)]
pub struct WebBrowser {
    /// What the settings file calls it.
    pub id: &'static str,
    pub name: &'static str,
    /// macOS app bundles, looked up in the usual app folders.
    mac: &'static [&'static str],
    windows: &'static [Spot],
    linux: &'static [Spot],
}

impl WebBrowser {
    /// Where to look on `os`, most preferred first; the first hit is used.
    pub fn lookups(&self, os: Os) -> Vec<Lookup> {
        app_lookups(os, self.mac, self.windows, self.linux)
    }
}

/// A Chromium-family browser installed per machine (`Program Files`) or per
/// user (`%LOCALAPPDATA%`) under the same folder on Windows.
macro_rules! both_windows_roots {
    ($path:literal) => {
        &[
            Spot::Path(Base::ProgramFiles, $path),
            Spot::Path(Base::LocalAppData, $path),
        ]
    };
}

pub const BROWSERS: &[WebBrowser] = &[
    WebBrowser {
        id: "safari",
        name: "Safari",
        mac: &["Safari.app"],
        windows: &[],
        linux: &[],
    },
    WebBrowser {
        id: "safari-preview",
        name: "Safari Technology Preview",
        mac: &["Safari Technology Preview.app"],
        windows: &[],
        linux: &[],
    },
    WebBrowser {
        id: "chrome",
        name: "Google Chrome",
        mac: &["Google Chrome.app"],
        windows: both_windows_roots!("Google/Chrome/Application/chrome.exe"),
        linux: &[
            Spot::Command("google-chrome"),
            Spot::Command("google-chrome-stable"),
            Spot::Flatpak("com.google.Chrome"),
        ],
    },
    WebBrowser {
        id: "chrome-canary",
        name: "Chrome Canary",
        mac: &["Google Chrome Canary.app"],
        windows: &[Spot::Path(
            Base::LocalAppData,
            "Google/Chrome SxS/Application/chrome.exe",
        )],
        linux: &[Spot::Command("google-chrome-unstable")],
    },
    WebBrowser {
        id: "chromium",
        name: "Chromium",
        mac: &["Chromium.app"],
        windows: &[Spot::Path(
            Base::LocalAppData,
            "Chromium/Application/chrome.exe",
        )],
        linux: &[
            Spot::Command("chromium"),
            Spot::Command("chromium-browser"),
            Spot::Path(Base::Root, "snap/bin/chromium"),
            Spot::Flatpak("org.chromium.Chromium"),
        ],
    },
    WebBrowser {
        id: "arc",
        name: "Arc",
        mac: &["Arc.app"],
        windows: &[],
        linux: &[],
    },
    WebBrowser {
        id: "dia",
        name: "Dia",
        mac: &["Dia.app"],
        windows: &[],
        linux: &[],
    },
    WebBrowser {
        id: "firefox",
        name: "Firefox",
        mac: &["Firefox.app"],
        windows: both_windows_roots!("Mozilla Firefox/firefox.exe"),
        linux: &[
            Spot::Command("firefox"),
            Spot::Path(Base::Root, "snap/bin/firefox"),
            Spot::Flatpak("org.mozilla.firefox"),
        ],
    },
    WebBrowser {
        id: "firefox-developer",
        name: "Firefox Developer Edition",
        mac: &["Firefox Developer Edition.app"],
        windows: &[Spot::Path(
            Base::ProgramFiles,
            "Firefox Developer Edition/firefox.exe",
        )],
        linux: &[Spot::Command("firefox-developer-edition")],
    },
    WebBrowser {
        id: "firefox-nightly",
        name: "Firefox Nightly",
        mac: &["Firefox Nightly.app"],
        windows: &[Spot::Path(
            Base::ProgramFiles,
            "Firefox Nightly/firefox.exe",
        )],
        linux: &[Spot::Command("firefox-nightly")],
    },
    WebBrowser {
        id: "zen",
        name: "Zen",
        mac: &["Zen.app", "Zen Browser.app"],
        windows: &[Spot::Path(Base::ProgramFiles, "Zen Browser/zen.exe")],
        linux: &[
            Spot::Command("zen-browser"),
            Spot::Command("zen"),
            Spot::Flatpak("app.zen_browser.zen"),
        ],
    },
    WebBrowser {
        id: "librewolf",
        name: "LibreWolf",
        mac: &["LibreWolf.app"],
        windows: &[Spot::Path(Base::ProgramFiles, "LibreWolf/librewolf.exe")],
        linux: &[
            Spot::Command("librewolf"),
            Spot::Flatpak("io.gitlab.librewolf-community"),
        ],
    },
    WebBrowser {
        id: "brave",
        name: "Brave",
        mac: &["Brave Browser.app"],
        windows: both_windows_roots!("BraveSoftware/Brave-Browser/Application/brave.exe"),
        linux: &[
            Spot::Command("brave-browser"),
            Spot::Command("brave"),
            Spot::Path(Base::Root, "snap/bin/brave"),
            Spot::Flatpak("com.brave.Browser"),
        ],
    },
    WebBrowser {
        id: "edge",
        name: "Microsoft Edge",
        mac: &["Microsoft Edge.app"],
        windows: &[Spot::Path(
            Base::ProgramFiles,
            "Microsoft/Edge/Application/msedge.exe",
        )],
        linux: &[
            Spot::Command("microsoft-edge"),
            Spot::Command("microsoft-edge-stable"),
            Spot::Flatpak("com.microsoft.Edge"),
        ],
    },
    WebBrowser {
        id: "opera",
        name: "Opera",
        mac: &["Opera.app"],
        windows: &[Spot::Path(
            Base::LocalAppData,
            "Programs/Opera/launcher.exe",
        )],
        linux: &[Spot::Command("opera"), Spot::Flatpak("com.opera.Opera")],
    },
    WebBrowser {
        id: "opera-gx",
        name: "Opera GX",
        mac: &["Opera GX.app"],
        windows: &[Spot::Path(
            Base::LocalAppData,
            "Programs/Opera GX/launcher.exe",
        )],
        linux: &[],
    },
    WebBrowser {
        id: "vivaldi",
        name: "Vivaldi",
        mac: &["Vivaldi.app"],
        windows: both_windows_roots!("Vivaldi/Application/vivaldi.exe"),
        linux: &[
            Spot::Command("vivaldi"),
            Spot::Command("vivaldi-stable"),
            Spot::Flatpak("com.vivaldi.Vivaldi"),
        ],
    },
    WebBrowser {
        id: "orion",
        name: "Orion",
        mac: &["Orion.app"],
        windows: &[],
        linux: &[],
    },
];

/// The saved browser choice: a catalogued browser, or `None` for the
/// system's default (also for a value this version doesn't know).
pub fn choice(value: &Value) -> Option<&'static WebBrowser> {
    let id = value.as_str()?;
    BROWSERS.iter().find(|browser| browser.id == id)
}

/// The settings value for `browser` (`None`: the system's default).
pub fn choice_json(browser: Option<&WebBrowser>) -> Value {
    browser.map_or(Value::Null, |browser| Value::from(browser.id))
}

/// What Configuration calls the system's default browser.
pub const SYSTEM_DEFAULT: &str = "System default";

/// The browser a page outside the app opens in (an agent's page, the card's
/// ↗): the chosen one only while agents' pages go to the browser, since the
/// choice is hidden otherwise. `None` is the system's default.
pub fn opener(
    pages: AgentPages,
    chosen: Option<&'static WebBrowser>,
) -> Option<&'static WebBrowser> {
    chosen.filter(|_| pages == AgentPages::Browser)
}

/// An http(s) URL that is safe to hand a browser as one argument: nothing
/// before the scheme (so it can't read as an option like `--flag`), no
/// spaces or control characters.
pub fn is_web_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    rest.is_some_and(|rest| !rest.is_empty())
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The program and arguments that open `url` in the browser found at
/// `found`: `open -a <App.app> <url>` on macOS (which also brings it to the
/// front), the browser's own program with the URL elsewhere. `None` for
/// anything but a [`is_web_url`] URL. Never a shell: each piece is one
/// argument.
pub fn launch(os: Os, found: &Path, url: &str) -> Option<(PathBuf, Vec<OsString>)> {
    if !is_web_url(url) {
        return None;
    }
    Some(match os {
        Os::Mac => (
            PathBuf::from("/usr/bin/open"),
            vec!["-a".into(), found.into(), url.into()],
        ),
        Os::Windows | Os::Linux => (found.to_path_buf(), vec![url.into()]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_pages_default_to_the_app_and_round_trip() {
        assert_eq!(AgentPages::default(), AgentPages::InApp);
        for pages in AgentPages::ALL {
            assert_eq!(AgentPages::from_json(&pages.to_json()), pages);
        }
        assert_eq!(
            AgentPages::from_json(&json!("browser")),
            AgentPages::Browser
        );
        assert_eq!(AgentPages::from_json(&json!("tab")), AgentPages::InApp);
        assert_eq!(AgentPages::from_json(&Value::Null), AgentPages::InApp);
        // Never chosen, or saved by something else: inside the app, and
        // nothing opens outside.
        assert_eq!(AgentPages::from_setting(None), AgentPages::InApp);
        assert!(!AgentPages::from_setting(None).outside());
        assert!(!AgentPages::from_setting(Some(&json!(true))).outside());
        assert!(AgentPages::from_setting(Some(&json!("browser"))).outside());
        assert_eq!(AgentPages::InApp.label(), "Inside the app");
        assert_eq!(AgentPages::Browser.label(), "In my browser");
    }

    #[test]
    fn the_design_canvas_keeps_the_system_browser_closed_whenever_the_app_opens_it() {
        let off = Some((DESIGN_OPEN_BROWSER_ENV.to_string(), "0".to_string()));
        // The app's card, or the chosen browser the app opens itself.
        for os in [Os::Mac, Os::Windows, Os::Linux] {
            for pages in AgentPages::ALL {
                assert_eq!(design_launch_env(pages, os, None), off);
            }
        }
        // Wayland: the card can't draw, so only Gentle Shell's tab shows it;
        // pages in the browser are still the app's own to open.
        let wayland = Some(std::ffi::OsStr::new("wayland-0"));
        assert_eq!(
            design_launch_env(AgentPages::InApp, Os::Linux, wayland),
            None
        );
        assert_eq!(
            design_launch_env(AgentPages::Browser, Os::Linux, wayland),
            off
        );
        // X11 (an empty `WAYLAND_DISPLAY` names no compositor), and a
        // variable that means nothing off Linux.
        let empty = Some(std::ffi::OsStr::new(""));
        assert_eq!(design_launch_env(AgentPages::InApp, Os::Linux, empty), off);
        assert_eq!(design_launch_env(AgentPages::InApp, Os::Mac, wayland), off);
    }

    #[test]
    fn every_new_canvas_open_opens_outside_but_one_open_only_once() {
        let url = "http://127.0.0.1:4100/tok/d/landing/";
        let start = std::time::Instant::now();
        let mut outside = OutsideCanvas::default();
        assert!(outside.opens(url, start));
        // The same open reported twice (`notify` text and tool result).
        assert!(!outside.opens(url, start + std::time::Duration::from_millis(50)));
        // The tab was closed and the agent opened the canvas again.
        let later = start + OutsideCanvas::SAME_OPEN;
        assert!(outside.opens(url, later));
        // Another design right after is its own open.
        let other = "http://127.0.0.1:4100/tok/d/pricing/";
        assert!(outside.opens(other, later));
        assert!(outside.opens(url, later));
    }

    #[test]
    fn browser_choice_maps_ids_and_falls_back_to_the_system_default() {
        let chrome = choice(&json!("chrome")).unwrap();
        assert_eq!(chrome.name, "Google Chrome");
        assert_eq!(choice(&choice_json(Some(chrome))).unwrap().id, "chrome");
        assert!(choice(&json!("netscape")).is_none());
        assert!(choice(&Value::Null).is_none());
        assert_eq!(choice_json(None), Value::Null);
    }

    #[test]
    fn the_chosen_browser_only_opens_pages_while_they_go_to_the_browser() {
        let arc = choice(&json!("arc"));
        assert_eq!(opener(AgentPages::Browser, arc).map(|b| b.id), Some("arc"));
        assert!(opener(AgentPages::InApp, arc).is_none());
        assert!(opener(AgentPages::Browser, None).is_none());
    }

    #[test]
    fn ids_and_names_are_unique() {
        let mut ids: Vec<_> = BROWSERS.iter().map(|b| b.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), BROWSERS.len());
        let mut names: Vec<_> = BROWSERS.iter().map(|b| b.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), BROWSERS.len());
    }

    #[test]
    fn every_os_has_the_common_browsers() {
        let found = |os: Os, id: &str| {
            BROWSERS
                .iter()
                .find(|b| b.id == id)
                .is_some_and(|b| !b.lookups(os).is_empty())
        };
        for id in ["chrome", "firefox", "brave", "edge", "vivaldi"] {
            assert!(found(Os::Mac, id) && found(Os::Windows, id) && found(Os::Linux, id));
        }
        assert!(found(Os::Mac, "safari") && !found(Os::Windows, "safari"));
        let arc = BROWSERS.iter().find(|b| b.id == "arc").unwrap();
        assert_eq!(
            arc.lookups(Os::Mac)[0],
            Lookup::Path(Base::Root, "Applications/Arc.app".into())
        );
        let firefox = BROWSERS.iter().find(|b| b.id == "firefox").unwrap();
        assert!(firefox.lookups(Os::Linux).contains(&Lookup::Path(
            Base::Root,
            "var/lib/flatpak/exports/bin/org.mozilla.firefox".into()
        )));
    }

    #[test]
    fn only_plain_web_urls_open() {
        assert!(is_web_url("http://127.0.0.1:5173/"));
        assert!(is_web_url("https://example.com/a?b=c#d"));
        assert!(!is_web_url("--remote-debugging-port=9222"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("https://"));
        assert!(!is_web_url("https://x.dev/ --flag"));
        assert!(!is_web_url("https://x.dev/\n"));
    }

    #[test]
    fn launch_passes_the_url_as_one_argument() {
        let url = "http://localhost:3000/a b";
        assert!(launch(Os::Mac, Path::new("/Applications/Arc.app"), url).is_none());
        let url = "http://localhost:3000/";
        let (program, args) = launch(Os::Mac, Path::new("/Applications/Arc.app"), url).unwrap();
        assert_eq!(program, PathBuf::from("/usr/bin/open"));
        assert_eq!(
            args,
            ["-a", "/Applications/Arc.app", url].map(OsString::from)
        );
        let exe = Path::new(r"C:\Program Files\Mozilla Firefox\firefox.exe");
        let (program, args) = launch(Os::Windows, exe, url).unwrap();
        assert_eq!(
            (program, args),
            (exe.to_path_buf(), vec![OsString::from(url)])
        );
        let (program, args) = launch(Os::Linux, Path::new("/usr/bin/firefox"), url).unwrap();
        assert_eq!(program, PathBuf::from("/usr/bin/firefox"));
        assert_eq!(args, [OsString::from(url)]);
        assert!(launch(Os::Linux, Path::new("/usr/bin/firefox"), "-new-window").is_none());
    }
}
