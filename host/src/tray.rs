//! Menu bar (macOS) / system tray (Windows, Linux) UI. The server runs on
//! the tokio runtime; this owns the main thread's event loop.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::permissions;
use crate::server::{self, Host, HostEvent};
use crate::update::{self, Release};
use crate::video::cisco;

enum UiEvent {
    Host(HostEvent),
    Menu(MenuEvent),
    UpdateAvailable(Release),
    /// Ok once the update is installed and the app should restart.
    UpdateInstalled(Result<(), String>),
}

/// How often to look for a new release.
const UPDATE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// How often to re-check permissions while one is missing.
const PERMISSION_POLL: Duration = Duration::from_secs(2);

/// Linux servers and SSH sessions have no desktop to put a tray in.
pub fn available() -> bool {
    if cfg!(target_os = "linux") {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    } else {
        true
    }
}

pub fn run(host: Arc<Host>, runtime: tokio::runtime::Runtime) -> anyhow::Result<()> {
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        // Menu bar only: no Dock icon, no app menu.
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UiEvent::Menu(e));
    }));
    let proxy = event_loop.create_proxy();
    let events: server::Events = Arc::new(move |e: HostEvent| {
        eprintln!("{e}");
        let _ = proxy.send_event(UiEvent::Host(e));
    });
    runtime.spawn(server::run(host.clone(), events));

    let proxy = event_loop.create_proxy();
    std::thread::Builder::new().name("update-check".into()).spawn(move || {
        std::thread::sleep(Duration::from_secs(10));
        loop {
            match update::check() {
                Ok(Some(release)) => {
                    let _ = proxy.send_event(UiEvent::UpdateAvailable(release));
                }
                Ok(None) => {}
                Err(e) => eprintln!("update check failed: {e}"),
            }
            std::thread::sleep(UPDATE_INTERVAL);
        }
    })?;
    let proxy = event_loop.create_proxy();

    let mut ui: Option<TrayUi> = None;
    let mut next_permission_check = Instant::now();
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // macOS wants the tray created once the loop is actually running.
            Event::NewEvents(StartCause::Init) => match TrayUi::new(&host) {
                Ok(t) => {
                    if !t.permissions.all() {
                        show_setup_page_once(t.permissions);
                    }
                    ui = Some(t);
                }
                Err(e) => {
                    eprintln!("couldn't create tray icon: {e}");
                    *control_flow = ControlFlow::Exit;
                }
            },
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if let Some(ui) = ui.as_mut() {
                    ui.refresh_permissions();
                }
                next_permission_check = Instant::now() + PERMISSION_POLL;
            }
            Event::UserEvent(UiEvent::UpdateAvailable(release)) => {
                if let Some(ui) = ui.as_mut() {
                    ui.offer_update(release);
                }
            }
            Event::UserEvent(UiEvent::UpdateInstalled(result)) => match result {
                Ok(()) => {
                    host.shutdown();
                    update::relaunch();
                    *control_flow = ControlFlow::Exit;
                }
                Err(e) => {
                    eprintln!("update failed: {e}");
                    if let Some(ui) = ui.as_mut() {
                        ui.update_failed();
                    }
                    let _ = open::that(update::RELEASES_PAGE);
                }
            },
            Event::UserEvent(UiEvent::Host(e)) => {
                if let Some(ui) = ui.as_mut() {
                    ui.on_host_event(e);
                }
            }
            Event::UserEvent(UiEvent::Menu(e)) => {
                let Some(ui) = ui.as_mut() else { return };
                if e.id == ui.quit.id() {
                    host.shutdown();
                    *control_flow = ControlFlow::Exit;
                } else if e.id == ui.show_qr.id() {
                    show_pairing_page(&host);
                } else if e.id == ui.login.id() {
                    ui.toggle_login();
                } else if e.id == ui.about.id() {
                    let _ = open::that(concat!(
                        "https://github.com/iRunTech/remote-duo/releases/tag/v",
                        env!("CARGO_PKG_VERSION")
                    ));
                } else if e.id == ui.licenses.id() {
                    let _ = open::that(LICENSES_URL);
                } else if e.id == ui.allow_accessibility.id() {
                    permissions::request_accessibility();
                } else if e.id == ui.allow_screen.id() {
                    ui.request_screen(&host);
                } else if e.id == ui.update.id() {
                    ui.start_update(proxy.clone());
                } else if ui.codec.as_ref().is_some_and(|c| e.id == c.id()) {
                    ui.toggle_codec();
                }
            }
            _ => {}
        }
        // Watch for permissions being granted in System Settings.
        if ui.as_ref().is_some_and(|ui| !ui.permissions.all()) && *control_flow == ControlFlow::Wait {
            *control_flow = ControlFlow::WaitUntil(next_permission_check);
        }
    });
}

