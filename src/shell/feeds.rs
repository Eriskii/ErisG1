//! What the Mac feeds the shell's tiles: the weather (open-meteo.com, where the Mac is by its address), its own
//! battery (`pmset`), and its notifications, read from Notification Center's database, which macOS keeps in
//! `~/Library/Group Containers/group.com.apple.usernoted/db2/db` (read-only, with the system's `sqlite3`). Steps and
//! the heart rate have no source yet: placeholders. Navigation has none either (the phone's maps app, later), but a
//! demo route.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command as Process;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;

use super::Command;
use crate::apps::{sysmon, weather};

const WEATHER_EVERY: Duration = Duration::from_secs(600);
const NOTES_EVERY: Duration = Duration::from_secs(2);
const BATTERY_EVERY: Duration = Duration::from_secs(30);
/// A notification pops up if it arrived this recently (seconds), so the ones there at the start don't.
const FRESH: f64 = 120.0;
/// Seconds from 1970 to 2001, where the Mac's dates start.
const MAC_EPOCH: f64 = 978_307_200.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Weather {
    /// The WMO weather code, whether it's day.
    pub code: u32,
    pub day: bool,
    pub celsius: f64,
    pub humidity: f64,
    /// The Mac shows Fahrenheit.
    pub fahrenheit: bool,
}

/// A notification in Notification Center.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Note {
    pub id: i64,
    /// The sending app's bundle ID and name.
    pub app: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    /// When it arrived, seconds since 2001 (0 unknown).
    pub delivered: f64,
}

impl Note {
    /// "Title: body" (the app's name for a title when it has none).
    pub fn line(&self) -> String {
        let title = if self.title.trim().is_empty() { &self.app_name } else { &self.title };
        if self.body.trim().is_empty() { title.clone() } else { format!("{}: {}", title.trim(), self.body.trim()) }
    }
}

fn curl(url: &str) -> Option<serde_json::Value> {
    let out = Process::new("curl").args(["-s", "--max-time", "15", url]).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// The weather where the Mac is, by its address.
pub(crate) fn weather() -> Option<Weather> {
    let place = curl("https://ipinfo.io/json")?;
    let (lat, lon) = place.get("loc")?.as_str()?.split_once(',')?;
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}\
         &current=temperature_2m,relative_humidity_2m,weather_code,is_day",
        lat.trim(),
        lon.trim()
    );
    let json = curl(&url)?;
    let current = |key: &str| json.get("current")?.get(key)?.as_f64();
    Some(Weather {
        code: current("weather_code")? as u32,
        day: current("is_day")? != 0.0,
        celsius: current("temperature_2m")?,
        humidity: current("relative_humidity_2m")?,
        fahrenheit: weather::mac_uses_fahrenheit(),
    })
}

