//! Updating one arm's app core firmware over Bluetooth, the way Even's app does it: upload the image into the arm's
//! spare slot, check what landed there, then mark it for install and restart. Until that last step the arm keeps
//! running what it had, and an interrupted upload picks up where it stopped.
//!
//! The G1's bootloader overwrites the running image with the new one and has no way back, so an image that doesn't
//! bring Bluetooth up again leaves the arm reachable only by opening it up. Net core images, which carry the Bluetooth
//! radio itself, are never sent.

use std::time::Duration;

use btleplug::api::{Central, CentralEvent, Characteristic, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Peripheral};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout_at};
use uuid::Uuid;

use crate::link::{self, Error};
use crate::mcuboot::{self, Core, Image};
use crate::proto::{self, Side};
use crate::smp::{self, Slot};
use crate::v5;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Writing to the arm's external flash can hold an upload reply up for a while.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const HEARTBEAT: Duration = Duration::from_secs(8);
/// Reconnections an upload may need before giving up.
const UPLOAD_DROPS: u32 = 10;
/// How long the bootloader may take to copy the new image in and the arm to come back. About half a minute in
/// practice.
const INSTALL_WAIT: Duration = Duration::from_secs(300);
/// Below this, installing waits: a power cut while the bootloader copies is the one way a good image can brick an arm.
const INSTALL_BATTERY: u8 = 50;

/// Which firmware an arm runs, and its own channel for the little this needs besides updates.
enum Firmware {
    /// Even's: its command channel, for the heartbeat, the battery and leaving update mode.
    Even { tx: Characteristic },
    /// g1v0: its control channel, for the battery.
    G1v0 { ctrl: Characteristic },
}

/// Finds the arm on `side`, running Even's firmware (of the pair on `channel`, if given) or g1v0. Returns it and, for
/// Even's firmware, its pair's channel.
async fn find_arm(
    adapter: &Adapter,
    side: Side,
    channel: Option<u32>,
    wait: Duration,
) -> Result<(Peripheral, Option<u32>), Error> {
    let mut events = adapter.events().await?;
    adapter.start_scan(ScanFilter::default()).await?;
    let started = Instant::now();
    let deadline = started + wait;
    let mut channels = Vec::new();
    let mut found = None;
    while let Ok(Some(event)) = timeout_at(deadline, events.next()).await {
        let (CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id)) = event else { continue };
        let peripheral = adapter.peripheral(&id).await?;
        let Some(props) = peripheral.properties().await? else { continue };
        let Some(name) = props.local_name.or(props.advertisement_name) else { continue };
        link::trace(format_args!("seen {name}"));
        if let Some((ch, s)) = proto::parse_name(&name).filter(|&(ch, _)| channel.is_none_or(|c| c == ch)) {
            if !channels.contains(&ch) {
                channels.push(ch);
            }
            if s == side {
                found = Some((peripheral, Some(ch)));
            }
        } else if v5::parse_name(&name).is_some_and(|(s, _)| s == side) {
            found = Some((peripheral, None));
        }
        // A few seconds for any other pair in range to show up before settling on this one.
        if found.is_some() && started.elapsed() >= Duration::from_secs(3) {
            break;
        }
    }
    adapter.stop_scan().await?;
    if channels.len() > 1 {
        return Err(format!("several pairs in range ({channels:?}); pick one with --channel").into());
    }
    found.ok_or_else(|| {
        format!(
            "can't find the {} arm. It only talks to this Mac in the charging case with the lid open, and not while a \
             phone is connected to it",
            side_name(side)
        )
        .into()
    })
}

/// One arm, connected for an update.
struct Arm {
    side: Side,
    channel: Option<u32>,
    peripheral: Peripheral,
    smp: Characteristic,
    firmware: Firmware,
    /// Bytes per write: the ATT MTU less the 3-byte write header.
    fragment: usize,
    incoming: mpsc::UnboundedReceiver<(Uuid, Vec<u8>)>,
    reassembler: smp::Reassembler,
    seq: u8,
    last_heartbeat: Instant,
}

