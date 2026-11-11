//! The shell: a program that lives on the glasses and shows tiles (`layout.rs`, `program.rs`), and the Mac's side of
//! it.
//!
//! `erisg1 shell` connects to the glasses, puts the shell on them (or finds it already running there), sends it the
//! time and the dates, and passes on what it's sent: notifications and text from `erisg1 shell say` and `text`, and
//! what the Mac knows (`feeds.rs`): the weather, its battery, its notifications. It serves the layout editor
//! (`editor.rs`) at http://127.0.0.1:4141. When it stops, the shell goes on running on the glasses by itself, and a
//! later run picks it up where it is.

pub(crate) mod editor;
pub(crate) mod feeds;
pub(crate) mod fonts;
pub(crate) mod icons;
pub(crate) mod layout;
pub(crate) mod nav;
pub(crate) mod program;
pub(crate) mod sim;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Timelike;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use layout::{Kind, Layout};
use program::{Shell, TileData, WANT_KEEP, WANT_OFF, WANT_ON};

use crate::link::{self, Error};
use crate::proto::{self, Side};
use crate::v5::{self, prop, trigger};
use crate::v5_link::{self, Arm, host_us, side_name};

pub(crate) const PORT: u16 = 4141;
/// How long after the Mac sends an update both arms act on it.
const AHEAD_US: u32 = 300_000;
/// The time: acted on before the next whole second it names, which is at least 0.3 s away (`next_tick`).
const TIME_AHEAD_US: u32 = 250_000;
const CHECK_EVERY: Duration = Duration::from_secs(30);
const TIME_EVERY: Duration = Duration::from_secs(600);
const CONNECT_EVERY: Duration = Duration::from_secs(20);
/// A notification sent while the glasses are away still shows if they come back within this.
const KEEP_PENDING: Duration = Duration::from_secs(120);
/// The demo route: so many metres closer to the turn every step.
const DEMO_STEP: f64 = 20.0;
const DEMO_EVERY: Duration = Duration::from_secs(2);

/// Where the layout is kept.
pub(crate) fn layout_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    home.join(".config/erisg1/shell.json")
}

fn socket() -> PathBuf {
    std::env::temp_dir().join("erisg1-shell.sock")
}

/// The saved layout, or the default one.
pub(crate) fn current_layout() -> Result<Layout, String> {
    let path = layout_path();
    if path.exists() { Layout::load(&path) } else { Ok(Layout::default()) }
}

/// What the rest of the Mac asks of the shell.
pub(crate) enum Command {
    /// A notification: "TITLE: text", or just text, and its icon (a category; none, a bell).
    Say(String, Option<u8>),
    /// Text for a tile by name; empty turns it off.
    Text(String, String),
    /// A tile on for so many seconds (0: until turned off), or off.
    Show(String, f64),
    Hide(String),
    /// A new layout: saved, and put on the glasses.
    Apply(Layout, oneshot::Sender<Result<String, String>>),
    /// What the Mac knows, as it changes (`feeds::watch`): the weather, its battery (% and on power), what's in
    /// Notification Center, and a notification that just arrived.
    Weather(feeds::Weather),
    Device(Option<u32>, bool),
    Inbox(Vec<feeds::Note>),
    Popup(feeds::Note),
    /// A route being followed, as it changes; none when it ends.
    Nav(Option<nav::Route>),
}

/// A route to try navigation tiles with (the editor's button): the sample, walked to its turn a step every 2 s, then
/// the end.
pub(crate) async fn demo_route(tx: mpsc::UnboundedSender<Command>) {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut walked = 0.0;
    while walked <= 180.0 && tx.send(Command::Nav(Some(nav::sample(walked)))).is_ok() {
        tokio::time::sleep(DEMO_EVERY).await;
        walked += DEMO_STEP;
    }
    let _ = tx.send(Command::Nav(None));
    RUNNING.store(false, Ordering::SeqCst);
}

/// What the editor shows of the glasses.
#[derive(Clone, Default, serde::Serialize)]
pub(crate) struct Status {
    /// Each arm connected: its side, battery %, and whether it runs this layout.
    pub arms: Vec<(String, Option<u32>)>,
    pub note: String,
}