/// Whether the Mac measures in metres (else feet and miles).
pub(crate) fn metric() -> bool {
    let read = |key: &str| {
        Process::new("defaults")
            .args(["read", "-g", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    match read("AppleMetricUnits") {
        Some(metric) => metric == "1",
        None => !read("AppleLocale").is_some_and(|l| l.ends_with("_US") || l.ends_with("_LR") || l.ends_with("_MM")),
    }
}

/// The Mac's battery: % and whether it's on power; `None` without a battery.
pub(crate) fn battery() -> Option<(u32, bool)> {
    let (percent, state) = sysmon::parse_pmset(&sysmon::output("pmset", &["-g", "batt"]))?;
    Some((percent, !state.starts_with("discharging")))
}

fn database() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Group Containers/group.com.apple.usernoted/db2/db")
}

/// What's in Notification Center, newest first; apps' names from `names` (filled as needed).
pub(crate) fn notifications(names: &mut HashMap<String, String>) -> Result<Vec<Note>, String> {
    let query = "select r.rec_id, a.identifier, ifnull(r.delivered_date, 0), hex(r.data) from record r \
                 join app a on a.app_id = r.app_id order by ifnull(r.delivered_date, 0) desc limit 40;";
    let db = database();
    let out = Process::new("/usr/bin/sqlite3")
        .args(["-readonly", "-separator", "|"])
        .arg(&db)
        .arg(query)
        .output()
        .map_err(|e| format!("sqlite3: {e}"))?;
    if !out.status.success() {
        return Err(format!("{}: {}", db.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    let mut notes = Vec::new();
    for row in String::from_utf8_lossy(&out.stdout).lines() {
        let mut fields = row.splitn(4, '|');
        let (Some(id), Some(app), Some(delivered), Some(hex)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some((title, body)) = request(hex) else { continue };
        let name = names.entry(app.to_string()).or_insert_with(|| app_name(app)).clone();
        notes.push(Note {
            id: id.parse().unwrap_or(0),
            app: app.to_string(),
            app_name: name,
            title,
            body,
            delivered: delivered.parse().unwrap_or(0.0),
        });
    }
    Ok(notes)
}

/// A record's title (and subtitle) and body, from its binary plist in hex.
fn request(hex: &str) -> Option<(String, String)> {
    let bytes: Vec<u8> =
        (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()).collect();
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    let req = value.as_dictionary()?.get("req")?.as_dictionary()?;
    let text = |key: &str| req.get(key).and_then(|v| v.as_string()).unwrap_or("").trim().to_string();
    let (title, subtitle, body) = (text("titl"), text("subt"), text("body"));
    let title = match (title.is_empty(), subtitle.is_empty()) {
        (false, false) => format!("{title} - {subtitle}"),
        (true, false) => subtitle,
        _ => title,
    };
    Some((title, body))
}

/// An app's name from its bundle ID: what Spotlight finds, or the ID's last part.
fn app_name(bundle: &str) -> String {
    let found = Process::new("mdfind")
        .arg(format!("kMDItemCFBundleIdentifier == '{}'", bundle.replace('\'', "")))
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find(|l| l.ends_with(".app"))
                .and_then(|l| std::path::Path::new(l).file_stem().map(|s| s.to_string_lossy().into_owned()))
        });
    found.unwrap_or_else(|| {
        let last = bundle.rsplit('.').next().unwrap_or(bundle);
        let mut chars = last.chars();
        chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
    })
}

/// The Mac's clock as Notification Center's dates count.
fn mac_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()) - MAC_EPOCH
}

/// Watches them, on threads of their own, and sends what changes: the notifications every 2 s (a new one also as a
/// pop-up), the battery every 30 s, the weather every 10 minutes.
pub(crate) fn watch(tx: UnboundedSender<Command>) {
    let weather_tx = tx.clone();
    std::thread::spawn(move || {
        loop {
            match weather() {
                Some(w) => {
                    if weather_tx.send(Command::Weather(w)).is_err() {
                        return;
                    }
                    std::thread::sleep(WEATHER_EVERY);
                }
                // Offline: try again sooner.
                None => std::thread::sleep(Duration::from_secs(60)),
            }
        }
    });
    std::thread::spawn(move || {
        let mut names = HashMap::new();
        let (mut shown, mut newest, mut warned): (Vec<Note>, Option<i64>, bool) = (Vec::new(), None, false);
        let (mut battery_at, mut last_battery) = (Instant::now() - BATTERY_EVERY, None);
        loop {
            match notifications(&mut names) {
                Ok(notes) => {
                    if let Some(newest) = newest {
                        let now = mac_now();
                        for note in notes.iter().rev().filter(|n| n.id > newest && now - n.delivered < FRESH) {
                            if tx.send(Command::Popup(note.clone())).is_err() {
                                return;
                            }
                        }
                    }
                    newest = notes.iter().map(|n| n.id).max().or(newest);
                    if notes != shown {
                        shown = notes.clone();
                        if tx.send(Command::Inbox(notes)).is_err() {
                            return;
                        }
                    }
                }
                Err(e) if !warned => {
                    eprintln!("notifications: {e}");
                    warned = true;
                }
                Err(_) => {}
            }
            if battery_at.elapsed() >= BATTERY_EVERY {
                battery_at = Instant::now();
                let now = battery();
                if now != last_battery {
                    last_battery = now;
                    let (percent, charging) = now.map_or((None, false), |(p, c)| (Some(p), c));
                    if tx.send(Command::Device(percent, charging)).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(NOTES_EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_notification_record() {
        let mut req = plist::Dictionary::new();
        req.insert("titl".into(), "Alex".into());
        req.insert("body".into(), "lunch?".into());
        let mut top = plist::Dictionary::new();
        top.insert("app".into(), "org.whispersystems.signal-desktop".into());
        top.insert("req".into(), plist::Value::Dictionary(req));
        let mut bytes = Vec::new();
        plist::Value::Dictionary(top).to_writer_binary(&mut bytes).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
        assert_eq!(request(&hex), Some(("Alex".to_string(), "lunch?".to_string())));
        let note = Note {
            id: 1,
            app: "x".into(),
            app_name: "Signal".into(),
            title: String::new(),
            body: "hi".into(),
            delivered: 0.0,
        };
        assert_eq!(note.line(), "Signal: hi");
    }
}
