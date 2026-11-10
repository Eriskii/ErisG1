//! A live connection to one arm running g1v0, speaking protocol 5 (`v5`): packets of instructions sent ahead within
//! the arm's window, acknowledged and resent as §6.2 lays out, and control requests answered by tag.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use btleplug::api::{Central, CentralEvent, Characteristic, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Peripheral};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout_at};

use crate::link::{Error, trace};
use crate::proto::Side;
use crate::v5::{self, Event, EventKind, Hello};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const CONTROL_TRIES: u32 = 3;
/// How long `flush` waits for the arm to run everything sent.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(20);
const RTO_MIN: Duration = Duration::from_millis(100);
const RTO_FIRST: Duration = Duration::from_secs(1);

pub struct Found {
    pub side: Side,
    pub safe: bool,
    pub name: String,
    pub rssi: Option<i16>,
    peripheral: Peripheral,
}

/// Scans for arms running g1v0 for up to `wait`, stopping early once one of each of `sides` is found.
pub async fn scan(adapter: &Adapter, wait: Duration, sides: &[Side]) -> Result<Vec<Found>, Error> {
    let mut events = adapter.events().await?;
    adapter.start_scan(ScanFilter::default()).await?;
    let deadline = Instant::now() + wait;
    let mut found: HashMap<Side, Found> = HashMap::new();
    while let Ok(Some(event)) = timeout_at(deadline, events.next()).await {
        let (CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id)) = event else { continue };
        let peripheral = adapter.peripheral(&id).await?;
        let Some(props) = peripheral.properties().await? else { continue };
        let Some(name) = props.local_name.or(props.advertisement_name) else { continue };
        let Some((side, safe)) = v5::parse_name(&name) else { continue };
        found.insert(side, Found { side, safe, name, rssi: props.rssi, peripheral });
        if sides.iter().all(|s| found.contains_key(s)) {
            break;
        }
    }
    adapter.stop_scan().await?;
    let mut found: Vec<Found> = found.into_values().collect();
    found.sort_by_key(|f| f.side.letter());
    Ok(found)
}

struct Sent {
    seq: u16,
    packet: Vec<u8>,
    at: Instant,
    resent: bool,
}

/// The retransmission timeout of RFC 6298, as §6.2 asks.
struct Rto {
    srtt: Option<f64>,
    rttvar: f64,
    backoff: u32,
}

impl Rto {
    fn sample(&mut self, rtt: Duration) {
        let r = rtt.as_secs_f64();
        match self.srtt {
            None => {
                self.srtt = Some(r);
                self.rttvar = r / 2.0;
            }
            Some(srtt) => {
                self.rttvar = 0.75 * self.rttvar + 0.25 * (srtt - r).abs();
                self.srtt = Some(0.875 * srtt + 0.125 * r);
            }
        }
        self.backoff = 0;
    }

    fn get(&self, interval: Duration) -> Duration {
        let base = match self.srtt {
            None => RTO_FIRST,
            Some(srtt) => Duration::from_secs_f64(srtt + 4.0 * self.rttvar).max(4 * interval).max(RTO_MIN),
        };
        base * (1 << self.backoff.min(3))
    }
}

pub struct Arm {
    pub side: Side,
    pub hello: Hello,
    peripheral: Peripheral,
    cmd: Characteristic,
    ctrl: Characteristic,
    incoming: mpsc::UnboundedReceiver<Vec<u8>>,
    epoch: u32,
    tag: u8,
    /// Bytes of instructions per packet.
    pub max_code: usize,
    next_seq: u16,
    rx_next: u16,
    exec_next: u16,
    unacked: VecDeque<Sent>,
    last_progress: Instant,
    rto: Rto,
    interval: Duration,
    /// The arm's clock less this program's (`host_us`), from `sync_clock`.
    clock_offset: u32,
    errors: Vec<String>,
    replies: HashMap<u8, (u8, Vec<u8>)>,
    /// Events nobody asked for: sensor inputs, notifications, notices.
    pub events: VecDeque<Event>,
}

