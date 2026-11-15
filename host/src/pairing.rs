//! Pairing: a 6-digit PIN shown on the host is traded once for a long-lived
//! token that the phone stores and presents on every later connection.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand::RngExt;

pub struct Pairing {
    pub pin: String,
    path: PathBuf,
    /// token -> device name
    devices: Mutex<HashMap<String, String>>,
    /// False where no one can see the PIN: only already-paired phones get in.
    accepts_pin: bool,
    /// Wrong PINs since the last lockout, and when the current lockout ends.
    /// Global rather than per address, so switching addresses doesn't help a guesser.
    guesses: Mutex<(u32, Option<Instant>)>,
}

/// Wrong PINs allowed before pairing pauses for `LOCKOUT`. Five a minute
/// makes guessing all million PINs take months, even on a café network.
const MAX_GUESSES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

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
        Self { pin, path, devices: Mutex::new(devices), accepts_pin: true, guesses: Mutex::new((0, None)) }
    }

    /// Paired phones only, for the server that runs before anyone signs in.
    #[cfg(windows)]
    pub fn paired_only() -> Self {
        Self { accepts_pin: false, ..Self::load() }
    }

    /// False for the server that runs before anyone signs in on Windows.
    pub fn accepts_pin(&self) -> bool {
        self.accepts_pin
    }

    /// Returns the token to hand back to the client, or why it was rejected.
    pub fn authorize(&self, name: &str, pin: Option<&str>, token: Option<&str>) -> Result<String, &'static str> {
        let mut devices = self.devices.lock().unwrap();
        if let Some(t) = token {
            if devices.contains_key(t) {
                return Ok(t.to_string());
            }
        }
        if !self.accepts_pin {
            return Err("Sign in to this computer first, then pair from Remote Duo's menu.");
        }
        let Some(pin) = pin else { return Err("pairing failed: wrong PIN") };
        let mut guesses = self.guesses.lock().unwrap();
        if guesses.1.is_some_and(|until| Instant::now() < until) {
            return Err("Too many wrong PINs. Wait a minute, then try again.");
        }
        if pin != self.pin {
            guesses.0 += 1;
            if guesses.0 >= MAX_GUESSES {
                *guesses = (0, Some(Instant::now() + LOCKOUT));
                return Err("Too many wrong PINs. Wait a minute, then try again.");
            }
            return Err("pairing failed: wrong PIN");
        }
        *guesses = (0, None);
        let mut bytes = [0u8; 24];
        rand::rng().fill(&mut bytes);
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        devices.insert(token.clone(), name.to_string());
        self.save(&devices);
        Ok(token)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_pins_lock_out_even_the_right_one() {
        let p = Pairing {
            pin: "123456".into(),
            path: std::env::temp_dir().join("remote-duo-test-devices.json"),
            devices: Mutex::new(HashMap::new()),
            accepts_pin: true,
            guesses: Mutex::new((0, None)),
        };
        for _ in 0..MAX_GUESSES {
            assert!(p.authorize("phone", Some("000000"), None).is_err());
        }
        assert!(p.authorize("phone", Some("123456"), None).unwrap_err().starts_with("Too many"));
        // A device that already paired still gets in by token.
        p.devices.lock().unwrap().insert("tok".into(), "old phone".into());
        assert_eq!(p.authorize("old phone", None, Some("tok")).unwrap(), "tok");
    }
}