struct TrayUi {
    _tray: TrayIcon,
    menu: Menu,
    permissions: permissions::Status,
    allow_accessibility: MenuItem,
    allow_screen: MenuItem,
    /// Hidden until there's a newer release.
    update: MenuItem,
    pending_update: Option<Release>,
    status: MenuItem,
    show_qr: MenuItem,
    login: CheckMenuItem,
    /// Cisco's terms: users can turn its codec off, next to its notice.
    codec: Option<CheckMenuItem>,
    /// Shows the version; opens that release's notes.
    about: MenuItem,
    licenses: MenuItem,
    quit: MenuItem,
    auto_launch: Option<AutoLaunch>,
    /// device name -> viewing the screen
    devices: BTreeMap<String, bool>,
}

impl TrayUi {
    fn new(host: &Host) -> anyhow::Result<Self> {
        let auto_launch = login_item();
        let title = MenuItem::new(format!("Remote Duo · {}", host.name), false, None);
        let pin = MenuItem::new(format!("Pairing PIN: {}", host.pairing.pin), false, None);
        let show_qr = MenuItem::new("Show Pairing QR Code…", true, None);
        let status = MenuItem::new("Starting…", false, None);
        let enabled = auto_launch.as_ref().is_some_and(|a| a.is_enabled().unwrap_or(false));
        let login = CheckMenuItem::new("Start at Login", auto_launch.is_some(), enabled, None);
        let codec = crate::video::uses_cisco()
            .then(|| CheckMenuItem::new(cisco::NOTICE, true, cisco::enabled(), None));
        let about = MenuItem::new(concat!("Version ", env!("CARGO_PKG_VERSION"), " · What's New…"), true, None);
        let licenses = MenuItem::new("Licenses…", true, None);
        let allow_accessibility = MenuItem::new("⚠︎ Allow Accessibility (mouse and keyboard)…", true, None);
        let allow_screen = MenuItem::new("⚠︎ Allow Screen Recording (screen view)…", true, None);
        let update = MenuItem::new("Update", true, None);
        let quit = MenuItem::new("Quit Remote Duo", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &title,
            &pin,
            &show_qr,
            &PredefinedMenuItem::separator(),
            &status,
            &PredefinedMenuItem::separator(),
            &login,
        ])?;
        if let Some(codec) = &codec {
            menu.append(codec)?;
        }
        menu.append_items(&[&about, &licenses, &PredefinedMenuItem::separator(), &quit])?;

        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_tooltip(format!("Remote Duo · {}", host.name));
        // macOS tints a template icon to match the menu bar.
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon());
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icon());
        let tray = builder.build()?;

        let mut ui = Self {
            _tray: tray,
            menu,
            permissions: permissions::Status { accessibility: true, screen: true },
            allow_accessibility,
            allow_screen,
            update,
            pending_update: None,
            status,
            show_qr,
            login,
            codec,
            about,
            licenses,
            quit,
            auto_launch,
            devices: BTreeMap::new(),
        };
        ui.refresh_permissions();
        Ok(ui)
    }

    fn on_host_event(&mut self, e: HostEvent) {
        match e {
            HostEvent::Listening | HostEvent::Sound(..) => {}
            HostEvent::Connected(d) => {
                self.devices.insert(d, false);
            }
            HostEvent::Disconnected(d) => {
                self.devices.remove(&d);
            }
            HostEvent::Viewing(d, on) => {
                if let Some(v) = self.devices.get_mut(&d) {
                    *v = on;
                }
            }
            HostEvent::Failed(msg) => {
                self.status.set_text(format!("⚠︎ {msg}"));
                return;
            }
        }
        self.status.set_text(if self.devices.is_empty() {
            "No phones connected".to_string()
        } else {
            let names: Vec<String> = self
                .devices
                .iter()
                .map(|(d, viewing)| if *viewing { format!("{d} (viewing screen)") } else { d.clone() })
                .collect();
            format!("Connected: {}", names.join(", "))
        });
    }

    fn toggle_login(&mut self) {
        let Some(auto) = &self.auto_launch else { return };
        // The checkbox has already flipped by the time we hear about the click.
        let want = self.login.is_checked();
        let result = if want { auto.enable() } else { auto.disable() };
        if let Err(e) = result {
            eprintln!("couldn't change Start at Login: {e}");
            self.login.set_checked(!want);
        }
    }
}

