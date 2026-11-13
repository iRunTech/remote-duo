//! Pairing: a 6-digit PIN shown on the host is traded once for a long-lived
//! token that the phone stores and presents on every later connection.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use rand::RngExt;

pub struct Pairing {
    pub pin: String,
    path: PathBuf,
    /// token -> device name
    devices: Mutex<HashMap<String, String>>,
}

impl Pairing {
    pub fn load() -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("remote-duo")
            .join("devices.json");
        let devices = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let pin = format!("{:06}", rand::rng().random_range(0..1_000_000));
        Self { pin, path, devices: Mutex::new(devices) }
    }

    /// Returns the token to hand back to the client, or None if rejected.
    pub fn authorize(&self, name: &str, pin: Option<&str>, token: Option<&str>) -> Option<String> {
        let mut devices = self.devices.lock().unwrap();
        if let Some(t) = token {
            if devices.contains_key(t) {
                return Some(t.to_string());
            }
        }
        if pin? != self.pin {
            return None;
        }
        let mut bytes = [0u8; 24];
        rand::rng().fill(&mut bytes);
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        devices.insert(token.clone(), name.to_string());
        self.save(&devices);
        Some(token)
    }

    fn save(&self, devices: &HashMap<String, String>) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(devices) {
            if let Err(e) = std::fs::write(&self.path, s) {
                eprintln!("could not save paired devices: {e}");
            }
        }
    }
}
