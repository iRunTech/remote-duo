//! Menu bar (macOS) / system tray (Windows, Linux) UI. The server runs on
//! the tokio runtime; this owns the main thread's event loop.

use std::collections::BTreeMap;
use std::sync::Arc;

use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::server::{self, Host, HostEvent};
use crate::video::cisco;

enum UiEvent {
    Host(HostEvent),
    Menu(MenuEvent),
}

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

    let mut ui: Option<TrayUi> = None;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // macOS wants the tray created once the loop is actually running.
            Event::NewEvents(StartCause::Init) => match TrayUi::new(&host) {
                Ok(t) => ui = Some(t),
                Err(e) => {
                    eprintln!("couldn't create tray icon: {e}");
                    *control_flow = ControlFlow::Exit;
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
                } else if e.id == ui.licenses.id() {
                    let _ = open::that(LICENSES_URL);
                } else if ui.codec.as_ref().is_some_and(|c| e.id == c.id()) {
                    ui.toggle_codec();
                }
            }
            _ => {}
        }
    });
}

struct TrayUi {
    _tray: TrayIcon,
    status: MenuItem,
    show_qr: MenuItem,
    login: CheckMenuItem,
    /// Cisco's terms: users can turn its codec off, next to its notice.
    codec: Option<CheckMenuItem>,
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
        let licenses = MenuItem::new("Licenses…", true, None);
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
        menu.append_items(&[&licenses, &PredefinedMenuItem::separator(), &quit])?;

        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(format!("Remote Duo · {}", host.name));
        // macOS tints a template icon to match the menu bar.
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon());
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icon());
        let tray = builder.build()?;

        Ok(Self {
            _tray: tray,
            status,
            show_qr,
            login,
            codec,
            licenses,
            quit,
            auto_launch,
            devices: BTreeMap::new(),
        })
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
/// platforms get the same glyph in white, for dark trays.
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
                out[..3].fill(255);
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