pub(crate) struct Shared {
    pub layout: Layout,
    pub status: Status,
}

/// `erisg1 shell say` and the like: one line to a running `erisg1 shell`.
pub async fn tell(line: &str) -> Result<(), Error> {
    let mut stream = UnixStream::connect(socket()).await.map_err(|e| format!("no `erisg1 shell` running ({e})"))?;
    stream.write_all(format!("{}\n", line.replace('\n', " ")).as_bytes()).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// `erisg1 shell preview`: the saved layout, or the default one.
pub async fn preview(out: &Path, default: bool) -> Result<(), Error> {
    let layout = if default { Layout::default() } else { current_layout()? };
    sim::preview(&layout, out)?;
    Ok(())
}

/// `erisg1 shell`: the Mac's side, until Ctrl-C. Without glasses (`offline`), only the editor.
pub async fn run(side: Option<Side>, port: u16, offline: bool) -> Result<(), Error> {
    let layout = current_layout()?;
    let shell = Shell::new(layout.clone()).map_err(|e| format!("{}: {e}", layout_path().display()))?;
    let shared = Arc::new(Mutex::new(Shared { layout, status: Status::default() }));
    let (tx, mut rx) = mpsc::unbounded_channel();

    let listener = TcpListenerBound::bind(port).await?;
    println!("the editor: http://127.0.0.1:{port}");
    tokio::spawn(editor::serve(listener.0, port, shared.clone(), tx.clone()));
    tokio::spawn(listen(tx.clone()));
    if !offline {
        feeds::watch(tx.clone());
    }
    println!("`erisg1 shell say TEXT` shows a notification. Ctrl-C leaves the shell running on the glasses");

    let mut host = Host::new(shell, side, shared.clone());
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    let mut next_connect = Instant::now();
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        if !offline && host.missing() && Instant::now() >= next_connect {
            if let Err(e) = host.connect().await {
                host.note(format!("glasses: {e}"));
            }
            next_connect = Instant::now() + CONNECT_EVERY;
        }
        tokio::select! {
            command = rx.recv() => match command {
                Some(command) => host.command(command).await,
                None => break,
            },
            _ = ticker.tick() => host.poll().await,
            _ = &mut stop => break,
        }
    }
    host.leave().await;
    let _ = std::fs::remove_file(socket());
    Ok(())
}

/// The editor's listening socket, bound before anything else so a second run fails at once.
struct TcpListenerBound(tokio::net::TcpListener);

impl TcpListenerBound {
    async fn bind(port: u16) -> Result<TcpListenerBound, Error> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("127.0.0.1:{port}: {e} (is another `erisg1 shell` running?)"))?;
        Ok(TcpListenerBound(listener))
    }
}

/// Lines on the socket: "say TEXT", "notify ICON TEXT", "text NAME TEXT", "show NAME [SECONDS]", "hide NAME".
async fn listen(tx: mpsc::UnboundedSender<Command>) {
    let path = socket();
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            return;
        }
    };
    while let Ok((stream, _)) = listener.accept().await {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(command) = parse(&line) {
                    let _ = tx.send(command);
                }
            }
        });
    }
}

