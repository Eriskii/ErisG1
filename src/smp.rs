//! mcumgr's SMP, the protocol the G1's firmware takes updates over: an 8-byte header and a CBOR map, both ways, on
//! one characteristic of its own. Nothing here touches Bluetooth.

use ciborium::Value;
use uuid::Uuid;

pub const CHARACTERISTIC: Uuid = Uuid::from_u128(0xda2e7828_fbce_4e01_ae9e_261174997c48);

const OP_READ: u8 = 0;
const OP_WRITE: u8 = 2;
const GROUP_OS: u16 = 0;
const GROUP_IMAGE: u16 = 1;
const OS_RESET: u8 = 5;
const IMAGE_STATE: u8 = 0;
const IMAGE_UPLOAD: u8 = 1;

/// Image bytes per upload request. The firmware takes requests of up to 2475 bytes
/// (`CONFIG_MCUMGR_TRANSPORT_NETBUF_SIZE`), which leaves room for the header and the other fields.
pub const UPLOAD_CHUNK: usize = 2048;

pub struct Request {
    op: u8,
    group: u16,
    id: u8,
    body: Vec<(&'static str, Value)>,
}

impl Request {
    /// The frame to write, with sequence number `seq`.
    pub fn frame(&self, seq: u8) -> Vec<u8> {
        let map = Value::Map(self.body.iter().map(|(k, v)| (Value::Text((*k).into()), v.clone())).collect());
        let mut body = Vec::new();
        ciborium::into_writer(&map, &mut body).expect("CBOR into a Vec can't fail");
        let mut frame = vec![self.op, 0];
        frame.extend((body.len() as u16).to_be_bytes());
        frame.extend(self.group.to_be_bytes());
        frame.extend([seq, self.id]);
        frame.extend(body);
        frame
    }

    pub fn describe(&self) -> String {
        format!("group {} command {}", self.group, self.id)
    }
}

/// What's in each image slot.
pub fn image_state() -> Request {
    Request { op: OP_READ, group: GROUP_IMAGE, id: IMAGE_STATE, body: vec![] }
}

/// One piece of an image upload into image `image`'s second slot. The first piece, at offset 0, also gives the
/// image's length and the SHA-256 of the whole file. Sent again after a dropped link, it makes the firmware carry on
/// from what it already has instead of starting over.
pub fn upload(image: u32, data: &[u8], offset: usize, file_sha: &[u8; 32]) -> Request {
    let end = (offset + UPLOAD_CHUNK).min(data.len());
    let mut body = vec![("off", Value::from(offset as u64)), ("data", Value::Bytes(data[offset..end].to_vec()))];
    if offset == 0 {
        body.push(("image", Value::from(image)));
        body.push(("len", Value::from(data.len() as u64)));
        body.push(("sha", Value::Bytes(file_sha.to_vec())));
    }
    Request { op: OP_WRITE, group: GROUP_IMAGE, id: IMAGE_UPLOAD, body }
}

/// Marks the image with `hash`, uploaded into a second slot, to be installed for good at the next restart. (The G1's
/// bootloader can't go back either way; this is how scouter_kit's installer does it.)
pub fn install_next_boot(hash: &[u8; 32]) -> Request {
    let body = vec![("hash", Value::Bytes(hash.to_vec())), ("confirm", Value::Bool(true))];
    Request { op: OP_WRITE, group: GROUP_IMAGE, id: IMAGE_STATE, body }
}

pub fn reset() -> Request {
    Request { op: OP_WRITE, group: GROUP_OS, id: OS_RESET, body: vec![] }
}

pub struct Response {
    pub seq: u8,
    pub body: Vec<(Value, Value)>,
}

impl Response {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.body.iter().find(|(k, _)| k.as_text() == Some(key)).map(|(_, v)| v)
    }

    pub fn int(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(|v| v.as_integer()).and_then(|i| u64::try_from(i).ok())
    }

    /// The firmware's error, if it refused: mcumgr's `rc`, or the `err` map of SMP version 2.
    pub fn error(&self) -> Option<String> {
        match (self.int("rc"), self.get("err")) {
            (Some(0) | None, None) => None,
            (Some(rc), _) => Some(format!("error {rc} ({})", rc_name(rc))),
            (None, Some(err)) => Some(format!("error {err:?}")),
        }
    }

    pub fn slots(&self) -> Vec<Slot> {
        let Some(Value::Array(images)) = self.get("images") else { return vec![] };
        images.iter().filter_map(Slot::parse).collect()
    }
}

/// mcumgr's general error codes.
fn rc_name(rc: u64) -> &'static str {
    match rc {
        1 => "unknown",
        2 => "out of memory",
        3 => "invalid value",
        4 => "timeout",
        5 => "no such entry",
        6 => "bad state",
        7 => "response too large",
        8 => "not supported",
        9 => "corrupt",
        10 => "busy",
        _ => "?",
    }
}

/// Collects notifications into whole responses: each starts with the 8-byte header, whose length field says how much
/// CBOR follows.
#[derive(Default)]
pub struct Reassembler {
    buffer: Vec<u8>,
}