impl Arm {
    /// Finds the arm on `side` of the pair on `channel` (or the only pair in range) and connects to it.
    async fn connect(adapter: &Adapter, side: Side, channel: Option<u32>, wait: Duration) -> Result<Arm, Error> {
        let (peripheral, channel) = find_arm(adapter, side, channel, wait).await?;
        peripheral.connect().await?;
        peripheral.discover_services().await?;
        let characteristics = peripheral.characteristics();
        let find = |uuid| characteristics.iter().find(|c| c.uuid == uuid).cloned();
        let Some(smp) = find(smp::CHARACTERISTIC) else {
            let _ = peripheral.disconnect().await;
            return Err(format!("the {} arm has no firmware update service", side_name(side)).into());
        };
        peripheral.subscribe(&smp).await?;
        let firmware = match (find(proto::TX), find(proto::RX), find(v5::CTRL), find(v5::EVT)) {
            (Some(tx), Some(rx), _, _) => {
                peripheral.subscribe(&rx).await?;
                Firmware::Even { tx }
            }
            (_, _, Some(ctrl), Some(evt)) => {
                peripheral.subscribe(&evt).await?;
                Firmware::G1v0 { ctrl }
            }
            _ => {
                let _ = peripheral.disconnect().await;
                return Err(format!("the {} arm runs firmware this doesn't know", side_name(side)).into());
            }
        };
        let (sender, incoming) = mpsc::unbounded_channel();
        let mut notifications = peripheral.notifications().await?;
        tokio::spawn(async move {
            while let Some(n) = notifications.next().await {
                if sender.send((n.uuid, n.value)).is_err() {
                    break;
                }
            }
        });
        let fragment = (peripheral.mtu() as usize).saturating_sub(3).max(20);
        link::trace(format_args!("{}: connected, mtu {}", side.letter(), peripheral.mtu()));
        Ok(Arm {
            side,
            channel,
            peripheral,
            smp,
            firmware,
            fragment,
            incoming,
            reassembler: smp::Reassembler::default(),
            seq: 0,
            last_heartbeat: Instant::now(),
        })
    }

    /// Sends `request` and waits for its response, which may carry a refusal (see `smp::Response::error`). An error
    /// here means the link failed or the arm didn't answer.
    async fn request(&mut self, request: &smp::Request, wait: Duration) -> Result<smp::Response, Error> {
        self.seq = self.seq.wrapping_add(1);
        let frame = request.frame(self.seq);
        self.reassembler.clear();
        while self.incoming.try_recv().is_ok() {}
        // The firmware puts a request back together from several writes (CONFIG_MCUMGR_TRANSPORT_BT_REASSEMBLY).
        for piece in frame.chunks(self.fragment) {
            self.peripheral.write(&self.smp, piece, WriteType::WithoutResponse).await?;
        }
        let deadline = Instant::now() + wait;
        loop {
            match timeout_at(deadline, self.incoming.recv()).await {
                Ok(Some((uuid, data))) if uuid == smp::CHARACTERISTIC => match self.reassembler.push(&data) {
                    Some(Ok(response)) if response.seq == self.seq => return Ok(response),
                    Some(Ok(_)) => {} // a late answer to an earlier request
                    Some(Err(e)) => return Err(e.into()),
                    None => {}
                },
                Ok(Some(_)) => {}
                Ok(None) => return Err("lost the connection".into()),
                Err(_) => return Err(format!("no answer to {}", request.describe()).into()),
            }
        }
    }

    /// `request`, with a refusal turned into an error.
    async fn call(&mut self, request: &smp::Request) -> Result<smp::Response, Error> {
        let response = self.request(request, REQUEST_TIMEOUT).await?;
        match response.error() {
            Some(e) => Err(format!("{} arm refused {}: {e}", side_name(self.side), request.describe()).into()),
            None => Ok(response),
        }
    }

    async fn slots(&mut self) -> Result<Vec<Slot>, Error> {
        Ok(self.call(&smp::image_state()).await?.slots())
    }

    /// Even's firmware drops a host that's been quiet on its command channel for 30 seconds, outside update mode.
    async fn heartbeat(&mut self) {
        if let Firmware::Even { tx } = &self.firmware
            && self.last_heartbeat.elapsed() >= HEARTBEAT
        {
            let _ = self.peripheral.write(tx, &proto::silent_mode_get(), WriteType::WithoutResponse).await;
            self.last_heartbeat = Instant::now();
        }
    }

