//! `weather`: the weather where you are, from open-meteo.com (no key): the temperature now, big, and in words,
//! feels-like, humidity, wind, today's high and low, sunrise and sunset, and the next 12 hours of temperature with
//! the chance of rain as a small chart. Refreshed every 10 minutes.
//!
//! `erisg1 v0 app weather [--lat L --lon L] [--fahrenheit | --celsius]`: without a place, it's found from the Mac's
//! address (ipinfo.io); without a unit, the Mac's own setting (or Fahrenheit in a US locale). The panel sleeps a minute after it last changed; tap
//! either TouchBar to wake it (or put it to sleep).

use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};

use super::kit::{Canvas, Gesture, Glasses, Options};
use super::sim::Script;
use super::{App, Running};
use crate::link::Error;
use crate::proto::Side;

pub(crate) const APP: App = App {
    name: "weather",
    about: "the weather where you are: now, today, and the next 12 hours; tap to wake or sleep the panel",
    fun: false,
    options: Options { font: 2, big: 5, imu: false, boost: 5 },
    run,
    demo,
    demo_args: &[],
};

fn demo() -> Script {
    Script::still(12.0).touch(7.5, Side::Right, 1).touch(9.5, Side::Left, 1)
}

/// A JSON value: just enough of a reader for open-meteo's and ipinfo's answers.
#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn parse(text: &str) -> Option<Json> {
        let mut p = Parser { s: text.as_bytes(), i: 0 };
        let v = p.value()?;
        p.space();
        (p.i == p.s.len()).then_some(v)
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value at a path of keys.
    fn at(&self, path: &[&str]) -> Option<&Json> {
        path.iter().try_fold(self, |v, key| v.get(key))
    }

    fn num(&self) -> Option<f64> {
        if let Json::Num(n) = self { Some(*n) } else { None }
    }

    fn str(&self) -> Option<&str> {
        if let Json::Str(s) = self { Some(s) } else { None }
    }

    fn arr(&self) -> Option<&[Json]> {
        if let Json::Arr(a) = self { Some(a) } else { None }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.space();
        (self.s.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn value(&mut self) -> Option<Json> {
        self.space();
        match *self.s.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut fields = Vec::new();
                if self.eat(b'}').is_some() {
                    return Some(Json::Obj(fields));
                }
                loop {
                    self.space();
                    let Json::Str(key) = self.value()? else { return None };
                    self.eat(b':')?;
                    fields.push((key, self.value()?));
                    if self.eat(b',').is_none() {
                        self.eat(b'}')?;
                        return Some(Json::Obj(fields));
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                if self.eat(b']').is_some() {
                    return Some(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    if self.eat(b',').is_none() {
                        self.eat(b']')?;
                        return Some(Json::Arr(items));
                    }
                }
            }
            b'"' => {
                self.i += 1;
                let mut out = Vec::new();
                loop {
                    match *self.s.get(self.i)? {
                        b'"' => break,
                        b'\\' => {
                            self.i += 1;
                            match *self.s.get(self.i)? {
                                b'n' => out.push(b'\n'),
                                b't' => out.push(b'\t'),
                                b'u' => {
                                    let hex = std::str::from_utf8(self.s.get(self.i + 1..self.i + 5)?).ok()?;
                                    let c = char::from_u32(u32::from_str_radix(hex, 16).ok()?).unwrap_or('?');
                                    out.extend(c.to_string().as_bytes());
                                    self.i += 4;
                                }
                                c => out.push(c),
                            }
                        }
                        c => out.push(c),
                    }
                    self.i += 1;
                }
                self.i += 1;
                Some(Json::Str(String::from_utf8_lossy(&out).into_owned()))
            }
            b't' if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Some(Json::Bool(true))
            }
            b'f' if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Some(Json::Bool(false))
            }
            b'n' if self.s[self.i..].starts_with(b"null") => {
                self.i += 4;
                Some(Json::Null)
            }
            _ => {
                let start = self.i;
                while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || b"+-.eE".contains(c)) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok().map(Json::Num)
            }
        }
    }
}