fn parse(line: &str) -> Option<Command> {
    let (verb, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
    let (name, text) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
    match verb {
        "say" => Some(Command::Say(rest.trim().to_string(), None)),
        "notify" => Some(Command::Say(text.trim().to_string(), icons::category_named(name))),
        "text" => Some(Command::Text(name.to_string(), text.trim().to_string())),
        "show" => Some(Command::Show(name.to_string(), text.trim().parse().unwrap_or(0.0))),
        "hide" => Some(Command::Hide(name.to_string())),
        _ => None,
    }
}

struct Host {
    shell: Shell,
    side: Option<Side>,
    arms: Vec<Arm>,
    generation: u32,
    last_time: Instant,
    last_check: Instant,
    pending: Option<(String, Option<u8>, Instant)>,
    /// The Mac's latest: the weather, its battery, Notification Center.
    weather: Option<feeds::Weather>,
    device: Option<(Option<u32>, bool)>,
    inbox: Vec<feeds::Note>,
    /// The route being followed, whether distances are in metres, and the turn whose arrow the arms have.
    route: Option<nav::Route>,
    metric: bool,
    arrow: Option<nav::Turn>,
    shared: Arc<Mutex<Shared>>,
}

impl Host {
    fn new(shell: Shell, side: Option<Side>, shared: Arc<Mutex<Shared>>) -> Host {
        Host {
            shell,
            side,
            arms: Vec::new(),
            // Any value the arms haven't seen: they compare, never count.
            generation: host_us() | 1,
            last_time: Instant::now(),
            last_check: Instant::now(),
            pending: None,
            weather: None,
            device: None,
            inbox: Vec::new(),
            route: None,
            metric: feeds::metric(),
            arrow: None,
            shared,
        }
    }

    fn sides(&self) -> Vec<Side> {
        match self.side {
            Some(side) => vec![side],
            None => vec![Side::Left, Side::Right],
        }
    }

    fn missing(&self) -> bool {
        self.sides().iter().any(|s| !self.arms.iter().any(|a| a.side == *s))
    }

    fn note(&self, note: String) {
        println!("{note}");
        self.shared.lock().unwrap().status.note = note;
    }

    fn publish(&self) {
        let mut shared = self.shared.lock().unwrap();
        let mut arms: Vec<(String, Option<u32>)> =
            self.arms.iter().map(|a| (side_name(a.side).to_string(), None)).collect();
        for (name, battery) in &mut arms {
            *battery = shared.status.arms.iter().find(|(n, _)| n == name).and_then(|(_, b)| *b);
        }
        shared.status.arms = arms;
    }

    fn next_generation(&mut self) -> u32 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.generation
    }

    /// Connects to the arms not connected yet, and puts the shell on them or picks it up.
    async fn connect(&mut self) -> Result<(), Error> {
        let wanted: Vec<Side> = self.sides().into_iter().filter(|s| !self.arms.iter().any(|a| a.side == *s)).collect();
        let adapter = link::adapter().await?;
        let found = v5_link::scan(&adapter, Duration::from_secs(8), &wanted).await?;
        let mut new = Vec::new();
        for f in found.into_iter().filter(|f| wanted.contains(&f.side)) {
            if f.safe {
                self.note(format!("the {} arm is in safe mode: no drawing", side_name(f.side)));
                continue;
            }
            let side = f.side;
            match Arm::open(f).await {
                Ok(mut arm) => {
                    let _ = arm.ask_for_fast_link().await;
                    match arm.sync_clock().await {
                        Ok(_) => new.push(arm),
                        Err(e) => {
                            self.note(format!("the {} arm: {e}", side_name(side)));
                            arm.disconnect().await;
                        }
                    }
                }
                Err(e) => self.note(format!("the {} arm: {e}", side_name(side))),
            }
        }
        if new.is_empty() {
            return Err("no arm found (in range, paired, and not busy with another program?)".into());
        }
        // A new arm may have any arrow.
        self.arrow = None;
        let mut fresh = Vec::new();
        for mut arm in new {
            let touch = (1u16 << trigger::TOUCH).to_le_bytes();
            match self.running_here(&mut arm).await {
                Ok(true) => {
                    arm.control(v5::ctrl::EVENTS, &touch).await?;
                    self.note(format!("the {} arm: the shell is already running; picked it up", side_name(arm.side)));
                    self.arms.push(arm);
                }
                Ok(false) => fresh.push(arm),
                Err(e) => {
                    self.note(format!("the {} arm: {e}", side_name(arm.side)));
                    arm.disconnect().await;
                }
            }
        }
        if !fresh.is_empty() {
            if let Err(e) = self.install(&mut fresh).await {
                for arm in fresh {
                    arm.disconnect().await;
                }
                return Err(e);
            }
            self.arms.extend(fresh);
        }
        self.send_time().await?;
        self.feed_all().await?;
        self.publish();
        if let Some((text, icon, at)) = self.pending.take()
            && at.elapsed() < KEEP_PENDING
        {
            self.say(&text, icon).await?;
        }
        Ok(())
    }

    /// Whether the arm runs this very shell already.
    async fn running_here(&self, arm: &mut Arm) -> Result<bool, Error> {
        let mut body = program::SIGNATURE.to_le_bytes().to_vec();
        body.extend(16u32.to_le_bytes());
        let crc = arm.control(v5::ctrl::ARENA_CRC, &body).await?;
        let status = arm.control(v5::ctrl::VM_STATUS, &[]).await?;
        Ok(crc.get(4..20) == Some(&self.shell.signature(arm.side)[..]) && status.first() == Some(&1))
    }

    /// Stops whatever the arms run and starts the shell on them, together.
    async fn install(&mut self, arms: &mut [Arm]) -> Result<(), Error> {
        self.arrow = None;
        for arm in arms.iter_mut() {
            self.note(format!("the {} arm: putting the shell on", side_name(arm.side)));
            arm.control(v5::ctrl::VM_STOP, &[]).await?;
            arm.run(self.shell.prepare()).await?;
            for (at, asset) in self.shell.assets() {
                // Fonts and icons stay in the arena until a reboot: only what isn't there already goes.
                let mut body = at.to_le_bytes().to_vec();
                body.extend((asset.len() as u32).to_le_bytes());
                let there = arm.control(v5::ctrl::ARENA_CRC, &body).await?;
                if there.get(..4) != Some(&proto::crc32(&asset).to_le_bytes()[..]) {
                    arm.run(v5::upload(at, &asset, arm.max_code)).await?;
                }
            }
            arm.run(self.shell.load(arm.side, 0)).await?;
            // The accelerometer at its slowest for the tilt and the gyroscope off, whatever ran before left.
            arm.run([
                v5::set(prop::PEER_LINK, 1),
                v5::set(prop::AUTO_BRIGHTNESS, (arm.side == Side::Left) as u32),
                v5::set(prop::IMU_RATE, 13),
                v5::set(prop::GYRO_RATE, 0),
            ])
            .await?;
            arm.flush().await?;
        }
        // The time and dates in place before it starts, and both arms starting at one moment.
        let generation = self.next_generation();
        let (sod, tick_host, date) = next_tick();
        let start_host = host_us().wrapping_add(400_000);
        for arm in arms.iter_mut() {
            let mut code = vec![program::time(self.shell.map(), generation, sod, arm.arm_time(tick_host))];
            code.extend(self.dates(date));
            code.push(v5::write(self.shell.map().start, &arm.arm_time(start_host).to_le_bytes()));
            arm.run(code).await?;
            arm.flush().await?;
        }
        for arm in arms.iter_mut() {
            arm.control(v5::ctrl::VM_START, &program::TRIGGERS.to_le_bytes()).await?;
            arm.control(v5::ctrl::EVENTS, &(1u16 << trigger::TOUCH).to_le_bytes()).await?;
        }
        self.last_time = Instant::now();
        self.note(format!(
            "the shell runs on the {} arm{}",
            arms.iter().map(|a| side_name(a.side)).collect::<Vec<_>>().join(" and "),
            if arms.len() > 1 { "s" } else { "" }
        ));
        Ok(())
    }

    /// Host code for every date tile's dates from `date` on.
    fn dates(&self, date: chrono::NaiveDate) -> Vec<Vec<u8>> {
        let mut code = Vec::new();
        for (i, tile) in self.shell.layout.tiles.iter().enumerate() {
            if tile.kind == Kind::Date {
                let data = TileData { lines: layout::dates(tile.date_format(), date), ..TileData::default() };
                code.extend(program::content(&self.shell.map().tiles[i], &data));
            }
        }
        code
    }

    /// The time and dates to the running shells.
    async fn send_time(&mut self) -> Result<(), Error> {
        let generation = self.next_generation();
        let (sod, tick_host, date) = next_tick();
        let due = host_us().wrapping_add(TIME_AHEAD_US);
        for i in 0..self.arms.len() {
            let arm = &self.arms[i];
            let mut code = self.dates(date);
            code.push(program::time(self.shell.map(), generation, sod, arm.arm_time(tick_host)));
            code.extend(program::raise(self.shell.map(), arm.arm_time(due)));
            self.arms[i].run(code).await?;
        }
        self.last_time = Instant::now();
        Ok(())
    }

    fn find(&self, name: &str) -> Result<usize, String> {
        self.shell.layout.tiles.iter().position(|t| t.name == name).ok_or_else(|| {
            let names: Vec<&str> = self.shell.layout.tiles.iter().map(|t| t.name.as_str()).collect();
            format!("no tile {name:?}; there are {}", names.join(", "))
        })
    }

    /// Tile `i`: new content (or the old for `None`), and on, off or as it is (`WANT_*`), on for `seconds` (0:
    /// until turned off).
    async fn update(&mut self, i: usize, data: Option<&TileData>, want: u32, seconds: f64) -> Result<(), Error> {
        let generation = self.next_generation();
        let due = host_us().wrapping_add(AHEAD_US);
        let duration = (seconds * 1e6).round() as u32;
        for k in 0..self.arms.len() {
            let tile = &self.shell.map().tiles[i];
            let mut code = data.map(|d| program::content(tile, d)).unwrap_or_default();
            code.push(program::turn(tile, generation, want, duration));
            code.extend(program::raise(self.shell.map(), self.arms[k].arm_time(due)));
            self.arms[k].run(code).await?;
        }
        Ok(())
    }

    /// A notification in the first notification tile (or the first that mirrors the Mac's, for `mirrored`).
    async fn notify(&mut self, text: &str, icon: Option<u8>, mirrored: bool) -> Result<(), Error> {
        let tiles = self.shell.of(Kind::Notification);
        let Some(&i) = tiles.iter().find(|&&i| !mirrored || self.shell.layout.tiles[i].mirror) else {
            return if mirrored { Ok(()) } else { Err("the layout has no notification tile".into()) };
        };
        if self.arms.is_empty() {
            if !mirrored {
                self.pending = Some((text.to_string(), icon, Instant::now()));
                self.note("no glasses connected: it shows when they are".into());
            }
            return Ok(());
        }
        let data = self.shell.note(i, text, icon.unwrap_or(icons::category(None)));
        let seconds = self.shell.layout.tiles[i].timeout;
        self.update(i, Some(&data), WANT_ON, seconds).await
    }

    async fn say(&mut self, text: &str, icon: Option<u8>) -> Result<(), Error> {
        self.notify(text, icon, false).await
    }

    /// New content for every tile of `kind`, made by `make`; on or off as they are.
    async fn feed(&mut self, kind: Kind, make: impl Fn(&Shell, usize) -> TileData) -> Result<(), Error> {
        for i in self.shell.of(kind) {
            let data = make(&self.shell, i);
            self.update(i, Some(&data), WANT_KEEP, 0.0).await?;
        }
        Ok(())
    }

    async fn feed_weather(&mut self) -> Result<(), Error> {
        let Some(w) = self.weather.clone() else { return Ok(()) };
        let icon = icons::weather(w.code, w.day);
        self.feed(Kind::Weather, |shell, i| shell.weather(i, icon, w.celsius, w.humidity, w.fahrenheit)).await
    }

    async fn feed_device(&mut self) -> Result<(), Error> {
        let Some((percent, charging)) = self.device else { return Ok(()) };
        self.feed(Kind::DeviceBattery, |shell, _| shell.device(percent, charging)).await
    }

    async fn feed_inbox(&mut self) -> Result<(), Error> {
        let rows: Vec<(u8, String)> = self.inbox.iter().map(|n| (icons::category(Some(&n.app)), n.line())).collect();
        self.feed(Kind::NotificationList, |shell, i| shell.list(i, &rows)).await
    }

    /// The route to every navigation tile, and them on; or them off without one.
    async fn feed_nav(&mut self) -> Result<(), Error> {
        let Some(route) = self.route.clone() else {
            for i in self.shell.of(Kind::Navigation) {
                self.update(i, None, WANT_OFF, 0.0).await?;
            }
            return Ok(());
        };
        if self.arrow != Some(route.turn) {
            let code = self.shell.arrows(route.turn);
            for arm in &mut self.arms {
                arm.run(code.clone()).await?;
            }
            self.arrow = Some(route.turn);
        }
        for i in self.shell.of(Kind::Navigation) {
            let data = self.shell.navigation(i, &route, self.metric);
            self.update(i, Some(&data), WANT_ON, 0.0).await?;
        }
        Ok(())
    }

    /// Everything the Mac knows, to every tile that shows it; the steps and heart rate placeholders for now.
    async fn feed_all(&mut self) -> Result<(), Error> {
        self.feed_weather().await?;
        self.feed_device().await?;
        self.feed_inbox().await?;
        self.feed_nav().await?;
        self.feed(Kind::Steps, |shell, _| shell.steps(sim::SAMPLE_STEPS)).await?;
        self.feed(Kind::HeartRate, |shell, _| shell.heart(sim::SAMPLE_HEART)).await
    }

    async fn command(&mut self, command: Command) {
        let result: Result<(), Error> = match command {
            Command::Say(text, icon) => self.say(&text, icon).await,
            Command::Text(name, text) => match self.find(&name) {
                Ok(i) => {
                    let data = self.shell.note(i, &text, icons::NONE);
                    let want = if data.lines.is_empty() { WANT_OFF } else { WANT_ON };
                    let seconds = self.shell.layout.tiles[i].timeout;
                    self.update(i, Some(&data), want, seconds).await
                }
                Err(e) => Err(e.into()),
            },
            Command::Show(name, seconds) => match self.find(&name) {
                Ok(i) => self.update(i, None, WANT_ON, seconds).await,
                Err(e) => Err(e.into()),
            },
            Command::Hide(name) => match self.find(&name) {
                Ok(i) => self.update(i, None, WANT_OFF, 0.0).await,
                Err(e) => Err(e.into()),
            },
            Command::Apply(layout, reply) => {
                let result = self.apply(layout).await;
                let _ = reply.send(result.map_err(|e| e.to_string()));
                return;
            }
            Command::Weather(w) => {
                self.weather = Some(w);
                self.feed_weather().await
            }
            Command::Device(percent, charging) => {
                self.device = Some((percent, charging));
                self.feed_device().await
            }
            Command::Inbox(notes) => {
                self.inbox = notes;
                self.feed_inbox().await
            }
            Command::Popup(note) => {
                let icon = icons::category(Some(&note.app));
                self.notify(&note.line(), Some(icon), true).await
            }
            Command::Nav(route) => {
                self.route = route;
                self.feed_nav().await
            }
        };
        if let Err(e) = result {
            self.note(format!("{e}"));
            self.drop_lost().await;
        }
    }

    /// A new layout: checked, saved, and put on the connected arms.
    async fn apply(&mut self, layout: Layout) -> Result<String, Error> {
        let shell = Shell::new(layout)?;
        let layout = shell.layout.clone();
        layout.save(&layout_path())?;
        self.shell = shell;
        self.shared.lock().unwrap().layout = layout;
        if self.arms.is_empty() {
            return Ok("saved; it goes on the glasses when they connect".into());
        }
        let mut arms = std::mem::take(&mut self.arms);
        let mut result = self.install(&mut arms).await;
        self.arms = arms;
        if result.is_ok() {
            result = self.feed_all().await;
        }
        match result {
            Ok(()) => Ok("saved and running on the glasses".into()),
            Err(e) => {
                self.drop_lost().await;
                Err(e)
            }
        }
    }

    /// Takes in what the arms sent; now and then checks they're there, and sends the time again.
    async fn poll(&mut self) {
        let mut lost = false;
        for k in 0..self.arms.len() {
            let arm = &mut self.arms[k];
            if let Err(e) = arm.poll().await {
                println!("{e}");
                lost = true;
                continue;
            }
            while let Some(event) = arm.events.pop_front() {
                match event.kind {
                    // Both arms report a tile; the right's is enough.
                    v5::EventKind::Notify { a, b, .. } if arm.side == Side::Right || self.side.is_some() => {
                        let name = self.shell.layout.tiles.get(b as usize).map_or("?", |t| t.name.as_str());
                        let what = match a {
                            program::NOTE_EXPIRED => "its time was up",
                            program::NOTE_DISMISSED => "tapped away",
                            program::NOTE_RESTORED => "brought back",
                            _ => "?",
                        };
                        println!("{name}: {what}");
                    }
                    v5::EventKind::Input { trigger: trigger::TOUCH, value } => {
                        println!("{} TouchBar: {}", side_name(arm.side), v5::gesture_name(value));
                    }
                    v5::EventKind::Halt { address, code, .. } => {
                        println!(
                            "the {} arm's shell stopped at {address:#x}: {}",
                            side_name(arm.side),
                            v5::error_name(code)
                        );
                        lost = true;
                    }
                    _ => {}
                }
            }
        }
        if !lost && self.last_check.elapsed() >= CHECK_EVERY {
            self.last_check = Instant::now();
            let mut batteries = Vec::new();
            for arm in &mut self.arms {
                match arm.props(&[prop::BATT_PCT]).await {
                    Ok(v) => {
                        batteries.push((side_name(arm.side).to_string(), v.first().copied().filter(|&p| p <= 100)))
                    }
                    Err(e) => {
                        println!("{e}");
                        lost = true;
                    }
                }
            }
            self.shared.lock().unwrap().status.arms = batteries;
        }
        if !lost && !self.arms.is_empty() && self.last_time.elapsed() >= TIME_EVERY {
            for arm in &mut self.arms {
                let _ = arm.sync_clock().await;
            }
            if let Err(e) = self.send_time().await {
                println!("{e}");
                lost = true;
            }
        }
        if lost {
            self.drop_lost().await;
        }
    }

    /// Lets go of every arm, to connect again: one that answers nothing, restarted or stopped its shell is
    /// picked up again (or the shell put back) on the next connect.
    async fn drop_lost(&mut self) {
        for arm in self.arms.drain(..) {
            arm.disconnect().await;
        }
        self.publish();
    }

    /// Lets go of the glasses, the shell running on.
    async fn leave(&mut self) {
        for arm in self.arms.drain(..) {
            arm.disconnect().await;
        }
        println!("the shell goes on running on the glasses; `erisg1 v0 stop` stops it");
    }
}