    async fn battery(&mut self) -> Option<u8> {
        let (characteristic, request) = match &self.firmware {
            Firmware::Even { tx } => (tx, proto::battery_get()),
            Firmware::G1v0 { ctrl } => (ctrl, v5::control(v5::ctrl::PROPS, 0xB0, &[v5::prop::BATT_PCT])),
        };
        self.peripheral.write(characteristic, &request, WriteType::WithoutResponse).await.ok()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Ok(Some((uuid, data))) = timeout_at(deadline, self.incoming.recv()).await {
            if uuid == proto::RX
                && let Some(percent) = proto::battery_percent(&data)
            {
                return Some(percent);
            }
            if uuid == v5::EVT
                && let Some(v5::Event { kind: v5::EventKind::Reply { tag: 0xB0, status: 0, body, .. }, .. }) =
                    v5::event(&data)
                && let Some(b) = body.first_chunk::<4>()
                && u32::from_le_bytes(*b) != v5::UNKNOWN
            {
                return Some(u32::from_le_bytes(*b) as u8);
            }
        }
        None
    }

    /// Disconnects, first putting an arm on Even's firmware back in its normal work mode: connecting from macOS
    /// switches it to update mode (see `proto::normal_mode`).
    async fn leave(self) {
        if let Firmware::Even { tx } = &self.firmware
            && self.peripheral.mtu() == 498
        {
            let _ = self.peripheral.write(tx, &proto::normal_mode(), WriteType::WithoutResponse).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let _ = self.peripheral.disconnect().await;
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}

fn find(slots: &[Slot], core: Core, slot: u32) -> Option<&Slot> {
    slots.iter().find(|s| s.image == core.index() && s.slot == slot)
}

fn print_slots(side: Side, slots: &[Slot]) {
    println!("{} arm:", side_name(side));
    for core in [Core::App, Core::Net] {
        for slot in 0..2 {
            let what = if slot == 0 { "installed" } else { "spare    " };
            match find(slots, core, slot) {
                Some(s) => println!("  {} {what}  {}", core.name(), s.describe()),
                None => println!("  {} {what}  empty", core.name()),
            }
        }
    }
}

fn short(hash: &[u8]) -> String {
    hash.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Prints the images in an update file.
pub fn inspect(path: &std::path::Path) -> Result<(), Error> {
    for image in mcuboot::load(path)? {
        println!(
            "{}: {}, version {}, {} bytes, hash {}, {}",
            image.name,
            image.core.name(),
            image.version,
            image.data.len(),
            short(&image.hash),
            if image.trusted() {
                "signed with the key the bootloader trusts"
            } else {
                "NOT signed with the trusted key"
            }
        );
    }
    Ok(())
}

/// Prints what's in each arm's image slots, changing nothing.
pub async fn images(sides: &[Side], channel: Option<u32>) -> Result<(), Error> {
    let adapter = link::adapter().await?;
    for &side in sides {
        let mut arm = Arm::connect(&adapter, side, channel, Duration::from_secs(20)).await?;
        let slots = arm.slots().await;
        let battery = arm.battery().await;
        arm.leave().await;
        print_slots(side, &slots?);
        println!("  battery {}", battery.map_or("?".into(), |b| format!("{b}%")));
    }
    Ok(())
}

/// Uploads the app core image from `path` into the spare slot of the arm on `side`, and with `install`, installs it.
pub async fn flash(path: &std::path::Path, side: Side, channel: Option<u32>, install: bool) -> Result<(), Error> {
    let images = mcuboot::load(path)?;
    for image in images.iter().filter(|i| i.core == Core::Net) {
        println!("leaving out {}: net core images are never sent", image.name);
    }
    let Some(image) = images.into_iter().find(|i| i.core == Core::App) else {
        return Err("no app core image in it".into());
    };
    if !image.trusted() {
        return Err(format!(
            "{} isn't signed with the key the G1's bootloader trusts, so it would refuse it",
            image.name
        )
        .into());
    }
    println!(
        "{}: app core, version {}, {} bytes, hash {}",
        image.name,
        image.version,
        image.data.len(),
        short(&image.hash)
    );

    let adapter = link::adapter().await?;
    let mut arm = Arm::connect(&adapter, side, channel, Duration::from_secs(20)).await?;
    let channel = arm.channel;
    let slots = arm.slots().await?;
    print_slots(side, &slots);
    if find(&slots, Core::App, 0).is_some_and(|s| s.hash == image.hash) {
        println!("already running it");
        arm.leave().await;
        return Ok(());
    }

    if find(&slots, Core::App, 1).is_some_and(|s| s.hash == image.hash) {
        println!("already uploaded");
    } else {
        upload(&adapter, &mut arm, channel, &image).await?;
    }
    let slots = arm.slots().await?;
    if !find(&slots, Core::App, 1).is_some_and(|s| s.hash == image.hash) {
        arm.leave().await;
        return Err("the spare slot doesn't hold the image after the upload".into());
    }
    println!("uploaded and checked; the arm still runs what it did");

    if !install {
        arm.leave().await;
        println!("to install it, run this again with --install");
        return Ok(());
    }
    install_uploaded(&adapter, arm, channel, &image).await
}

async fn upload(adapter: &Adapter, arm: &mut Arm, channel: Option<u32>, image: &Image) -> Result<(), Error> {
    let file_sha: [u8; 32] = Sha256::digest(&image.data).into();
    let total = image.data.len();
    let started = std::time::Instant::now();
    let mut offset = 0;
    let mut drops = 0;
    while offset < total {
        arm.heartbeat().await;
        let request = smp::upload(Core::App.index(), &image.data, offset, &file_sha);
        let response = match arm.request(&request, UPLOAD_TIMEOUT).await {
            Ok(response) => response,
            Err(e) => {
                drops += 1;
                if drops > UPLOAD_DROPS {
                    return Err(format!("upload keeps failing: {e}").into());
                }
                println!("\n{e} at {offset}/{total}; reconnecting");
                *arm = Arm::connect(adapter, arm.side, channel, Duration::from_secs(60)).await?;
                // The first piece again, with the same SHA-256, gets the offset the arm already has.
                offset = 0;
                continue;
            }
        };
        if let Some(e) = response.error() {
            return Err(format!("the arm refused the upload at {offset}/{total}: {e}").into());
        }
        let next = response.int("off").unwrap_or(0) as usize;
        if next <= offset {
            return Err(format!("upload stuck at {offset}/{total}").into());
        }
        offset = next;
        let rate = offset as f64 / 1024.0 / started.elapsed().as_secs_f64();
        print!("\ruploading {offset}/{total} ({}%, {rate:.1} KB/s)   ", offset * 100 / total);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    println!();
    Ok(())
}

async fn install_uploaded(adapter: &Adapter, mut arm: Arm, channel: Option<u32>, image: &Image) -> Result<(), Error> {
    match arm.battery().await {
        Some(level) if level < INSTALL_BATTERY => {
            arm.leave().await;
            return Err(format!("battery at {level}%; charge it past {INSTALL_BATTERY}% first").into());
        }
        Some(level) => println!("battery {level}%"),
        None => println!("battery: no answer"),
    }
    let response = arm.call(&smp::install_next_boot(&image.hash)).await?;
    if !find(&response.slots(), Core::App, 1).is_some_and(|s| s.pending) {
        arm.leave().await;
        return Err("the arm didn't mark the image for install".into());
    }
    println!("restarting to install; leave the arm in the case until it's back");
    if arm.request(&smp::reset(), Duration::from_secs(5)).await.is_err() {
        // The firmware's own restart command.
        let (characteristic, restart) = match &arm.firmware {
            Firmware::Even { tx } => (tx, proto::reboot()),
            Firmware::G1v0 { ctrl } => (ctrl, v5::control(v5::ctrl::REBOOT, 0xB1, &[0])),
        };
        let _ = arm.peripheral.write(characteristic, &restart, WriteType::WithoutResponse).await;
    }
    let side = arm.side;
    let _ = arm.peripheral.disconnect().await;
    drop(arm);

    tokio::time::sleep(Duration::from_secs(10)).await;
    println!("waiting for it to come back…");
    let mut arm = Arm::connect(adapter, side, channel, INSTALL_WAIT).await?;
    let slots = arm.slots().await;
    arm.leave().await;
    let slots = slots?;
    print_slots(side, &slots);
    if find(&slots, Core::App, 0).is_some_and(|s| s.hash == image.hash && s.active) {
        println!("installed and running");
        Ok(())
    } else {
        Err("the arm came back without the new image: the bootloader didn't install it".into())
    }
}