/// WMO weather codes, as open-meteo gives them, in words.
fn conditions(code: u32) -> &'static str {
    match code {
        0 => "Clear sky",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 => "Light rain",
        63 => "Rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 => "Light snow",
        73 => "Snow",
        75 => "Heavy snow",
        77 => "Snow grains",
        80..=82 => "Showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm, hail",
        _ => "?",
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Forecast {
    place: String,
    /// "C" or "F", and the wind's unit.
    unit: &'static str,
    wind_unit: &'static str,
    temp: f64,
    feels: f64,
    humidity: f64,
    wind: f64,
    code: u32,
    high: f64,
    low: f64,
    sunrise: String,
    sunset: String,
    /// The next 12 hours from this one: the hour ("15"), temperature and chance of rain (%).
    hours: Vec<(String, f64, f64)>,
    /// When open-meteo's "now" is, "HH:MM".
    updated: String,
}

/// "2026-11-10T14:15" to "14:15".
fn hh_mm(time: &str) -> String {
    time.split('T').nth(1).unwrap_or(time).chars().take(5).collect()
}

/// The forecast in open-meteo's answer to `url`.
fn forecast(json: &Json, place: &str, fahrenheit: bool) -> Option<Forecast> {
    let current = |key: &str| json.at(&["current", key]).and_then(Json::num);
    let daily = |key: &str| json.at(&["daily", key]).and_then(Json::arr).and_then(|a| a.first().cloned());
    let now = json.at(&["current", "time"])?.str()?;
    let times = json.at(&["hourly", "time"])?.arr()?;
    let temps = json.at(&["hourly", "temperature_2m"])?.arr()?;
    let rain = json.at(&["hourly", "precipitation_probability"])?.arr()?;
    // The hour now is the first whose "YYYY-MM-DDTHH" matches.
    let first = times.iter().position(|t| t.str().is_some_and(|t| t.get(..13) == now.get(..13)))?;
    let hours = (first..(first + 12).min(times.len()))
        .map(|i| {
            let hour = times[i].str().map(|t| hh_mm(t).chars().take(2).collect()).unwrap_or_default();
            (hour, temps.get(i).and_then(Json::num).unwrap_or(0.0), rain.get(i).and_then(Json::num).unwrap_or(0.0))
        })
        .collect();
    Some(Forecast {
        place: place.to_string(),
        unit: if fahrenheit { "F" } else { "C" },
        wind_unit: if fahrenheit { "mph" } else { "km/h" },
        temp: current("temperature_2m")?,
        feels: current("apparent_temperature")?,
        humidity: current("relative_humidity_2m")?,
        wind: current("wind_speed_10m")?,
        code: current("weather_code")? as u32,
        high: daily("temperature_2m_max")?.num()?,
        low: daily("temperature_2m_min")?.num()?,
        sunrise: hh_mm(daily("sunrise")?.str()?),
        sunset: hh_mm(daily("sunset")?.str()?),
        hours,
        updated: hh_mm(now),
    })
}

fn url(lat: f64, lon: f64, fahrenheit: bool) -> String {
    let units = if fahrenheit { "&temperature_unit=fahrenheit&wind_speed_unit=mph" } else { "" };
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m\
         &hourly=temperature_2m,precipitation_probability&daily=temperature_2m_max,temperature_2m_min,sunrise,sunset\
         &timezone=auto&forecast_days=2{units}"
    )
}

fn curl(url: &str) -> Option<Json> {
    let out = Command::new("curl").args(["-s", "--max-time", "15", url]).output().ok()?;
    Json::parse(&String::from_utf8_lossy(&out.stdout))
}

/// Where the Mac is, from its address: latitude, longitude and the town.
fn locate() -> Option<(f64, f64, String)> {
    let json = curl("https://ipinfo.io/json")?;
    let (lat, lon) = json.get("loc")?.str()?.split_once(',')?;
    let city = json.get("city").and_then(Json::str).unwrap_or("Here").to_string();
    Some((lat.trim().parse().ok()?, lon.trim().parse().ok()?, city))
}