impl Reassembler {
    pub fn push(&mut self, data: &[u8]) -> Option<Result<Response, String>> {
        self.buffer.extend_from_slice(data);
        if self.buffer.len() < 8 {
            return None;
        }
        let end = 8 + u16::from_be_bytes([self.buffer[2], self.buffer[3]]) as usize;
        if self.buffer.len() < end {
            return None;
        }
        let frame: Vec<u8> = self.buffer.drain(..end).collect();
        let seq = frame[6];
        if end == 8 {
            return Some(Ok(Response { seq, body: vec![] }));
        }
        Some(match ciborium::from_reader::<Value, _>(&frame[8..]) {
            Ok(Value::Map(body)) => Ok(Response { seq, body }),
            Ok(other) => Err(format!("response isn't a map: {other:?}")),
            Err(e) => Err(format!("unreadable response: {e}")),
        })
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

#[derive(Debug, Default)]
pub struct Slot {
    pub image: u32,
    pub slot: u32,
    pub version: String,
    pub hash: Vec<u8>,
    pub active: bool,
    pub confirmed: bool,
    pub pending: bool,
    pub permanent: bool,
    pub bootable: bool,
}

impl Slot {
    fn parse(value: &Value) -> Option<Slot> {
        let Value::Map(fields) = value else { return None };
        let mut slot = Slot::default();
        for (key, value) in fields {
            let flag = || value.as_bool().unwrap_or(false);
            let int = || value.as_integer().and_then(|i| u32::try_from(i).ok()).unwrap_or(0);
            match key.as_text()? {
                "image" => slot.image = int(),
                "slot" => slot.slot = int(),
                "version" => slot.version = value.as_text().unwrap_or_default().into(),
                "hash" => slot.hash = value.as_bytes().cloned().unwrap_or_default(),
                "active" => slot.active = flag(),
                "confirmed" => slot.confirmed = flag(),
                "pending" => slot.pending = flag(),
                "permanent" => slot.permanent = flag(),
                "bootable" => slot.bootable = flag(),
                _ => {}
            }
        }
        Some(slot)
    }

    pub fn describe(&self) -> String {
        let flags = [
            (self.active, "running"),
            (self.confirmed, "confirmed"),
            (self.pending, "installs at next restart"),
            (self.permanent, "permanent"),
            (!self.bootable, "not bootable"),
        ];
        let flags: Vec<&str> = flags.iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect();
        let hash: String = self.hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
        format!(
            "version {}, hash {hash}…{}",
            self.version,
            if flags.is_empty() { String::new() } else { format!(", {}", flags.join(", ")) }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_a_request() {
        let frame = image_state().frame(7);
        // Read, no flags, a one-byte body (an empty map), image group, sequence 7, state command.
        assert_eq!(frame, [0, 0, 0, 1, 0, 1, 7, 0, 0xA0]);
    }

    #[test]
    fn first_upload_piece_says_what_is_coming() {
        let data = vec![0xAB; 5000];
        let frame = upload(0, &data, 0, &[1; 32]).frame(1);
        let Some(Ok(request)) = Reassembler::default().push(&frame) else { panic!() };
        assert_eq!(request.int("off"), Some(0));
        assert_eq!(request.int("len"), Some(5000));
        assert_eq!(request.int("image"), Some(0));
        assert_eq!(request.get("data").and_then(|d| d.as_bytes()).map(Vec::len), Some(UPLOAD_CHUNK));
        assert!(frame.len() <= 2475);

        let frame = upload(0, &data, 4096, &[1; 32]).frame(2);
        let Some(Ok(request)) = Reassembler::default().push(&frame) else { panic!() };
        assert_eq!(request.get("data").and_then(|d| d.as_bytes()).map(Vec::len), Some(5000 - 4096));
        assert!(request.get("len").is_none());
    }

    #[test]
    fn reassembles_split_responses_and_reads_slots() {
        let slot = |n: u64, active: bool| {
            Value::Map(vec![
                (Value::Text("image".into()), Value::from(0u64)),
                (Value::Text("slot".into()), Value::from(n)),
                (Value::Text("version".into()), Value::Text("0.0.0".into())),
                (Value::Text("hash".into()), Value::Bytes(vec![n as u8; 32])),
                (Value::Text("active".into()), Value::Bool(active)),
                (Value::Text("bootable".into()), Value::Bool(true)),
            ])
        };
        let request = Request {
            op: 1,
            group: 1,
            id: 0,
            body: vec![("images", Value::Array(vec![slot(0, true), slot(1, false)]))],
        };
        let frame = request.frame(3);
        let mut reassembler = Reassembler::default();
        assert!(reassembler.push(&frame[..20]).is_none());
        let response = reassembler.push(&frame[20..]).unwrap().unwrap();
        assert_eq!(response.seq, 3);
        assert!(response.error().is_none());
        let slots = response.slots();
        assert_eq!(slots.len(), 2);
        assert!(slots[0].active && !slots[1].active);
        assert_eq!(slots[1].hash, vec![1; 32]);
    }

    #[test]
    fn reads_errors() {
        let request = Request { op: 3, group: 1, id: 1, body: vec![("rc", Value::from(6u64))] };
        let response = Reassembler::default().push(&request.frame(0)).unwrap().unwrap();
        assert_eq!(response.error().as_deref(), Some("error 6 (bad state)"));
    }
}
