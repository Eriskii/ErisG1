//! A live connection to both arms of a pair of G1s.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use btleplug::api::{Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout_at};

use crate::proto::{self, Side};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// The gap between bitmap packets, as in the official app. Any gap from 0 to 70 ms takes about as long, since the
/// link is the limit. (Writes with a response corrupt bitmap packets.)
const BITMAP_SPACING: Duration = Duration::from_millis(8);
/// The wait between the end marker's reply and the CRC. The glasses acknowledge the end before they've stored the
/// last few packets, and a CRC sent straight away is checked against an image with the old tail. Their log shows the
/// store finishing within about 15 ms of the end marker.
const BITMAP_SETTLE: Duration = Duration::from_millis(100);
/// The wait after the CRCs for the arms to redraw. The first packet of the next image resets their display state, so
/// starting it sooner skips the frame: 400 ms kept both eyes on every frame, 200 ms didn't.
pub const BITMAP_DRAW: Duration = Duration::from_millis(400);
const BITMAP_ATTEMPTS: u32 = 3;

pub struct Found {
    pub channel: u32,
    pub side: Side,
    pub name: String,
    pub rssi: Option<i16>,
    peripheral: Peripheral,
}

pub async fn adapter() -> Result<Adapter, Error> {
    let manager = Manager::new().await?;
    manager.adapters().await?.into_iter().next().ok_or_else(|| "no Bluetooth adapter".into())
}

/// Scans for G1 arms for up to `wait`, stopping early once both arms of one pair (of `channel`, if given) are found.
pub async fn scan(adapter: &Adapter, wait: Duration, channel: Option<u32>) -> Result<Vec<Found>, Error> {
    let mut events = adapter.events().await?;
    adapter.start_scan(ScanFilter::default()).await?;
    let deadline = Instant::now() + wait;
    let mut found: HashMap<(u32, Side), Found> = HashMap::new();
    while let Ok(Some(event)) = timeout_at(deadline, events.next()).await {
        let (CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id)) = event else { continue };
        let peripheral = adapter.peripheral(&id).await?;
        let Some(props) = peripheral.properties().await? else { continue };
        let Some(name) = props.local_name.or(props.advertisement_name) else { continue };
        let Some((ch, side)) = proto::parse_name(&name) else { continue };
        if channel.is_some_and(|c| c != ch) {
            continue;
        }
        found.insert((ch, side), Found { channel: ch, side, name, rssi: props.rssi, peripheral });
        if found.contains_key(&(ch, Side::Left)) && found.contains_key(&(ch, Side::Right)) {
            break;
        }
    }
    adapter.stop_scan().await?;
    let mut found: Vec<Found> = found.into_values().collect();
    found.sort_by_key(|f| (f.channel, f.side.letter()));
    Ok(found)
}

struct Arm {
    peripheral: Peripheral,
    tx: Characteristic,
}

pub struct Glasses {
    pub channel: u32,
    left: Arm,
    right: Arm,
    incoming: mpsc::UnboundedReceiver<(Side, Vec<u8>)>,
    /// Notifications that arrived while waiting for a reply to something else.
    backlog: VecDeque<(Side, Vec<u8>)>,
    text_seq: u8,
    /// How long `show_bitmap` waits for the image to reach the panel before returning.
    pub bitmap_draw: Duration,
    /// How long `show_bitmap` waits between the end marker's reply and the CRCs.
    pub bitmap_settle: Duration,
}