/// Whether the Mac is set to Fahrenheit: its temperature setting, or failing that a US locale.
fn mac_uses_fahrenheit() -> bool {
    let read = |key: &str| {
        Command::new("defaults")
            .args(["read", "-g", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    match read("AppleTemperatureUnit") {
        Some(unit) => unit == "Fahrenheit",
        None => read("AppleLocale").is_some_and(|locale| locale.ends_with("_US")),
    }
}

/// `--lat`, `--lon`, `--fahrenheit`, `--celsius` from the arguments.
/// A place to ask about: latitude and longitude.
type Place = Option<(f64, f64)>;

/// What a fetch gave: the forecast, or what went wrong.
type Got = Result<Forecast, String>;

fn options(args: &[String]) -> Result<(Place, Option<bool>), Error> {
    let value = |name: &str| -> Result<Option<f64>, Error> {
        match args.iter().position(|a| a == name) {
            Some(i) => args
                .get(i + 1)
                .and_then(|v| v.parse().ok())
                .map(Some)
                .ok_or_else(|| format!("{name} needs a number").into()),
            None => Ok(None),
        }
    };
    let place = match (value("--lat")?, value("--lon")?) {
        (Some(lat), Some(lon)) => Some((lat, lon)),
        (None, None) => None,
        _ => return Err("give both --lat and --lon".into()),
    };
    let unit = if args.iter().any(|a| a == "--fahrenheit") {
        Some(true)
    } else if args.iter().any(|a| a == "--celsius") {
        Some(false)
    } else {
        None
    };
    Ok((place, unit))
}

/// Fetches the forecast every 10 minutes on a thread of its own; reports failures as text.
fn fetch(place: Place, unit: Option<bool>, out: Sender<Got>) {
    std::thread::spawn(move || {
        let fahrenheit = unit.unwrap_or_else(mac_uses_fahrenheit);
        loop {
            let got = match place.map(|(lat, lon)| (lat, lon, format!("{lat:.2}, {lon:.2}"))).or_else(locate) {
                None => Err("can't tell where this is; give --lat and --lon".to_string()),
                Some((lat, lon, name)) => curl(&url(lat, lon, fahrenheit))
                    .and_then(|json| forecast(&json, &name, fahrenheit))
                    .ok_or_else(|| "no answer from open-meteo".to_string()),
            };
            if out.send(got).is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(600));
        }
    });
}

/// An open-meteo answer for a made-up November afternoon, for the preview and the tests.
fn made_up() -> String {
    let times: Vec<String> = (0..48).map(|h| format!("\"2026-11-{:02}T{:02}:00\"", 10 + h / 24, h % 24)).collect();
    let temps: Vec<String> = (0..48)
        .map(|h| format!("{:.1}", 9.0 + 5.0 * ((h as f64 - 9.0) / 24.0 * std::f64::consts::TAU).sin()))
        .collect();
    let rain: Vec<String> = (0..48)
        .map(|h| (if (17..22).contains(&(h % 24)) { 20 + (h % 5) * 12 } else { h % 3 * 5 }).to_string())
        .collect();
    format!(
        "{{\"current\":{{\"time\":\"2026-11-10T14:15\",\"temperature_2m\":12.4,\"apparent_temperature\":10.1,\
         \"relative_humidity_2m\":72,\"weather_code\":2,\"wind_speed_10m\":14.2}},\
         \"hourly\":{{\"time\":[{}],\"temperature_2m\":[{}],\"precipitation_probability\":[{}]}},\
         \"daily\":{{\"temperature_2m_max\":[14.6,13.0],\"temperature_2m_min\":[7.2,6.1],\
         \"sunrise\":[\"2026-11-10T07:02\",\"2026-11-11T07:04\"],\"sunset\":[\"2026-11-10T16:41\",\"2026-11-11T16:40\"]}}}}",
        times.join(","),
        temps.join(","),
        rain.join(",")
    )
}

/// The chart: x, y, w, h of the temperature line, and the rain bars' height under it.
const CHART: [i32; 4] = [56, 222, 552, 92];
const RAIN: i32 = 36;

fn degrees(t: f64) -> String {
    format!("{:.0}", t.round())
}

fn draw(c: &mut Canvas, f: &Forecast) {
    c.clear(0, 56, 640, 344);
    c.text(16, 66, &f.place.chars().take(24).collect::<String>(), 9);
    let updated = format!("at {}", f.updated);
    let w = c.width(&updated);
    c.text(624 - w, 66, &updated, 6);
    // The temperature, big, with a small square for the degree sign and the unit.
    let temp = degrees(f.temp);
    let w = c.big(24, 96, &temp, 15);
    c.rect(24 + w + 4, 98, 7, 7, 15);
    c.text(24 + w + 14, 98, f.unit, 12);
    let x = 200;
    c.text(x, 96, conditions(f.code), 14);
    c.text(x, 120, &format!("feels {}  humidity {:.0}%", degrees(f.feels), f.humidity), 10);
    c.text(
        16,
        152,
        &format!("wind {:.0} {}   high {}  low {}", f.wind, f.wind_unit, degrees(f.high), degrees(f.low)),
        11,
    );
    c.text(16, 176, &format!("sunrise {}   sunset {}", f.sunrise, f.sunset), 8);
    // The next 12 hours: temperatures as a line, the chance of rain as bars under it, every third hour named.
    let [cx, cy, cw, ch] = CHART;
    let (lo, hi) = f.hours.iter().fold((f64::MAX, f64::MIN), |(lo, hi), h| (lo.min(h.1), hi.max(h.1)));
    let span = (hi - lo).max(1.0);
    let step = cw as f64 / (f.hours.len().max(2) - 1) as f64;
    let at = |i: usize, t: f64| [cx as f64 + step * i as f64, cy as f64 + ch as f64 * (1.0 - (t - lo) / span)];
    let lines: Vec<([f64; 4], u8)> = f
        .hours
        .windows(2)
        .enumerate()
        .map(|(i, pair)| {
            let (a, b) = (at(i, pair[0].1), at(i + 1, pair[1].1));
            ([a[0], a[1], b[0], b[1]], 14)
        })
        .collect();
    c.lines(&lines);
    let base = (cy + ch + 8 + RAIN) as f64;
    for (i, h) in f.hours.iter().enumerate() {
        let x = cx + (step * i as f64) as i32;
        let tall = (RAIN as f64 * h.2 / 100.0).round() as i32;
        c.fill(x - 4, base as i32 - tall, 9, tall, 6);
    }
    c.line(cx, base as i32 + 1, cx + cw, base as i32 + 1, 4);
    c.text(4, cy - 6, &degrees(hi), 7);
    c.text(4, cy + ch - 10, &degrees(lo), 7);
    for (i, h) in f.hours.iter().enumerate().step_by(3) {
        c.text_centred(cx + (step * i as f64) as i32, base as i32 + 8, &h.0, 7);
    }
    c.text(cx + cw - 64, base as i32 - RAIN - 2, "rain", 5);
    c.show(0, 56, 640, 344);
}

/// How long the panel stays up after it last changed, seconds.
const AWAKE: f64 = 60.0;

fn run<'a>(g: &'a mut Glasses, args: &'a [String]) -> Running<'a> {
    Box::pin(async move {
        let (place, unit) = options(args)?;
        let (tx, rx): (Sender<Got>, Receiver<Got>) = mpsc::channel();
        if g.preview() {
            let json = Json::parse(&made_up()).ok_or("the made-up forecast doesn't parse")?;
            let _ = tx.send(
                forecast(&json, "Portland", false).ok_or("the made-up forecast is incomplete").map_err(str::to_string),
            );
        } else {
            fetch(place, unit, tx);
            g.frame(|_, c| {
                c.text_centred(320, 200, "asking open-meteo...", 8);
                c.show(0, 200, 640, 16);
            })
            .await?;
        }
        let (mut latest, mut awake, mut changed): (Option<Forecast>, bool, f64) = (None, true, g.now());
        while g.running() {
            let mut redraw = false;
            while let Ok(got) = rx.try_recv() {
                match got {
                    Ok(f) => {
                        redraw = latest.as_ref() != Some(&f);
                        latest = Some(f);
                    }
                    Err(e) => {
                        eprintln!("weather: {e}");
                        if latest.is_none() {
                            g.frame(|_, c| {
                                c.clear(0, 56, 640, 344);
                                c.text_centred(320, 200, &e.chars().take(38).collect::<String>(), 10);
                                c.show(0, 56, 640, 344);
                            })
                            .await?;
                        }
                    }
                }
            }
            for (_, gesture) in g.input().await? {
                if gesture == Gesture::Tap {
                    awake = !awake;
                    if awake {
                        redraw = true;
                    } else {
                        g.sleep_panel().await?;
                    }
                }
            }
            if redraw && let Some(f) = &latest {
                awake = true;
                changed = g.now();
                g.frame(|_, c| draw(c, f)).await?;
            }
            if awake && g.now() - changed > AWAKE {
                awake = false;
                g.sleep_panel().await?;
            }
            g.wait(0.25).await?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_json() {
        let j = Json::parse(r#" {"a": [1, -2.5e1, "x\"yA"], "b": {"c": true, "d": null}} "#).unwrap();
        assert_eq!(j.at(&["a"]).and_then(Json::arr).map(<[Json]>::len), Some(3));
        assert_eq!(j.at(&["a"]).unwrap().arr().unwrap()[1], Json::Num(-25.0));
        assert_eq!(j.at(&["a"]).unwrap().arr().unwrap()[2], Json::Str("x\"yA".into()));
        assert_eq!(j.at(&["b", "c"]), Some(&Json::Bool(true)));
        assert_eq!(Json::parse("[1,"), None);
    }

    #[test]
    fn reads_the_forecast() {
        let f = forecast(&Json::parse(&made_up()).unwrap(), "Here", false).unwrap();
        assert_eq!((f.temp, f.code, f.updated.as_str(), f.sunset.as_str()), (12.4, 2, "14:15", "16:41"));
        assert_eq!(f.hours.len(), 12);
        assert_eq!(f.hours[0].0, "14");
        assert_eq!(conditions(f.code), "Partly cloudy");
        assert!(url(45.5, -122.6, true).contains("temperature_unit=fahrenheit"));
    }

    #[test]
    fn reads_options() {
        let args: Vec<String> = ["--lat", "45.5", "--lon", "-122.6", "--fahrenheit"].map(String::from).to_vec();
        assert_eq!(options(&args).unwrap(), (Some((45.5, -122.6)), Some(true)));
        assert!(options(&["--lat".to_string(), "1".to_string()]).is_err());
    }
}