impl TrayUi {
    /// Shows an "Allow …" item at the top of the menu for each missing permission.
    fn refresh_permissions(&mut self) {
        let now = permissions::status();
        let was = self.permissions;
        if now == was {
            return;
        }
        // Items sit right under the title, accessibility first.
        if was.accessibility != now.accessibility {
            if now.accessibility {
                let _ = self.menu.remove(&self.allow_accessibility);
            } else {
                let _ = self.menu.insert(&self.allow_accessibility, 1);
            }
        }
        if was.screen != now.screen {
            if now.screen {
                let _ = self.menu.remove(&self.allow_screen);
            } else {
                let _ = self.menu.insert(&self.allow_screen, if now.accessibility { 1 } else { 2 });
            }
        }
        self.permissions = now;
    }

    /// macOS only sees a new Screen Recording grant after a restart, so the
    /// second click restarts the app.
    fn request_screen(&mut self, host: &Host) {
        if self.allow_screen.text().starts_with("⚠︎") {
            permissions::request_screen();
            self.allow_screen.set_text("Allowed Screen Recording? Click to restart Remote Duo");
        } else {
            host.shutdown();
            update::relaunch();
            std::process::exit(0);
        }
    }

    fn offer_update(&mut self, release: Release) {
        if self.pending_update.is_none() {
            let _ = self.menu.insert(&self.update, 1);
        }
        self.update.set_text(format!("Update to Remote Duo Host {}…", release.version));
        self.update.set_enabled(true);
        self.pending_update = Some(release);
    }

    fn start_update(&mut self, proxy: tao::event_loop::EventLoopProxy<UiEvent>) {
        let Some(release) = self.pending_update.clone() else { return };
        self.update.set_text(format!("Installing {}…", release.version));
        self.update.set_enabled(false);
        std::thread::spawn(move || {
            let result = update::install(&release).map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UiEvent::UpdateInstalled(result));
        });
    }

    fn update_failed(&mut self) {
        self.update.set_text("Update failed. Download it from the website…");
        self.update.set_enabled(false);
    }

    fn toggle_codec(&mut self) {
        let Some(codec) = &self.codec else { return };
        let want = codec.is_checked();
        if let Err(e) = cisco::set_enabled(want) {
            eprintln!("couldn't change the OpenH264 setting: {e}");
            codec.set_checked(!want);
        }
    }
}

const LICENSES_URL: &str = "https://github.com/iRunTech/remote-duo/blob/main/THIRD-PARTY-NOTICES.md";

fn login_item() -> Option<AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    AutoLaunchBuilder::new()
        .set_app_name("Remote Duo Host")
        .set_app_path(&exe.to_string_lossy())
        .build()
        .ok()
}

