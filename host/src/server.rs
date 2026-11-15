//! The network side of the host: Bonjour advertising, the control channel
//! (pairing + input), and the video channel. Reports what happens through
//! `HostEvent`s so either the tray UI or the terminal can show it.

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use crate::fit::Fit;
use crate::identity::Identity;
use crate::input::{self, InputTx};
use crate::pairing::Pairing;
use crate::protocol::{ClientMsg, HostMsg};
use crate::video;

const SERVICE_TYPE: &str = "_remoteduo._tcp.local.";
const DEFAULT_PORT: u16 = 47800;

#[derive(Debug, Clone)]
pub enum HostEvent {
    Listening,
    Connected(String),
    Disconnected(String),
    Viewing(String, bool),
    Sound(String, bool),
    Failed(String),
}

impl fmt::Display for HostEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostEvent::Listening => write!(f, "listening for phones"),
            HostEvent::Connected(d) => write!(f, "{d} connected"),
            HostEvent::Disconnected(d) => write!(f, "{d} disconnected"),
            HostEvent::Viewing(d, true) => write!(f, "{d} is viewing the screen"),
            HostEvent::Viewing(d, false) => write!(f, "{d} stopped viewing the screen"),
            HostEvent::Sound(d, true) => write!(f, "{d} is playing the computer's sound"),
            HostEvent::Sound(d, false) => write!(f, "{d} stopped playing the computer's sound"),
            HostEvent::Failed(e) => write!(f, "error: {e}"),
        }
    }
}

pub type Events = Arc<dyn Fn(HostEvent) + Send + Sync>;

/// Everything the server needs, shared by the tray and connection tasks.
pub struct Host {
    pub name: String,
    pub port: u16,
    pub pairing: Pairing,
    pub identity: Identity,
    input: InputTx,
    fit: Fit,
    mdns: Mutex<Option<ServiceDaemon>>,
}

impl Host {
    pub fn new() -> anyhow::Result<Self> {
        let port = std::env::var("REMOTE_DUO_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        Ok(Self {
            name: display_name(),
            port,
            pairing: Pairing::load(),
            identity: Identity::load_or_create()?,
            input: input::spawn()?,
            fit: Fit::new(),
            mdns: Mutex::new(None),
        })
    }

    /// The link the phone's camera opens to pair: remoteduo://pair?...
    pub fn pairing_url(&self) -> Option<String> {
        let ip = lan_ip()?;
        Some(format!(
            "remoteduo://pair?addr={ip}:{}&name={}&pin={}&fp={}",
            self.port,
            urlencoding::encode(&self.name),
            self.pairing.pin,
            self.identity.fingerprint
        ))
    }

    /// Withdraw the Bonjour ad so phones don't list a ghost host.
    pub fn shutdown(&self) {
        if let Some(mdns) = self.mdns.lock().unwrap().take() {
            if let Ok(rx) = mdns.shutdown() {
                let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
            }
        }
    }
}

/// Accept connections until the process exits.
pub async fn run(host: Arc<Host>, events: Events) {
    let listener = listen(host.port, &events).await;
    match advertise(&host.name, host.port, &host.identity.fingerprint) {
        Ok(mdns) => *host.mdns.lock().unwrap() = Some(mdns),
        Err(e) => events(HostEvent::Failed(format!("can't advertise on the network: {e}"))),
    }
    events(HostEvent::Listening);
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };
        let (host, events) = (host.clone(), events.clone());
        tokio::spawn(async move {
            let _ = stream.set_nodelay(true);
            // Plain TCP probes (the phone resolving our address) never finish a handshake.
            let Ok(stream) = host.identity.acceptor.accept(stream).await else { return };
            if let Err(e) = serve(stream, addr, &host, &events).await {
                eprintln!("{addr}: {e}");
            }
        });
    }
}