impl Arm {
    pub async fn open(found: Found) -> Result<Arm, Error> {
        let Found { side, peripheral, .. } = found;
        peripheral.connect().await?;
        peripheral.discover_services().await?;
        let characteristics = peripheral.characteristics();
        let find = |uuid| characteristics.iter().find(|c| c.uuid == uuid).cloned();
        let (Some(cmd), Some(ctrl), Some(evt)) = (find(v5::CMD), find(v5::CTRL), find(v5::EVT)) else {
            let _ = peripheral.disconnect().await;
            return Err(format!("the {} arm has no g1v0 service", side_name(side)).into());
        };
        peripheral.subscribe(&evt).await?;
        let (sender, incoming) = mpsc::unbounded_channel();
        let mut notifications = peripheral.notifications().await?;
        tokio::spawn(async move {
            while let Some(n) = notifications.next().await {
                if n.uuid == v5::EVT && sender.send(n.value).is_err() {
                    break;
                }
            }
        });
        trace(format_args!("{}: connected, mtu {}", side.letter(), peripheral.mtu()));
        let mut arm = Arm {
            side,
            hello: Hello::parse(&[v5::PROTOCOL; 35]).unwrap(), // replaced by the arm's own just below
            max_code: 20,
            peripheral,
            cmd,
            ctrl,
            incoming,
            epoch: 0,
            tag: 0,
            next_seq: 0,
            rx_next: 0,
            exec_next: 0,
            unacked: VecDeque::new(),
            last_progress: Instant::now(),
            rto: Rto { srtt: None, rttvar: 0.0, backoff: 0 },
            interval: Duration::from_millis(30),
            clock_offset: 0,
            errors: Vec::new(),
            replies: HashMap::new(),
            events: VecDeque::new(),
        };
        let hello = arm.hello().await?;
        arm.next_seq = hello.rx_next;
        arm.rx_next = hello.rx_next;
        arm.exec_next = hello.exec_next;
        // The packet is at most min(MTU − 3, 244) bytes, and macOS gives no way to read the write size.
        arm.max_code = (hello.max_packet as usize).min(arm.peripheral.mtu() as usize - 3) - 2;
        if let Ok(interval) = arm.props(&[v5::prop::LINK_INTERVAL]).await {
            arm.interval = Duration::from_micros(interval[0] as u64);
        }
        Ok(arm)
    }

    async fn hello(&mut self) -> Result<Hello, Error> {
        let body = self.control(v5::ctrl::HELLO, &[]).await?;
        let hello = Hello::parse(&body)?;
        self.hello = hello.clone();
        Ok(hello)
    }

    /// Sends a control request and returns its reply's body, or an error for a refusal.
    pub async fn control(&mut self, op: u8, body: &[u8]) -> Result<Vec<u8>, Error> {
        for _ in 0..CONTROL_TRIES {
            self.tag = self.tag.wrapping_add(1);
            let tag = self.tag;
            self.peripheral.write(&self.ctrl, &v5::control(op, tag, body), WriteType::WithoutResponse).await?;
            let deadline = Instant::now() + CONTROL_TIMEOUT;
            while Instant::now() < deadline {
                if let Some((status, body)) = self.replies.remove(&tag) {
                    if status != 0 {
                        return Err(format!(
                            "{} arm refused control request {op:#04x}: {}",
                            side_name(self.side),
                            v5::error_name(status)
                        )
                        .into());
                    }
                    return Ok(body);
                }
                self.pump(deadline).await?;
            }
        }
        Err(format!("{} arm didn't answer control request {op:#04x}", side_name(self.side)).into())
    }

    /// Reads properties (§2.5), at most 12.
    pub async fn props(&mut self, ids: &[u8]) -> Result<Vec<u32>, Error> {
        let body = self.control(v5::ctrl::PROPS, ids).await?;
        Ok(body.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect())
    }

    /// Sends instructions, packed into as few packets as fit, waiting for room in the arm's window as needed.
    pub async fn run(&mut self, instructions: impl IntoIterator<Item = Vec<u8>>) -> Result<(), Error> {
        for code in v5::pack(instructions, self.max_code) {
            self.send(code).await?;
        }
        Ok(())
    }