/// First launch with a permission missing: open a page explaining what to
/// allow and why, with buttons into System Settings. Only once; after that
/// the "Allow …" menu items do the job.
fn show_setup_page_once(status: permissions::Status) {
    let marker = crate::identity::config_dir().join("setup-page-shown");
    if marker.exists() {
        return;
    }
    let step = |done: bool, title: &str, why: &str, url: &str| {
        let (badge, action) = if done {
            ("<span class=\"done\">✓ Allowed</span>".to_string(), String::new())
        } else {
            (String::new(), format!("<a class=\"button\" href=\"{url}\">Open System Settings</a>"))
        };
        format!("<li><h2>{title} {badge}</h2><p>{why}</p>{action}</li>")
    };
    let page = format!(
        r#"<!doctype html><meta charset="utf-8"><title>Set up Remote Duo Host</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font: 16px/1.5 -apple-system, system-ui, sans-serif; margin: 0; background: Canvas; color: CanvasText; }}
  main {{ max-width: 620px; margin: 0 auto; padding: 48px 20px; }}
  h1 {{ font-size: 28px; margin: 0 0 6px; }}
  .lede {{ opacity: .7; margin: 0 0 28px; }}
  ol {{ padding: 0; list-style: none; counter-reset: s; }}
  li {{ counter-increment: s; border: 1px solid color-mix(in srgb, CanvasText 15%, transparent);
        border-radius: 14px; padding: 18px 20px; margin-bottom: 14px; }}
  h2 {{ font-size: 18px; margin: 0 0 4px; }}
  h2::before {{ content: counter(s) ". "; }}
  p {{ margin: 0 0 12px; opacity: .8; }}
  .button {{ display: inline-block; background: #2f6fed; color: #fff; text-decoration: none;
             padding: 8px 14px; border-radius: 8px; font-weight: 600; }}
  .done {{ color: #2a9d4b; font-size: 15px; margin-left: 6px; }}
  .tip {{ opacity: .7; font-size: 14px; }}
</style>
<main>
  <h1>Set up Remote Duo Host</h1>
  <p class="lede">macOS asks you to allow two things before your iPhone can control this Mac. In each one,
  turn on <strong>Remote Duo Host</strong>.</p>
  <ol>
    {accessibility}
    {screen}
    <li><h2>Pair your iPhone</h2><p>Open Remote Duo on your iPhone, tap <strong>+</strong>, pick this Mac, and
    enter the PIN from the book icon in the menu bar.</p></li>
  </ol>
  <p class="tip">After allowing Screen Recording, macOS offers to <strong>Quit &amp; Reopen</strong> Remote Duo
  Host; choose that. Missed a step? The book icon's menu shows what's still needed.</p>
</main>"#,
        accessibility = step(
            status.accessibility,
            "Accessibility",
            "Lets your iPhone move the pointer, click, and type.",
            permissions::ACCESSIBILITY_SETTINGS
        ),
        screen = step(
            status.screen,
            "Screen &amp; System Audio Recording",
            "Lets your iPhone show this Mac's screen and play its sound.",
            permissions::SCREEN_SETTINGS
        ),
    );
    let path = std::env::temp_dir().join("remote-duo-setup.html");
    if std::fs::write(&path, page).is_ok() && open::that(&path).is_ok() {
        let _ = std::fs::create_dir_all(crate::identity::config_dir());
        let _ = std::fs::write(marker, b"");
    }
}

/// Write a page with a big QR code and open it in the default browser.
fn show_pairing_page(host: &Host) {
    let Some(url) = host.pairing_url() else {
        eprintln!("no network address to pair with");
        return;
    };
    let Ok(code) = qrcode::QrCode::new(url.as_bytes()) else { return };
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(320, 320)
        .quiet_zone(true)
        .build();
    let name = html_escape(&host.name);
    let page = format!(
        r#"<!doctype html><meta charset="utf-8"><title>Pair with Remote Duo</title>
<style>
  body {{ font: 16px -apple-system, system-ui, sans-serif; background: #111; color: #eee;
         display: grid; place-items: center; min-height: 100vh; margin: 0; }}
  main {{ text-align: center; }}
  .qr {{ background: #fff; padding: 20px; border-radius: 20px; display: inline-block; }}
  .qr svg {{ display: block; width: 320px; height: 320px; }}
  .pin {{ font: 600 34px ui-monospace, monospace; letter-spacing: .15em; margin: 8px 0 0; }}
  p {{ color: #aaa; }}
</style>
<main>
  <h1>Pair with {name}</h1>
  <div class="qr">{svg}</div>
  <p>Scan with your iPhone camera, or enter this PIN in Remote Duo:</p>
  <div class="pin">{pin}</div>
</main>"#,
        pin = host.pairing.pin,
    );
    let path = std::env::temp_dir().join("remote-duo-pair.html");
    if let Err(e) = std::fs::write(&path, page) {
        eprintln!("couldn't write pairing page: {e}");
        return;
    }
    if let Err(e) = open::that(&path) {
        eprintln!("couldn't open pairing page: {e}");
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A folded-phone glyph drawn in code: a rounded outline split by the fold.
/// Black-on-transparent so macOS can tint it as a template image; other
/// platforms can't tint, so they get the brand purple, which reads on both
/// light and dark taskbars (white vanished on Windows' light theme).
fn icon() -> Icon {
    // An unfolded phone, drawn as an open book: two pages tilting up from a spine.
    const S: usize = 44;
    const LINES: [((f32, f32), (f32, f32)); 7] = [
        ((22.0, 12.0), (4.0, 8.0)),
        ((4.0, 8.0), (4.0, 33.0)),
        ((4.0, 33.0), (22.0, 37.0)),
        ((22.0, 12.0), (40.0, 8.0)),
        ((40.0, 8.0), (40.0, 33.0)),
        ((40.0, 33.0), (22.0, 37.0)),
        ((22.0, 12.0), (22.0, 37.0)),
    ];
    let mut rgba = vec![0u8; S * S * 4];
    for y in 0..S {
        for x in 0..S {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let d = LINES.iter().map(|&(a, b)| distance_to_segment(p, a, b)).fold(f32::MAX, f32::min);
            // 3px strokes with a one-pixel antialiased edge.
            let alpha = (2.0 - d).clamp(0.0, 1.0);
            let out = &mut rgba[(y * S + x) * 4..][..4];
            if !cfg!(target_os = "macos") {
                out[..3].copy_from_slice(&[0x79, 0x6c, 0xbf]);
            }
            out[3] = (alpha * 255.0) as u8;
        }
    }
    Icon::from_rgba(rgba, S as u32, S as u32).expect("valid icon")
}

fn distance_to_segment((px, py): (f32, f32), (ax, ay): (f32, f32), (bx, by): (f32, f32)) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (px - ax - t * dx).hypot(py - ay - t * dy)
}