impl Glasses {
    /// Connects to both arms of the first complete pair found, or of `channel`.
    pub async fn connect(adapter: &Adapter, channel: Option<u32>) -> Result<Glasses, Error> {
        let found = scan(adapter, Duration::from_secs(20), channel).await?;
        let pair = found.iter().map(|f| f.channel).find(|ch| found.iter().filter(|f| f.channel == *ch).count() == 2);
        let Some(channel) = pair else {
            return Err("didn't find both arms of a pair of G1s".into());
        };
        let take = |side| found.iter().find(|f| f.channel == channel && f.side == side).unwrap().peripheral.clone();
        let (sender, incoming) = mpsc::unbounded_channel();
        let left = open(take(Side::Left), Side::Left, sender.clone()).await?;
        let right = open(take(Side::Right), Side::Right, sender).await?;
        let mut glasses = Glasses {
            channel,
            left,
            right,
            incoming,
            backlog: VecDeque::new(),
            text_seq: 0,
            bitmap_draw: BITMAP_DRAW,
            bitmap_settle: BITMAP_SETTLE,
        };
        // Clear anything a killed upload left behind: the end marker resets the packet counter, and a CRC, even a
        // wrong one, makes the arm drop its half-stored file.
        for packet in [proto::bitmap_reset_counter(), proto::bitmap_reset_file()] {
            for side in [Side::Left, Side::Right] {
                glasses.send(side, &packet).await?;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        for side in [Side::Left, Side::Right] {
            if glasses.arm(side).peripheral.mtu() == 498 {
                glasses.send(side, &proto::normal_mode()).await?;
            }
            let reply = glasses.request(side, &proto::init()).await?;
            if reply.as_deref().and_then(|r| r.get(1)) != Some(&proto::ACK) {
                eprintln!("{}: init got {:?}", side.letter(), reply.map(|r| proto::hex(&r)));
            }
        }
        Ok(glasses)
    }

    fn arm(&self, side: Side) -> &Arm {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    pub async fn send(&self, side: Side, packet: &[u8]) -> Result<(), Error> {
        let arm = self.arm(side);
        arm.peripheral.write(&arm.tx, packet, WriteType::WithoutResponse).await?;
        Ok(())
    }

    /// Sends `packet` to one arm and waits for the reply that echoes its command byte. None if it doesn't come.
    pub async fn request(&mut self, side: Side, packet: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.request_each(&[side], packet).await?.pop().flatten())
    }

    /// Sends `packet` to each of `sides` without waiting in between, then waits for each one's reply, in the same
    /// order as `sides`.
    pub async fn request_each(&mut self, sides: &[Side], packet: &[u8]) -> Result<Vec<Option<Vec<u8>>>, Error> {
        // A late or doubled reply to an earlier request would otherwise answer this one.
        while let Ok(message) = self.incoming.try_recv() {
            self.backlog.push_back(message);
        }
        self.backlog.retain(|(from, data)| !(sides.contains(from) && data.first() == packet.first()));
        for &side in sides {
            self.send(side, packet).await?;
        }
        let mut replies: Vec<Option<Vec<u8>>> = vec![None; sides.len()];
        let deadline = Instant::now() + REPLY_TIMEOUT;
        while replies.iter().any(Option::is_none) {
            match timeout_at(deadline, self.incoming.recv()).await {
                Ok(Some((from, data))) => {
                    let waiting = sides.iter().position(|&s| s == from).filter(|&i| replies[i].is_none());
                    match waiting {
                        Some(i) if data.first() == packet.first() => replies[i] = Some(data),
                        _ => self.backlog.push_back((from, data)),
                    }
                }
                Ok(None) => return Err("lost the connection".into()),
                Err(_) => break,
            }
        }
        Ok(replies)
    }

    /// Sends `packet` to the left arm, then to the right once the left has replied, as the official app does.
    pub async fn request_both(&mut self, packet: &[u8]) -> Result<[Option<Vec<u8>>; 2], Error> {
        let left = self.request(Side::Left, packet).await?;
        let right = self.request(Side::Right, packet).await?;
        Ok([left, right])
    }

    pub async fn show_text(&mut self, text: &str) -> Result<(), Error> {
        let seq = self.text_seq;
        self.text_seq = self.text_seq.wrapping_add(1);
        // Both arms at once, so neither eye lags the other by a round trip.
        for packet in proto::text(seq, text) {
            self.request_each(&[Side::Left, Side::Right], &packet).await?;
        }
        Ok(())
    }

    /// Puts a BMP from `proto::bitmap` on the display.
    pub async fn show_bitmap(&mut self, bmp: &[u8]) -> Result<(), Error> {
        self.show_bitmap_paced(bmp, BITMAP_SPACING, self.bitmap_settle, BITMAP_ATTEMPTS).await.map(|_| ())
    }

    /// `show_bitmap` with `spacing` between packets and `settle` before the end marker. Returns how many arm uploads
    /// had to be redone.
    pub async fn show_bitmap_paced(
        &mut self,
        bmp: &[u8],
        spacing: Duration,
        settle: Duration,
        attempts: u32,
    ) -> Result<usize, Error> {
        let (packets, end) = proto::bitmap_packets(bmp);
        let mut redone = 0;
        let mut pending = vec![Side::Left, Side::Right];
        for attempt in 1..=attempts {
            // Both arms take each packet in turn, so they finish together. Nothing else may be sent mid-upload.
            for packet in &packets {
                for &side in &pending {
                    self.send(side, packet).await?;
                }
                tokio::time::sleep(spacing).await;
            }
            trace(format_args!("bitmap packets handed to macOS"));
            let failed = self.finish_bitmap(&pending, &end, bmp, settle).await?;
            if !failed.is_empty() {
                let report = failed.iter().map(|(side, why)| format!("{}: {why}", side.letter())).collect::<Vec<_>>();
                if attempt == attempts {
                    return Err(report.join("; ").into());
                }
                eprintln!("{}; sending again", report.join("; "));
                redone += failed.len();
            }
            let failed: Vec<Side> = failed.into_iter().map(|(side, _)| side).collect();
            pending = failed;
            if pending.is_empty() {
                break;
            }
        }
        Ok(redone)
    }

    /// Ends a bitmap upload to `sides` at once and checks it arrived whole. Returns the arms it didn't, and why.
    async fn finish_bitmap(
        &mut self,
        sides: &[Side],
        end: &[u8],
        bmp: &[u8],
        settle: Duration,
    ) -> Result<Vec<(Side, String)>, Error> {
        let mut failed = Vec::new();
        let ended = self.request_each(sides, end).await?;
        trace(format_args!("bitmap end acknowledged"));
        let mut ok = Vec::new();
        for (&side, reply) in sides.iter().zip(ended) {
            match reply.as_deref().and_then(|r| r.get(1)) {
                Some(&proto::ACK) => ok.push(side),
                _ => failed.push((side, format!("bitmap end got {:?}", reply.map(|r| proto::hex(&r))))),
            }
        }
        if ok.is_empty() {
            return Ok(failed);
        }
        tokio::time::sleep(settle).await;
        // Both arms at once: each redraws as its own CRC lands, so any gap between them shows as one eye lagging.
        let checked = self.request_each(&ok, &proto::bitmap_crc(bmp)).await?;
        trace(format_args!("bitmap CRCs answered"));
        for (&side, reply) in ok.iter().zip(checked) {
            if reply.as_deref().and_then(|r| r.get(5)) != Some(&proto::ACK) {
                failed.push((side, format!("bitmap CRC got {:?}", reply.map(|r| proto::hex(&r)))));
            }
        }
        tokio::time::sleep(self.bitmap_draw).await;
        Ok(failed)
    }

    /// The next notification nobody was waiting for.
    pub async fn next(&mut self) -> Option<(Side, Vec<u8>)> {
        match self.backlog.pop_front() {
            Some(queued) => Some(queued),
            None => self.incoming.recv().await,
        }
    }

    pub async fn disconnect(self) {
        let _ = self.left.peripheral.disconnect().await;
        let _ = self.right.peripheral.disconnect().await;
    }
}

fn stamp() -> String {
    chrono::Local::now().format("%H:%M:%S%.3f").to_string()
}

/// Prints `what`, timestamped, when `ERISG1_DEBUG` is set.
pub fn trace(what: std::fmt::Arguments) {
    if std::env::var_os("ERISG1_DEBUG").is_some() {
        eprintln!("{} {what}", stamp());
    }
}

/// Connects to one arm, finds its UART characteristics and forwards its notifications into `sender`.
async fn open(
    peripheral: Peripheral,
    side: Side,
    sender: mpsc::UnboundedSender<(Side, Vec<u8>)>,
) -> Result<Arm, Error> {
    let step = |what: &str| trace(format_args!("{}: {what}", side.letter()));
    step("connecting");
    peripheral.connect().await?;
    step("discovering services");
    peripheral.discover_services().await?;
    let characteristics = peripheral.characteristics();
    let find = |uuid| characteristics.iter().find(|c| c.uuid == uuid).cloned();
    let (Some(tx), Some(rx)) = (find(proto::TX), find(proto::RX)) else {
        return Err(format!("{} arm has no UART service", side.letter()).into());
    };
    step("subscribing");
    peripheral.subscribe(&rx).await?;
    trace(format_args!("{}: ready, mtu {}", side.letter(), peripheral.mtu()));
    let mut notifications = peripheral.notifications().await?;
    let print_log = std::env::var_os("ERISG1_DEBUG").is_some();
    tokio::spawn(async move {
        while let Some(n) = notifications.next().await {
            // The firmware's debug log, printed as it arrives so it lines up with what we send.
            if print_log && n.value.first() == Some(&0xF4) {
                let line = String::from_utf8_lossy(&n.value[1..]).trim_end_matches(['\0', '\n', '\r']).to_string();
                eprintln!("{} {} log: {line}", stamp(), side.letter());
                continue;
            }
            if n.uuid == proto::RX && sender.send((side, n.value)).is_err() {
                break;
            }
        }
    });
    Ok(Arm { peripheral, tx })
}