    async fn send(&mut self, code: Vec<u8>) -> Result<(), Error> {
        while self.next_seq.wrapping_sub(self.exec_next) >= v5::WINDOW {
            self.pump(Instant::now() + self.rto.get(self.interval)).await?;
        }
        let seq = self.next_seq;
        self.next_seq = seq.wrapping_add(1);
        let packet = v5::packet(seq, &code);
        self.peripheral.write(&self.cmd, &packet, WriteType::WithoutResponse).await?;
        if self.unacked.is_empty() {
            self.last_progress = Instant::now();
        }
        self.unacked.push_back(Sent { seq, packet, at: Instant::now(), resent: false });
        Ok(())
    }

    /// Waits until the arm has run everything sent, and reports the first error any of it hit.
    pub async fn flush(&mut self) -> Result<(), Error> {
        let sent = self.next_seq;
        self.wait_for(sent).await
    }

    /// The sequence number the next packet will get: `wait_for` it to wait for everything sent so far.
    pub fn sent(&self) -> u16 {
        self.next_seq
    }

    /// Waits until the arm has run every packet before `seq`, and reports the first error any of it hit.
    pub async fn wait_for(&mut self, seq: u16) -> Result<(), Error> {
        let deadline = Instant::now() + FLUSH_TIMEOUT;
        while v5::before(self.exec_next, seq) {
            if Instant::now() > deadline {
                return Err(format!("{} arm stopped running packets", side_name(self.side)).into());
            }
            self.pump(deadline).await?;
        }
        if self.errors.is_empty() { Ok(()) } else { Err(std::mem::take(&mut self.errors).join("; ").into()) }
    }

    /// Asks for a 7.5–15 ms connection interval and the 2M PHY; the Mac decides. Returns the interval in force
    /// afterwards. (Asking for 15–30 ms, as Apple's accessory guidelines would have it, got 30 ms.)
    pub async fn ask_for_fast_link(&mut self) -> Result<Duration, Error> {
        let mut body = Vec::new();
        for value in [6u16, 12, 0, 400] {
            body.extend(value.to_le_bytes());
        }
        body.push(2);
        let reply = self.control(v5::ctrl::LINK, &body).await?;
        trace(format_args!("{}: link request answered {}", self.side.letter(), crate::proto::hex(&reply)));
        // A granted change arrives as a NOTICE within a few connection events.
        let _ = self.next_event(Duration::from_secs(2)).await?;
        Ok(self.interval)
    }

    /// Measures the arm's clock against this program's (`host_us`) with a few `PING`s, keeping the one with the
    /// shortest round trip (§7). Returns its round trip.
    pub async fn sync_clock(&mut self) -> Result<Duration, Error> {
        let mut best: Option<(u32, u32)> = None;
        for _ in 0..8 {
            let sent = host_us();
            let reply = self.control(v5::ctrl::PING, &sent.to_le_bytes()).await?;
            let received = host_us();
            let [echo, t_rx, t_tx] =
                reply.as_chunks::<4>().0[..3].iter().map(|b| u32::from_le_bytes(*b)).collect::<Vec<_>>()[..]
            else {
                return Err("short PING reply".into());
            };
            if echo != sent {
                continue;
            }
            let delay = received.wrapping_sub(sent).wrapping_sub(t_tx.wrapping_sub(t_rx));
            // offset = (t_rx − sent + t_tx − received) / 2, in 32-bit wrapping arithmetic.
            let there = t_rx.wrapping_sub(sent);
            let back = t_tx.wrapping_sub(received);
            let offset = there.wrapping_add((back.wrapping_sub(there) as i32 / 2) as u32);
            if best.is_none_or(|(d, _)| delay < d) {
                best = Some((delay, offset));
            }
        }
        let (delay, offset) = best.ok_or("no PING came back")?;
        self.clock_offset = offset;
        Ok(Duration::from_micros(delay as u64))
    }