/// The next whole second at least 0.3 s away: the second of the day shown until then, when it is (this program's
/// clock, `host_us`), and the date shown until then.
fn next_tick() -> (u32, u32, chrono::NaiveDate) {
    let (now, host) = (chrono::Local::now(), host_us());
    let mut to_next = 1_000_000 - (now.timestamp_subsec_micros() % 1_000_000);
    if to_next < 300_000 {
        to_next += 1_000_000;
    }
    let shown = now + chrono::Duration::microseconds(to_next as i64 - 1_000_000);
    (shown.num_seconds_from_midnight(), host.wrapping_add(to_next), shown.date_naive())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_socket_lines() {
        assert!(matches!(parse("say Build: done"), Some(Command::Say(t, None)) if t == "Build: done"));
        assert!(matches!(parse("notify mail Inbox: 3 new"), Some(Command::Say(t, Some(2))) if t == "Inbox: 3 new"));
        assert!(
            matches!(parse("text status Now playing"), Some(Command::Text(n, t)) if n == "status" && t == "Now playing")
        );
        assert!(matches!(parse("text status"), Some(Command::Text(n, t)) if n == "status" && t.is_empty()));
        assert!(matches!(parse("show battery 5"), Some(Command::Show(n, s)) if n == "battery" && s == 5.0));
        assert!(matches!(parse("hide battery"), Some(Command::Hide(n)) if n == "battery"));
        assert!(parse("dance").is_none());
    }

    #[test]
    fn the_next_tick_is_ahead() {
        let host = host_us();
        let (sod, tick, _) = next_tick();
        let ahead = tick.wrapping_sub(host);
        assert!((300_000..1_300_001).contains(&ahead), "{ahead}");
        assert!(sod < 86_400);
    }
}