/// Binds our port, retrying while something else holds it (often another
/// copy of the host that's about to quit), so we start working the moment
/// it's free instead of sitting there broken.
async fn listen(port: u16, events: &Events) -> TcpListener {
    let mut reported = None;
    loop {
        match TcpListener::bind(("0.0.0.0", port)).await {
            Ok(l) => return l,
            Err(e) => {
                let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                    format!("Port {port} is in use (is Remote Duo already running?), retrying…")
                } else {
                    format!("can't listen on port {port}: {e}, retrying…")
                };
                if reported.as_ref() != Some(&msg) {
                    events(HostEvent::Failed(msg.clone()));
                    reported = Some(msg);
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

/// The address other devices on the LAN can reach us at. Connecting a UDP
/// socket sends nothing; it just makes the OS pick the outbound interface.
pub fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

/// Friendly machine name: the macOS Computer Name, else the short hostname.
fn display_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !name.is_empty() {
            return name;
        }
    }
    let host = hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
    match host.split('.').next() {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => "Computer".to_string(),
    }
}

fn advertise(host_name: &str, port: u16, fingerprint: &str) -> anyhow::Result<ServiceDaemon> {
    let mdns = ServiceDaemon::new()?;
    let lan = lan_ip();
    // A computer on Ethernet and Wi-Fi at once hears its own announcement on
    // the other interface, takes it for a rival, and renames itself "Name (2)".
    // Advertising on the one interface we'd hand out as `ip` avoids that.
    if let Some(ip) = lan {
        mdns.disable_interface(IfKind::All)?;
        mdns.enable_interface(IfKind::Addr(ip))?;
    }
    // mDNS host labels can't contain dots or spaces.
    let label: String = host_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Phones connect to `ip` directly; resolving our .local name can race
    // with the OS's own mDNS responder right after startup.
    let ip = lan.map(|ip| ip.to_string()).unwrap_or_default();
    let port_s = port.to_string();
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        host_name,
        &format!("{label}.local."),
        "",
        port,
        &[("os", std::env::consts::OS), ("v", "1"), ("ip", ip.as_str()), ("port", port_s.as_str()), ("fp", fingerprint)][..],
    )?
    .enable_addr_auto();
    mdns.register(info)?;
    Ok(mdns)
}

async fn serve<S>(stream: S, addr: SocketAddr, host: &Host, events: &Events) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else { return Ok(()) };
    let (mut tx, mut rx) = ws.split();
    let send = |m: HostMsg| Message::Text(serde_json::to_string(&m).unwrap().into());

    // Handshake: the first frame must be a valid hello.
    let (device, channel, max_width, display) = match next_msg(&mut rx).await? {
        Some(ClientMsg::Hello { name, pin, token, channel, max_width, display }) => {
            // Only the control channel may pair with a PIN.
            let pin = if channel.is_some() { None } else { pin };
            match host.pairing.authorize(&name, pin.as_deref(), token.as_deref()) {
                Ok(token) => {
                    tx.send(send(HostMsg::Welcome {
                        token,
                        host: host.name.clone(),
                        os: std::env::consts::OS,
                        displays: crate::displays::list(),
                        fit: crate::fit::AVAILABLE,
                    }))
                    .await?;
                    (name, channel, max_width, display)
                }
                Err(msg) => {
                    tx.send(send(HostMsg::Error { msg: msg.into() }))
                        .await?;
                    return Ok(());
                }
            }
        }
        _ => return Ok(()),
    };

    if channel.as_deref() == Some("audio") {
        return stream_audio(tx, rx, &device, events).await;
    }
    if channel.as_deref() == Some("video") {
        let max_width = max_width.unwrap_or(1920).clamp(320, 3840);
        return stream_video(tx, rx, &device, max_width, display, events).await;
    }
    eprintln!("{device} connected from {addr}");
    events(HostEvent::Connected(device.clone()));

    static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);
    let conn = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
    let result = control_loop(&mut tx, &mut rx, conn, host).await;
    host.fit.release(conn);
    events(HostEvent::Disconnected(device));
    result
}

