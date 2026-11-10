//! `erisg1`: custom software for Even Realities G1 glasses, talking to both arms over Bluetooth LE.

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

mod canvas;
mod dfu;
mod link;
mod mcuboot;
mod proto;
mod smp;
mod v0;
mod v0_flight;
mod v0_sensors;
mod v5;
mod v5_asm;
mod v5_link;

use canvas::Canvas;
use link::{Error, Glasses};
use proto::{Event, Side};

/// The glasses drop the link after about 30 seconds of silence.
const HEARTBEAT: Duration = Duration::from_secs(8);

/// How long to leave the glasses alone after changing a setting, so they save it. Firmware 1.66 waits for five quiet
/// seconds, but a height change only survived a restart after about ten.
const SETTINGS_SAVE: Duration = Duration::from_secs(12);

#[derive(Parser)]
#[command(version, about = "Custom software for Even Realities G1 glasses")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the G1 arms in range.
    Scan {
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// Show the display brightness, or set it.
    Brightness {
        /// 0 to 42. Leave out to only show the current setting.
        level: Option<u8>,
        /// Let the glasses adjust from `level` by ambient light.
        #[arg(long)]
        auto: bool,
        #[arg(long)]
        channel: Option<u32>,
    },
    /// Show how far you tilt your head up before the glasses count it as up, or set it.
    HeadUp {
        /// Degrees, 0 to 60. Leave out to only show the current setting.
        degrees: Option<u8>,
        #[arg(long)]
        channel: Option<u32>,
    },
    /// Time full-screen bitmaps with several waits before the end marker, to find the fastest that arrives whole.
    #[command(hide = true)]
    BitmapTest {
        /// Waits before the end marker to try, in milliseconds.
        #[arg(default_values_t = [0, 100, 300, 600])]
        settles_ms: Vec<u64>,
        #[arg(long, default_value_t = 30)]
        spacing_ms: u64,
        #[arg(long, default_value_t = 3)]
        rounds: u32,
    },
    /// Close whatever the glasses have open, ours or theirs, and say what was running before and after.
    Exit {
        #[arg(long)]
        channel: Option<u32>,
    },
    /// Show where the image sits, or move it.
    Display {
        /// 0 to 8. Leave out to only show the current setting.
        height: Option<u8>,
        /// How far away the image appears. Keeps the current depth when left out.
        depth: Option<u8>,
        #[arg(long)]
        channel: Option<u32>,
    },
    /// Restart both arms.
    Reboot {
        #[arg(long)]
        channel: Option<u32>,
    },
    /// A clock top left, the battery top right, and a frame counter bottom right, redrawn as fast as the glasses take
    /// it, until Ctrl-C.
    Hud {
        /// The least time between frames, in seconds. 0 sends them as fast as the glasses take them.
        #[arg(long, default_value_t = 0.0)]
        interval: f64,
        /// Send it as text, which the glasses redraw far faster than a bitmap but lay out themselves.
        #[arg(long)]
        text: bool,
        /// Characters across a line of text, for pushing things to the right edge.
        #[arg(long, default_value_t = 40)]
        columns: usize,
        /// Stream the glasses' own debug log into this program's output while it runs.
        #[arg(long)]
        firmware_log: bool,
        /// How big the image's text is: the 8×8 font drawn this many times over.
        #[arg(long, default_value_t = 3)]
        scale: usize,
        /// Draw only this many rows from the top, which is less to send. The rest of the screen keeps whatever was
        /// there.
        #[arg(long, default_value_t = proto::HEIGHT)]
        rows: usize,
        /// How long to wait after each image for it to reach the panel, in milliseconds.
        #[arg(long)]
        draw_ms: Option<u64>,
        /// How long to wait between an image's end marker and its CRC, in milliseconds.
        #[arg(long)]
        settle_ms: Option<u64>,
        /// Spaces before each line of text. The glasses' text area sits left of center, and this moves the text right
        /// within it.
        #[arg(long, default_value_t = 0)]
        indent: usize,
        #[arg(long)]
        channel: Option<u32>,
    },
    /// Turn the glasses' own debug log on or off. With `ERISG1_DEBUG` set, other commands print it as it arrives.
    FirmwareLog {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Send one of the firmware's `#` debug commands to both arms and print what comes back.
    #[command(hide = true)]
    Debug { command: String },
    /// Ask both arms everything they're known to answer without changing anything, then listen to their debug log
    /// while you move your head.
    Probe {
        /// How long to listen to the debug log.
        #[arg(long, default_value_t = 30)]
        listen: u64,
        #[arg(long)]
        channel: Option<u32>,
    },
    /// Commands for arms running the g1v0 firmware (scouter_kit).
    V0 {
        #[command(subcommand)]
        command: V0Command,
    },
    /// Show what's in a firmware update (an update .zip or an image .bin) and whether the glasses would take it.
    Inspect { file: std::path::PathBuf },
    /// Show which firmware each arm has installed, and what's waiting in its spare slot. Changes nothing.
    Images {
        /// Only this arm.
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
        #[arg(long)]
        channel: Option<u32>,
    },
    /// Upload the app core image of a firmware update (an update .zip or an app core .bin) to one arm's spare slot and
    /// check it, leaving the arm running what it did. With --install, then install it, which can't be undone.
    Flash {
        file: std::path::PathBuf,
        #[arg(long, value_parser = ["left", "right"])]
        arm: String,
        /// Install the uploaded image and restart into it. The old firmware is gone after this.
        #[arg(long)]
        install: bool,
        #[arg(long)]
        channel: Option<u32>,
    },
    /// Connect, show some text, and print what the glasses report until Ctrl-C.
    Connect {
        /// What to show: text (use \n for new lines; the glasses don't wrap), a 576×136 1-bit .bmp file, or
        /// @extents for a test pattern of the display's edges.
        #[arg(default_value = "Hello from ErisG1")]
        show: String,
        /// Show this instead while your head is down, and the first one only while it's up.
        #[arg(long)]
        down: Option<String>,
        /// Show it only while your head is up, and nothing while it's down.
        #[arg(long, conflicts_with = "down")]
        only_up: bool,
        /// Only connect to this pair, by the number in their names.
        #[arg(long)]
        channel: Option<u32>,
        /// Also print every notification as hex.
        #[arg(long)]
        raw: bool,
    },
}

#[derive(Subcommand)]
enum V0Command {
    /// List the arms running g1v0 in range.
    Scan,
    /// Show each arm's firmware, state, battery and sensors.
    Hello {
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// Draw a test pattern and read the panel back to check every pixel arrived.
    Test {
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// Show text (use \n for new lines).
    Text {
        text: String,
        /// How big: the 8×8 font drawn this many times over.
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=10))]
        scale: u8,
        #[arg(long, default_value_t = 40)]
        x: i32,
        #[arg(long, default_value_t = 40)]
        y: i32,
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// A clock, the battery, head pitch and roll and a frame counter, redrawn in place as fast as the arms take it.
    Hud {
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=10))]
        scale: u8,
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// Every sensor reading of both arms, the left arm's in a column on the left and the right's on the right, and a
    /// level line from the accelerometers between them, the same in both eyes, until Ctrl-C.
    Sensors {
        /// Levels (of 42) added to the brightness Even's light table gives.
        #[arg(long, default_value_t = 0)]
        boost: u8,
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// A mock flight HUD with your head as the controls: tilt it up to climb, bank it to turn. A pitch ladder,
    /// heading tape, speed, altitude and G, drawn by the arms themselves from about 110 bytes a frame.
    Flight {
        /// Levels (of 42) added to the brightness Even's light table gives.
        #[arg(long, default_value_t = 5)]
        boost: u8,
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// Stop any program running on the arms and switch the panel off.
    Stop {
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// Print head pitch and roll and the accelerometer, and with --gyro the gyroscope.
    Imu {
        #[arg(long, default_value_t = 10)]
        hz: u32,
        #[arg(long)]
        gyro: bool,
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// Print TouchBar, wear, case and battery events.
    Events {
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// Switch the panel off, on, or to standby (wakes in 4 ms instead of 71).
    Panel {
        #[arg(value_parser = ["off", "on", "standby"])]
        state: String,
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// Restart the arms.
    Reboot {
        /// Also clear the count of bad boots, which takes an arm out of safe mode (named G1v0S_L or G1v0S_R).
        #[arg(long)]
        clear: bool,
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
    /// Show the brightness, or set it.
    Brightness {
        /// 0 to 42.
        level: Option<u8>,
        /// Follow the light sensor (on the right arm; the left follows the right).
        #[arg(long)]
        auto: bool,
        #[arg(long, value_parser = ["left", "right"])]
        arm: Option<String>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Scan { seconds } => scan(Duration::from_secs(seconds)).await,
        Command::Brightness { level, auto, channel } => brightness(level, auto, channel).await,
        Command::HeadUp { degrees, channel } => head_up(degrees, channel).await,
        Command::BitmapTest { settles_ms, spacing_ms, rounds } => {
            bitmap_test(&settles_ms, Duration::from_millis(spacing_ms), rounds).await
        }
        Command::Exit { channel } => exit(channel).await,
        Command::Display { height, depth, channel } => display(height, depth, channel).await,
        Command::Reboot { channel } => reboot(channel).await,
        Command::Hud { interval, text, columns, firmware_log, scale, rows, draw_ms, settle_ms, indent, channel } => {
            hud(HudOptions {
                interval: Duration::from_secs_f64(interval),
                text,
                columns,
                indent,
                firmware_log,
                scale: scale.max(1),
                rows: rows.clamp(1, proto::HEIGHT),
                draw: draw_ms.map(Duration::from_millis),
                settle: settle_ms.map(Duration::from_millis),
                channel,
            })
            .await
        }
        Command::FirmwareLog { state } => firmware_log(state == "on").await,
        Command::Debug { command } => debug(&command).await,
        Command::Probe { listen, channel } => probe(Duration::from_secs(listen), channel).await,
        Command::V0 { command } => v0_command(command).await,
        Command::Inspect { file } => dfu::inspect(&file),
        Command::Images { arm, channel } => {
            let sides = match arm.as_deref() {
                Some(arm) => vec![side(arm)],
                None => vec![Side::Left, Side::Right],
            };
            dfu::images(&sides, channel).await
        }
        Command::Flash { file, arm, install, channel } => dfu::flash(&file, side(&arm), channel, install).await,
        Command::Connect { show, down, only_up, channel, raw } => {
            let up = Show::parse(&show);
            let down = match (down, only_up) {
                (Some(down), _) => Some(Show::parse(&down)),
                (None, true) => Some(Show::Nothing),
                (None, false) => None,
            };
            connect(up, down, channel, raw).await
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erisg1: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn v0_command(command: V0Command) -> Result<(), Error> {
    let pick = |arm: Option<String>| arm.as_deref().map(side);
    match command {
        V0Command::Scan => v0::scan().await,
        V0Command::Hello { arm } => v0::hello(pick(arm)).await,
        V0Command::Test { arm } => v0::test(pick(arm)).await,
        V0Command::Text { text, scale, x, y, arm } => v0::text(pick(arm), &text, scale, x, y).await,
        V0Command::Hud { scale, arm } => v0::hud(pick(arm), scale).await,
        V0Command::Sensors { boost, arm } => v0_sensors::sensors(pick(arm), boost).await,
        V0Command::Flight { boost, arm } => v0_flight::flight(pick(arm), boost).await,
        V0Command::Stop { arm } => v0_sensors::stop(pick(arm)).await,
        V0Command::Imu { hz, gyro, arm } => v0::imu(pick(arm), hz, gyro).await,
        V0Command::Events { arm } => v0::events(pick(arm)).await,
        V0Command::Panel { state, arm } => v0::panel(pick(arm), &state).await,
        V0Command::Reboot { clear, arm } => v0::reboot(pick(arm), clear).await,
        V0Command::Brightness { level, auto, arm } => v0::brightness(pick(arm), level, auto).await,
    }
}

fn side(arm: &str) -> Side {
    if arm == "left" { Side::Left } else { Side::Right }
}

async fn scan(wait: Duration) -> Result<(), Error> {
    let adapter = link::adapter().await?;
    let found = link::scan(&adapter, wait, None).await?;
    if found.is_empty() {
        println!("no G1 arms in range");
    }
    for f in found {
        println!("channel {:3} {}  {}  rssi {}", f.channel, f.side.letter(), f.name, f.rssi.unwrap_or(0));
    }
    Ok(())
}

enum Show {
    Text(String),
    Extents,
    File(std::path::PathBuf),
    Nothing,
}

impl Show {
    fn parse(arg: &str) -> Show {
        if arg == "@extents" {
            Show::Extents
        } else if arg.to_lowercase().ends_with(".bmp") {
            Show::File(arg.into())
        } else {
            Show::Text(arg.replace("\\n", "\n"))
        }
    }

    async fn on(&self, glasses: &mut Glasses) -> Result<(), Error> {
        match self {
            Show::Nothing => glasses.request_both(&proto::exit()).await.map(|_| ()),
            Show::Text(text) => glasses.show_text(text).await,
            Show::Extents => glasses.show_bitmap(&proto::bitmap(extents_pattern)).await,
            Show::File(path) => glasses.show_bitmap(&std::fs::read(path)?).await,
        }
    }

    fn describe(&self) -> String {
        match self {
            Show::Text(text) => format!("{text:?}"),
            Show::Extents => "the extents pattern".into(),
            Show::File(path) => path.display().to_string(),
            Show::Nothing => "nothing".into(),
        }
    }
}

async fn connect(up: Show, down: Option<Show>, channel: Option<u32>, raw: bool) -> Result<(), Error> {
    let adapter = link::adapter().await?;
    println!("connecting…");
    let mut glasses = Glasses::connect(&adapter, channel).await?;
    println!("connected to channel {}", glasses.channel);

    if let Some(reply) = glasses.request(Side::Left, &proto::firmware_get()).await? {
        println!("firmware: {}", proto::firmware_string(&reply).unwrap_or_else(|| proto::hex(&reply)));
    }
    let [left, right] = glasses.request_both(&proto::battery_get()).await?;
    let percent =
        |r: Option<Vec<u8>>| r.as_deref().and_then(proto::battery_percent).map_or("?".into(), |p| format!("{p}%"));
    println!("battery: L {}  R {}", percent(left), percent(right));

    // Until the glasses report otherwise, assume the head is down.
    match &down {
        Some(down) => {
            down.on(&mut glasses).await?;
            println!(
                "showing {} with your head up and {} with it down. Ctrl-C to quit.",
                up.describe(),
                down.describe()
            );
        }
        None => {
            up.on(&mut glasses).await?;
            println!("showing {}; tap, long-press, look up and down. Ctrl-C to quit.", up.describe());
        }
    }

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                // Sent without waiting so a slow reply never stalls events; the replies are skipped below.
                glasses.send(Side::Left, &proto::silent_mode_get()).await?;
                glasses.send(Side::Right, &proto::silent_mode_get()).await?;
            }
            message = glasses.next() => {
                let Some((side, data)) = message else { return Err("lost the connection".into()) };
                if raw {
                    println!("{} < {}", side.letter(), proto::hex(&data));
                }
                if let Some(firmware) = proto::firmware_string(&data) {
                    println!("firmware: {firmware}");
                }
                match proto::event(&data) {
                    Some(Event::Other(_)) | None => {}
                    Some(event) => {
                        println!("{}: {event:?}", side.letter());
                        let started = std::time::Instant::now();
                        let shown = match (&event, &down) {
                            (Event::HeadUp, Some(_)) => Some(up.on(&mut glasses).await?),
                            (Event::HeadDown, Some(down)) => Some(down.on(&mut glasses).await?),
                            _ => None,
                        };
                        if shown.is_some() {
                            println!("  redrawn in {} ms", started.elapsed().as_millis());
                        }
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    glasses.disconnect().await;
    Ok(())
}

/// A frame around the whole display, with ticks every 16 pixels (longer every 64) along each edge and a cross in the
/// middle, to see how much of the bitmap is visible.
fn extents_pattern(x: usize, y: usize) -> bool {
    let (w, h) = (proto::WIDTH, proto::HEIGHT);
    let tick =
        |along: usize, depth: usize| along.is_multiple_of(16) && depth < if along.is_multiple_of(64) { 12 } else { 5 };
    let frame = x == 0 || y == 0 || x == w - 1 || y == h - 1;
    let ticks = tick(x, y) || tick(x, h - 1 - y) || tick(y, x) || tick(y, w - 1 - x);
    let cross = (x == w / 2 && y.abs_diff(h / 2) <= 10) || (y == h / 2 && x.abs_diff(w / 2) <= 10);
    frame || ticks || cross
}

async fn brightness(level: Option<u8>, auto: bool, channel: Option<u32>) -> Result<(), Error> {
    if level.is_some_and(|l| l > proto::BRIGHTNESS_MAX) {
        return Err(format!("brightness runs from 0 to {}", proto::BRIGHTNESS_MAX).into());
    }
    let adapter = link::adapter().await?;
    let mut glasses = Glasses::connect(&adapter, channel).await?;
    if let Some(level) = level {
        let [left, right] = glasses.request_both(&proto::brightness_set(level, auto)).await?;
        for (side, reply) in [('L', left), ('R', right)] {
            if reply.as_deref().and_then(|r| r.get(1)) != Some(&proto::ACK) {
                eprintln!("{side}: brightness set got {:?}", reply.map(|r| proto::hex(&r)));
            }
        }
        println!("saving…");
        tokio::time::sleep(SETTINGS_SAVE).await;
    }
    // The left arm holds the setting the official app reads.
    let reply = glasses.request(Side::Left, &proto::brightness_get()).await?;
    match reply.as_deref().and_then(proto::brightness) {
        Some((level, auto)) => {
            println!("brightness {level}/{}{}", proto::BRIGHTNESS_MAX, if auto { ", auto" } else { "" })
        }
        None => println!("brightness: no answer"),
    }
    glasses.disconnect().await;
    Ok(())
}

async fn head_up(degrees: Option<u8>, channel: Option<u32>) -> Result<(), Error> {
    if degrees.is_some_and(|d| d > proto::HEAD_UP_ANGLE_MAX) {
        return Err(format!("the head-up angle runs from 0 to {}", proto::HEAD_UP_ANGLE_MAX).into());
    }
    let adapter = link::adapter().await?;
    let mut glasses = Glasses::connect(&adapter, channel).await?;
    if let Some(degrees) = degrees {
        let reply = glasses.request(Side::Right, &proto::head_up_angle_set(degrees)).await?;
        if reply.as_deref().and_then(|r| r.get(1)) != Some(&proto::ACK) {
            eprintln!("head-up angle set got {:?}", reply.map(|r| proto::hex(&r)));
        }
    }
    let reply = glasses.request(Side::Right, &proto::head_up_angle_get()).await?;
    match reply.as_deref().and_then(proto::head_up_angle) {
        Some(degrees) => println!("head up at {degrees}°"),
        None => println!("head-up angle: no answer"),
    }
    glasses.disconnect().await;
    Ok(())
}

async fn bitmap_test(settles_ms: &[u64], spacing: Duration, rounds: u32) -> Result<(), Error> {
    let adapter = link::adapter().await?;
    let mut glasses = Glasses::connect(&adapter, None).await?;
    // Alternate two images so every round really redraws.
    let images = [proto::bitmap(extents_pattern), proto::bitmap(|x, y| !extents_pattern(x, y) && (x + y) % 9 == 0)];
    for &ms in settles_ms {
        for round in 0..rounds {
            let started = std::time::Instant::now();
            let image = &images[round as usize % 2];
            match glasses.show_bitmap_paced(image, spacing, Duration::from_millis(ms), 3).await {
                Ok(redone) => println!("settle {ms:3} ms: {:5} ms, {redone} redone", started.elapsed().as_millis()),
                Err(e) => println!("settle {ms:3} ms: failed after {} ms: {e}", started.elapsed().as_millis()),
            }
            // Keep the link alive between rounds.
            glasses.send(Side::Left, &proto::silent_mode_get()).await?;
            glasses.send(Side::Right, &proto::silent_mode_get()).await?;
        }
    }
    glasses.disconnect().await;
    Ok(())
}

async fn exit(channel: Option<u32>) -> Result<(), Error> {
    let adapter = link::adapter().await?;
    let mut glasses = Glasses::connect(&adapter, channel).await?;
    let running = |r: [Option<Vec<u8>>; 2]| r.map(|r| r.map_or("no answer".into(), |r| dump(&r)));
    let [left, right] = running(glasses.request_both(&[0x39]).await?);
    println!("running before: L {left}  R {right}");
    let [left, right] = glasses.request_both(&proto::exit()).await?.map(|r| r.map_or("no answer".into(), |r| dump(&r)));
    println!("exit: L {left}  R {right}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let [left, right] = running(glasses.request_both(&[0x39]).await?);
    println!("running after: L {left}  R {right}");
    for (side, data) in gather(&mut glasses, Duration::from_secs(1)).await {
        println!("  {} < {}", side.letter(), dump(&data));
    }
    glasses.disconnect().await;
    Ok(())
}

async fn read_display(glasses: &mut Glasses) -> Result<(u8, u8), Error> {
    let reply = glasses.request(Side::Right, &proto::display_get()).await?;
    if std::env::var_os("ERISG1_DEBUG").is_some() {
        let left = glasses.request(Side::Left, &proto::display_get()).await?;
        eprintln!(
            "display get: L {}  R {}",
            left.as_deref().map_or("none".into(), dump),
            reply.as_deref().map_or("none".into(), dump)
        );
    }
    reply.as_deref().and_then(proto::display).ok_or_else(|| "the glasses didn't say where the display sits".into())
}

/// Moves the image on both arms. Returns false if either didn't acknowledge.
async fn move_display(glasses: &mut Glasses, seq: u8, preview: bool, height: u8, depth: u8) -> Result<bool, Error> {
    let [left, right] = glasses.request_both(&proto::display_set(seq, preview, height, depth)).await?;
    if std::env::var_os("ERISG1_DEBUG").is_some() {
        let show = |r: &Option<Vec<u8>>| r.as_deref().map_or("none".into(), dump);
        eprintln!("display set {height}: L {}  R {}", show(&left), show(&right));
    }
    Ok(left.is_some() && right.is_some())
}

async fn display(height: Option<u8>, depth: Option<u8>, channel: Option<u32>) -> Result<(), Error> {
    if height.is_some_and(|h| h > proto::DISPLAY_HEIGHT_MAX) {
        return Err(format!("height runs from 0 to {}", proto::DISPLAY_HEIGHT_MAX).into());
    }
    let adapter = link::adapter().await?;
    let mut glasses = Glasses::connect(&adapter, channel).await?;
    let (current_height, current_depth) = read_display(&mut glasses).await?;
    if let Some(height) = height {
        // As Gadgetbridge does it: a preview first, then the same position for good five seconds later.
        let depth = depth.unwrap_or(current_depth);
        let previewed = move_display(&mut glasses, 1, true, height, depth).await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        let kept = move_display(&mut glasses, 2, false, height, depth).await?;
        if !(previewed && kept) {
            eprintln!("an arm didn't acknowledge the move");
        }
        // The arms save settings to flash once nothing has touched their storage for five seconds; anything sent in
        // that time, or a restart, loses the change.
        println!("saving…");
        tokio::time::sleep(SETTINGS_SAVE).await;
    }
    let (height, depth) =
        if height.is_some() { read_display(&mut glasses).await? } else { (current_height, current_depth) };
    println!("height {height}/{}, depth {depth}", proto::DISPLAY_HEIGHT_MAX);
    glasses.disconnect().await;
    Ok(())
}

async fn reboot(channel: Option<u32>) -> Result<(), Error> {
    let adapter = link::adapter().await?;
    let glasses = Glasses::connect(&adapter, channel).await?;
    glasses.send(Side::Left, &proto::reboot()).await?;
    glasses.send(Side::Right, &proto::reboot()).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    glasses.disconnect().await;
    println!("restarting");
    Ok(())
}

const HUD_MARGIN: usize = 4;
/// How often the HUD asks for the battery, on top of the levels the glasses report themselves.
const HUD_BATTERY_EVERY: Duration = Duration::from_secs(60);

/// Lines of text on one screen.
const HUD_TEXT_LINES: usize = 5;

struct HudOptions {
    interval: Duration,
    text: bool,
    columns: usize,
    indent: usize,
    firmware_log: bool,
    scale: usize,
    rows: usize,
    draw: Option<Duration>,
    settle: Option<Duration>,
    channel: Option<u32>,
}

async fn hud(options: HudOptions) -> Result<(), Error> {
    let HudOptions { interval, text, columns, indent, firmware_log, scale, rows, draw, settle, channel } = options;
    let adapter = link::adapter().await?;
    let mut glasses = Glasses::connect(&adapter, channel).await?;
    println!("connected to channel {}; Ctrl-C to stop", glasses.channel);
    if let Some(draw) = draw {
        glasses.bitmap_draw = draw;
    }
    if let Some(settle) = settle {
        glasses.bitmap_settle = settle;
    }
    if firmware_log {
        glasses.send(Side::Left, DEBUG_LOG_ON).await?;
        glasses.send(Side::Right, DEBUG_LOG_ON).await?;
    }
    if !text && rows < proto::HEIGHT {
        // The rows below a short image keep what was there, so start from a blank screen.
        glasses.show_bitmap(&proto::bitmap(|_, _| false)).await?;
    }
    if text {
        // Text draws over a bitmap rather than replacing it.
        glasses.request_both(&proto::exit()).await?;
    }
    let mut battery: [Option<u8>; 2] = [None, None];
    let mut battery_asked: Option<std::time::Instant> = None;
    let mut frames = 0u64;
    let mut recent = std::collections::VecDeque::new();
    // Bitmap traffic doesn't count: without the heartbeat the glasses stop redrawing about 30 seconds in, while still
    // accepting every frame.
    let mut heartbeat = std::time::Instant::now();
    let mut last_sent: Option<std::time::Instant> = None;
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        if battery_asked.is_none_or(|t| t.elapsed() > HUD_BATTERY_EVERY) {
            let replies = glasses.request_both(&proto::battery_get()).await?;
            for (level, reply) in battery.iter_mut().zip(replies) {
                *level = reply.as_deref().and_then(proto::battery_percent).or(*level);
            }
            battery_asked = Some(std::time::Instant::now());
        }
        if heartbeat.elapsed() >= HEARTBEAT {
            glasses.send(Side::Left, &proto::silent_mode_get()).await?;
            glasses.send(Side::Right, &proto::silent_mode_get()).await?;
            heartbeat = std::time::Instant::now();
        }
        // Take in whatever the glasses reported since the last frame, without waiting.
        while let Ok(Some((side, data))) = tokio::time::timeout(Duration::ZERO, glasses.next()).await {
            if let Some(Event::Battery(level)) = proto::event(&data) {
                battery[side as usize] = Some(level);
            }
            if data.first() != Some(&proto::SILENT_MODE_GET) {
                link::trace(format_args!("{} < {}", side.letter(), dump(&data)));
            }
        }

        frames += 1;
        let now = std::time::Instant::now();
        recent.push_back(now);
        while recent.len() > 10 {
            recent.pop_front();
        }
        let rate = match (recent.front(), recent.len()) {
            (Some(first), n) if n > 1 => (n - 1) as f64 / now.duration_since(*first).as_secs_f64(),
            _ => 0.0,
        };

        let mut canvas = Canvas::with_rows(rows);
        let line = 8 * scale;
        canvas.text(HUD_MARGIN, HUD_MARGIN, scale, &chrono::Local::now().format("%H:%M:%S").to_string());
        let percent = |level: Option<u8>| level.map_or("--".into(), |l| l.to_string());
        let power = format!("L{}% R{}%", percent(battery[0]), percent(battery[1]));
        canvas.text(proto::WIDTH - HUD_MARGIN - Canvas::text_width(&power, scale), HUD_MARGIN, scale, &power);
        let counter = format!("#{frames} {rate:.1}/s");
        let x = proto::WIDTH - HUD_MARGIN - Canvas::text_width(&counter, scale);
        canvas.text(x, canvas.rows().saturating_sub(HUD_MARGIN + line), scale, &counter);

        if let Some(last) = last_sent {
            tokio::time::sleep_until((last + interval).into()).await;
        }
        let sending = std::time::Instant::now();
        last_sent = Some(sending);
        let clock = chrono::Local::now().format("%H:%M:%S").to_string();
        let pad = |left: &str, right: &str| {
            let gap = columns.saturating_sub(indent + left.chars().count() + right.chars().count()).max(1);
            // The glasses drop plain spaces at the start of a line, so the indent uses full-width ones.
            format!("{}{left}{}{right}", "\u{3000}".repeat(indent), " ".repeat(gap))
        };
        let screen = format!("{}{}{}", pad(&clock, &power), "\n".repeat(HUD_TEXT_LINES - 1), pad("", &counter));
        let frame = canvas.bmp();
        let show = async { if text { glasses.show_text(&screen).await } else { glasses.show_bitmap(&frame).await } };
        tokio::select! {
            _ = &mut stop => break,
            shown = show => {
                let took = sending.elapsed().as_millis();
                match shown {
                    Ok(()) => link::trace(format_args!("frame {frames}: {took} ms")),
                    Err(e) => eprintln!("frame {frames} didn't arrive after {took} ms: {e}"),
                }
            }
        }
    }
    if firmware_log {
        glasses.send(Side::Left, DEBUG_LOG_OFF).await?;
        glasses.send(Side::Right, DEBUG_LOG_OFF).await?;
    }
    glasses.disconnect().await;
    Ok(())
}

async fn firmware_log(on: bool) -> Result<(), Error> {
    let adapter = link::adapter().await?;
    let glasses = Glasses::connect(&adapter, None).await?;
    let command = if on { DEBUG_LOG_ON } else { DEBUG_LOG_OFF };
    glasses.send(Side::Left, command).await?;
    glasses.send(Side::Right, command).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    glasses.disconnect().await;
    println!("firmware log {}", if on { "on" } else { "off" });
    Ok(())
}

async fn debug(command: &str) -> Result<(), Error> {
    let adapter = link::adapter().await?;
    let mut glasses = Glasses::connect(&adapter, None).await?;
    gather(&mut glasses, Duration::from_millis(500)).await;
    for side in [Side::Left, Side::Right] {
        glasses.send(side, command.as_bytes()).await?;
    }
    for (side, data) in gather(&mut glasses, Duration::from_secs(2)).await {
        println!("{} < {}", side.letter(), dump(&data));
    }
    glasses.disconnect().await;
    Ok(())
}

/// Commands Gadgetbridge only ever uses to read state, with the names it gives them.
const READS: &[(&str, &[u8])] = &[
    ("status", &[0x22]),
    ("brightness", &[0x29]),
    ("anti-shake", &[0x2A]),
    ("silent mode", &[0x2B]),
    ("battery and firmware", &[0x2C, 0x01]),
    ("MAC address", &[0x2D]),
    ("notification app list", &[0x2E]),
    ("head-up angle", &[0x32]),
    ("lens serial", &[0x33]),
    ("glasses serial", &[0x34]),
    ("ESB channel", &[0x35]),
    ("ESB notification count", &[0x36]),
    ("time since boot", &[0x37]),
    ("Apple notifications", &[0x38]),
    ("running app", &[0x39]),
    ("wear detection", &[0x3A]),
    ("display settings", &[0x3B]),
    ("notification auto display", &[0x3C]),
    ("buried point (telemetry)", &[0x3E]),
    ("hardware", &[0x3F]),
    ("firmware build", &[0x23, 0x74]),
];

const DEBUG_LOG_ON: &[u8] = &[0x23, 0x6C, 0x00];
const DEBUG_LOG_OFF: &[u8] = &[0x23, 0x6C, 0x31];

/// Hex, then the printable bytes as text, with trailing zero padding dropped.
fn dump(data: &[u8]) -> String {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1).max(2).min(data.len());
    let data = &data[..end];
    let text: String = data.iter().map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' }).collect();
    format!("{}  |{text}|", proto::hex(data))
}

/// Everything the glasses send for `wait`.
async fn gather(glasses: &mut Glasses, wait: Duration) -> Vec<(Side, Vec<u8>)> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut got = Vec::new();
    while let Ok(Some(message)) = tokio::time::timeout_at(deadline, glasses.next()).await {
        got.push(message);
    }
    got
}

async fn probe(listen: Duration, channel: Option<u32>) -> Result<(), Error> {
    let adapter = link::adapter().await?;
    let mut glasses = Glasses::connect(&adapter, channel).await?;
    println!("connected to channel {}", glasses.channel);
    gather(&mut glasses, Duration::from_secs(1)).await;

    for (name, command) in READS {
        println!("\n{name}  > {}", proto::hex(command));
        for side in [Side::Left, Side::Right] {
            glasses.send(side, command).await?;
            for (from, data) in gather(&mut glasses, Duration::from_millis(700)).await {
                println!("  {} < {}", from.letter(), dump(&data));
            }
        }
    }

    println!("\ndebug log on for {}s: move your head around, tilt, turn, nod", listen.as_secs());
    glasses.send(Side::Left, DEBUG_LOG_ON).await?;
    glasses.send(Side::Right, DEBUG_LOG_ON).await?;
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let deadline = tokio::time::Instant::now() + listen;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = heartbeat.tick() => {
                glasses.send(Side::Left, &proto::silent_mode_get()).await?;
                glasses.send(Side::Right, &proto::silent_mode_get()).await?;
            }
            message = glasses.next() => {
                let Some((side, data)) = message else { return Err("lost the connection".into()) };
                if data.first() != Some(&proto::SILENT_MODE_GET) {
                    println!("  {} < {}", side.letter(), dump(&data));
                }
            }
        }
    }
    glasses.send(Side::Left, DEBUG_LOG_OFF).await?;
    glasses.send(Side::Right, DEBUG_LOG_OFF).await?;
    println!("debug log off");
    gather(&mut glasses, Duration::from_secs(1)).await;
    glasses.disconnect().await;
    Ok(())
}