    /// The arm's clock when this program's (`host_us`) reads `host`.
    pub fn arm_time(&self, host: u32) -> u32 {
        host.wrapping_add(self.clock_offset)
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// The next event nobody asked for, waiting up to `wait`.
    pub async fn next_event(&mut self, wait: Duration) -> Result<Option<Event>, Error> {
        let deadline = Instant::now() + wait;
        while self.events.is_empty() && Instant::now() < deadline {
            self.pump(deadline).await?;
        }
        Ok(self.events.pop_front())
    }

    /// Handles what arrives until the next event or `deadline`, and resends what the arm is missing.
    async fn pump(&mut self, deadline: Instant) -> Result<(), Error> {
        let wake = match self.unacked.front() {
            Some(_) => deadline.min(self.last_progress + self.rto.get(self.interval)),
            None => deadline,
        };
        match timeout_at(wake, self.incoming.recv()).await {
            Ok(Some(data)) => match v5::event(&data) {
                Some(event) => {
                    if self.handle(event)? {
                        self.resend(0).await?;
                    }
                }
                None => trace(format_args!("{}: unreadable event {}", self.side.letter(), crate::proto::hex(&data))),
            },
            Ok(None) => return Err(format!("lost the {} arm", side_name(self.side)).into()),
            Err(_) => {}
        }
        if self.unacked.front().is_some() && self.last_progress.elapsed() >= self.rto.get(self.interval) {
            // Nothing moved for a whole timeout: the oldest packet is lost, or its acknowledgement was.
            self.rto.backoff += 1;
            self.last_progress = Instant::now();
            self.resend(0).await?;
        }
        Ok(())
    }

    async fn resend(&mut self, index: usize) -> Result<(), Error> {
        let Some(sent) = self.unacked.get_mut(index) else { return Ok(()) };
        trace(format_args!("{}: resending packet {}", self.side.letter(), sent.seq));
        sent.resent = true;
        sent.at = Instant::now();
        let packet = sent.packet.clone();
        self.peripheral.write(&self.cmd, &packet, WriteType::WithoutResponse).await?;
        Ok(())
    }

    /// Takes in an event. True when the oldest unacknowledged packet should go again: the arm holds later ones.
    fn handle(&mut self, event: Event) -> Result<bool, Error> {
        if self.epoch == 0 {
            self.epoch = event.epoch;
        } else if event.epoch != self.epoch {
            return Err(format!("the {} arm restarted, forgetting what it was sent", side_name(self.side)).into());
        }
        match event.kind {
            EventKind::Ack { rx_next, exec_next, sack, .. } => {
                if v5::before(self.rx_next, rx_next) {
                    self.last_progress = Instant::now();
                }
                self.rx_next = rx_next;
                self.exec_next = exec_next;
                while let Some(sent) = self.unacked.front() {
                    if !v5::before(sent.seq, rx_next) {
                        break;
                    }
                    if !sent.resent {
                        self.rto.sample(sent.at.elapsed());
                    }
                    self.unacked.pop_front();
                }
                // Later packets held but this one missing: resend it, at most once per round trip.
                if let Some(first) = self.unacked.front() {
                    let rtt = self.rto.get(self.interval) / 2;
                    return Ok(sack != 0 && first.seq == rx_next && first.at.elapsed() >= rtt);
                }
            }
            EventKind::Error { seq, offset, op, code, detail } => self.errors.push(format!(
                "{} arm: packet {seq}, instruction {op:#04x} at offset {offset}: {} (detail {detail:#x})",
                side_name(self.side),
                v5::error_name(code)
            )),
            EventKind::Reply { tag, status, body, .. } => {
                self.replies.insert(tag, (status, body));
            }
            kind => {
                // Link parameters changed: the interval paces resends.
                if let EventKind::Notice { kind: 1, body } = &kind
                    && let Some(units) = body.first_chunk::<2>()
                {
                    self.interval = Duration::from_micros(u16::from_le_bytes(*units) as u64 * 1250);
                    trace(format_args!("{}: link interval now {:?}", self.side.letter(), self.interval));
                }
                self.events.push_back(Event { kind, ..event });
            }
        }
        Ok(false)
    }

    pub async fn disconnect(self) {
        let _ = self.peripheral.disconnect().await;
    }
}

/// This program's clock for timing the arms, µs since it started, wrapping like theirs.
pub fn host_us() -> u32 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_micros() as u32
}

pub fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}