async fn control_loop<Tx, Rx>(tx: &mut Tx, rx: &mut Rx, conn: u64, host: &Host) -> anyhow::Result<()>
where
    Tx: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    Rx: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let send = |m: HostMsg| Message::Text(serde_json::to_string(&m).unwrap().into());
    while let Some(msg) = next_msg(rx).await? {
        match msg {
            ClientMsg::Ping => tx.send(send(HostMsg::Pong)).await?,
            ClientMsg::Fit { w, h } => {
                let result = host.fit.set(conn, w, h).await;
                let (display, error) = match result {
                    Ok(id) => (Some(id), None),
                    Err(e) => (None, Some(e)),
                };
                // Mirroring takes a moment to settle before the display list reflects it.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let displays = crate::displays::list();
                tx.send(send(HostMsg::Fitted { display, displays, error })).await?;
            }
            ClientMsg::Unfit => {
                host.fit.clear(conn).await;
                let displays = crate::displays::list();
                tx.send(send(HostMsg::Fitted { display: None, displays, error: None })).await?;
            }
            ClientMsg::Hello { .. } | ClientMsg::Keyframe => {}
            other => {
                host.input.send(other)?;
            }
        }
    }
    Ok(())
}

/// Video channel: stream encoded screen packets until the client goes away.
async fn stream_video<Tx, Rx>(
    mut tx: Tx,
    mut rx: Rx,
    device: &str,
    max_width: u32,
    display: Option<u32>,
    events: &Events,
) -> anyhow::Result<()>
where
    Tx: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    Rx: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let send = |m: HostMsg| Message::Text(serde_json::to_string(&m).unwrap().into());
    let mut source = match tokio::task::spawn_blocking(move || video::start(max_width, display)).await? {
        Ok(source) => source,
        Err(e) => {
            eprintln!("screen view for {device}: {e}");
            tx.send(send(HostMsg::Error { msg: e.to_string() })).await?;
            return Ok(());
        }
    };
    events(HostEvent::Viewing(device.to_string(), true));
    let mut cursor = video::cursor_updates(source.display.clone(), !source.cursor_in_video);
    loop {
        tokio::select! {
            packet = source.rx.recv() => match packet {
                Some(p) => tx.send(Message::Binary(p.encode().into())).await?,
                None => break,
            },
            Some(p) = cursor.recv() => tx.send(Message::Binary(p.encode().into())).await?,
            msg = next_msg(&mut rx) => match msg? {
                Some(ClientMsg::Keyframe) => source.request_keyframe(),
                Some(_) => {}
                None => break,
            },
        }
    }
    events(HostEvent::Viewing(device.to_string(), false));
    Ok(())
}

/// Audio channel: stream PCM packets until the client goes away.
async fn stream_audio<Tx, Rx>(mut tx: Tx, mut rx: Rx, device: &str, events: &Events) -> anyhow::Result<()>
where
    Tx: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    Rx: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let send = |m: HostMsg| Message::Text(serde_json::to_string(&m).unwrap().into());
    let mut source = match tokio::task::spawn_blocking(crate::audio::start).await? {
        Ok(source) => source,
        Err(e) => {
            eprintln!("sound for {device}: {e}");
            tx.send(send(HostMsg::Error { msg: e.to_string() })).await?;
            return Ok(());
        }
    };
    events(HostEvent::Sound(device.to_string(), true));
    loop {
        tokio::select! {
            packet = source.rx.recv() => match packet {
                Some(p) => tx.send(Message::Binary(p.into())).await?,
                None => break,
            },
            msg = next_msg(&mut rx) => if msg?.is_none() { break },
        }
    }
    events(HostEvent::Sound(device.to_string(), false));
    Ok(())
}

async fn next_msg<S>(rx: &mut S) -> anyhow::Result<Option<ClientMsg>>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    use tokio_tungstenite::tungstenite::{error::ProtocolError, Error};
    while let Some(frame) = rx.next().await {
        let frame = match frame {
            // Phones drop off Wi-Fi or get backgrounded; that's a normal goodbye.
            Err(Error::ConnectionClosed | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)) => {
                return Ok(None)
            }
            other => other?,
        };
        match frame {
            Message::Text(t) => match serde_json::from_str(t.as_str()) {
                Ok(m) => return Ok(Some(m)),
                Err(e) => eprintln!("bad message {t}: {e}"),
            },
            Message::Close(_) => return Ok(None),
            _ => {}
        }
    }
    Ok(None)
}
