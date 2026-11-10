//! Signed MCUboot firmware images, as a G1 update zip carries them, and the checks made before one goes near an arm.
//!
//! Each arm is an nRF5340 with two cores. The update zip holds one image for each: the app core runs the G1's own
//! firmware, and the net core runs the Bluetooth radio.

use std::io::Read;

use sha2::{Digest, Sha256};

const MAGIC: u32 = 0x96F3_B83D;
const TLV_INFO_MAGIC: u16 = 0x6907;
const TLV_PROTECTED_INFO_MAGIC: u16 = 0x6908;
const TLV_KEY_HASH: u16 = 0x01;
const TLV_SHA256: u16 = 0x10;

/// The SHA-256 of the public key the G1's bootloader checks signatures with: the RSA-2048 sample key that ships with
/// MCUboot and NCS (`bootloader/mcuboot/root-rsa-2048.pem`), which Even signs its own updates with.
pub const TRUSTED_KEY_HASH: [u8; 32] = [
    0xfc, 0x57, 0x01, 0xdc, 0x61, 0x35, 0xe1, 0x32, 0x38, 0x47, 0xbd, 0xc4, 0x0f, 0x04, 0xd2, 0xe5, 0xbe, 0xe5, 0x83,
    0x3b, 0x23, 0xc2, 0x9f, 0x93, 0x59, 0x3d, 0x00, 0x01, 0x8c, 0xfa, 0x99, 0x94,
];

/// Where the app core's code runs from (`app` in `firmware/pm_static.yml`), after MCUboot and the image header.
const APP_CODE: std::ops::Range<u32> = 0xC200..0xFC000;
/// The app core's slot. The bootloader keeps its install flags in the slot's last sector, so an image must end
/// before it.
const APP_SLOT_SIZE: usize = 0xF0000;
const SECTOR: usize = 0x1000;
const NET_CODE: std::ops::Range<u32> = 0x0100_0000..0x0104_0000;
const APP_RAM: std::ops::RangeInclusive<u32> = 0x2000_0000..=0x2008_0000;
const NET_RAM: std::ops::RangeInclusive<u32> = 0x2100_0000..=0x2101_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Core {
    App,
    Net,
}

impl Core {
    /// The image number mcumgr and MCUboot know this core's image by.
    pub fn index(self) -> u32 {
        match self {
            Core::App => 0,
            Core::Net => 1,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Core::App => "app core",
            Core::Net => "net core",
        }
    }
}

pub struct Image {
    pub name: String,
    pub data: Vec<u8>,
    pub core: Core,
    pub version: String,
    /// What mcumgr and MCUboot identify the image by: the SHA-256 in its trailer, over the header and code.
    pub hash: [u8; 32],
    pub key_hash: Option<[u8; 32]>,
}

impl Image {
    pub fn parse(name: &str, data: Vec<u8>) -> Result<Image, String> {
        let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if u32_at(0) != Some(MAGIC) {
            return Err(format!("{name}: not an MCUboot image"));
        }
        let header_size = u16_at(8).unwrap() as usize;
        let protected_size = u16_at(10).unwrap_or(0) as usize;
        let code_size = u32_at(12).unwrap_or(0) as usize;
        let version = match data.get(20..28) {
            Some(v) => format!("{}.{}.{}+{}", v[0], v[1], u16::from_le_bytes([v[2], v[3]]), u32_at(24).unwrap()),
            None => return Err(format!("{name}: truncated header")),
        };

        // The vector table at the start of the code: the initial stack pointer, then the reset handler, which says
        // which core and which flash region the image was built for.
        let (Some(stack), Some(reset)) = (u32_at(header_size), u32_at(header_size + 4)) else {
            return Err(format!("{name}: truncated"));
        };
        let core = if APP_CODE.contains(&reset) {
            Core::App
        } else if NET_CODE.contains(&reset) {
            Core::Net
        } else {
            return Err(format!(
                "{name}: reset handler at {reset:#x}, outside the G1's app core slot ({:#x}–{:#x}) and net core \
                 flash, so it was built for another layout",
                APP_CODE.start, APP_CODE.end
            ));
        };
        let ram = if core == Core::App { APP_RAM } else { NET_RAM };
        if !ram.contains(&stack) {
            return Err(format!("{name}: initial stack pointer {stack:#x} isn't in the {}'s RAM", core.name()));
        }

        // The trailer: an optional protected TLV area, then the TLV area with the hash and signature.
        let mut hash = None;
        let mut key_hash = None;
        let mut at = header_size + code_size;
        for magic in [TLV_PROTECTED_INFO_MAGIC, TLV_INFO_MAGIC] {
            if u16_at(at) != Some(magic) {
                if magic == TLV_PROTECTED_INFO_MAGIC && protected_size == 0 {
                    continue;
                }
                return Err(format!("{name}: malformed trailer"));
            }
            let end = at + u16_at(at + 2).unwrap() as usize;
            let mut tlv = at + 4;
            while tlv + 4 <= end {
                let (kind, len) = (u16_at(tlv).unwrap(), u16_at(tlv + 2).unwrap() as usize);
                let value = data.get(tlv + 4..tlv + 4 + len).ok_or(format!("{name}: truncated trailer"))?;
                match (kind, <[u8; 32]>::try_from(value)) {
                    (TLV_SHA256, Ok(v)) => hash = Some(v),
                    (TLV_KEY_HASH, Ok(v)) => key_hash = Some(v),
                    _ => {}
                }
                tlv += 4 + len;
            }
            at = end;
        }
        if at != data.len() {
            return Err(format!("{name}: {} stray bytes after the trailer", data.len() as isize - at as isize));
        }
        let Some(hash) = hash else { return Err(format!("{name}: no SHA-256 in the trailer")) };
        if Sha256::digest(&data[..header_size + code_size + protected_size]).as_slice() != hash {
            return Err(format!("{name}: its contents don't match its hash, so the file is damaged"));
        }
        if core == Core::App && data.len() > APP_SLOT_SIZE - SECTOR {
            return Err(format!("{name}: {} bytes, too big for the app core slot", data.len()));
        }
        Ok(Image { name: name.into(), data, core, version, hash, key_hash })
    }

    /// Whether the G1's bootloader will accept the signature's key. (It checks the signature itself.)
    pub fn trusted(&self) -> bool {
        self.key_hash == Some(TRUSTED_KEY_HASH)
    }
}

/// The images in an update zip, or the single image in a `.bin`.
pub fn load(path: &std::path::Path) -> Result<Vec<Image>, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if !data.starts_with(b"PK") {
        return Ok(vec![Image::parse(&name, data)?]);
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| format!("{name}: {e}"))?;
    let mut images = Vec::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| format!("{name}: {e}"))?;
        let entry = file.name().map_err(|e| format!("{name}: {e}"))?.to_string();
        if !entry.ends_with(".bin") {
            continue;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(|e| format!("{name}: {e}"))?;
        images.push(Image::parse(&format!("{name}/{entry}"), data)?);
    }
    if images.is_empty() {
        return Err(format!("{name}: no images in it"));
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal unsigned image whose code starts at `reset`: header, vector table, padding, and a trailer with its
    /// hash.
    pub fn fake(reset: u32, code: usize) -> Vec<u8> {
        let mut data = vec![0u8; 0x200];
        data[..4].copy_from_slice(&MAGIC.to_le_bytes());
        data[8..10].copy_from_slice(&0x200u16.to_le_bytes());
        data[12..16].copy_from_slice(&(code as u32).to_le_bytes());
        data[20..28].copy_from_slice(&[1, 6, 6, 0, 2, 0, 0, 0]);
        let mut body = vec![0u8; code];
        let stack: u32 = if NET_CODE.contains(&reset) { 0x2100_8000 } else { 0x2000_8000 };
        body[..4].copy_from_slice(&stack.to_le_bytes());
        body[4..8].copy_from_slice(&reset.to_le_bytes());
        data.extend(body);
        let hash = Sha256::digest(&data);
        data.extend(TLV_INFO_MAGIC.to_le_bytes());
        data.extend(((4 + 4 + 32) as u16).to_le_bytes());
        data.extend(TLV_SHA256.to_le_bytes());
        data.extend(32u16.to_le_bytes());
        data.extend(hash.as_slice());
        data
    }

    #[test]
    fn reads_an_app_core_image() {
        let image = Image::parse("app.bin", fake(0xC201 + 0x100, 0x400)).unwrap();
        assert_eq!(image.core, Core::App);
        assert_eq!(image.version, "1.6.6+2");
        assert!(!image.trusted());
    }

    #[test]
    fn tells_the_net_core_apart() {
        assert_eq!(Image::parse("net.bin", fake(0x0100_1001, 0x400)).unwrap().core, Core::Net);
    }

    #[test]
    fn refuses_images_built_for_another_layout() {
        assert!(Image::parse("dk.bin", fake(0x0000_0201, 0x400)).is_err());
        assert!(Image::parse("big.bin", fake(0xC301, APP_SLOT_SIZE)).is_err());
    }

    #[test]
    fn refuses_damaged_images() {
        let mut data = fake(0xC301, 0x400);
        data[0x300] ^= 1;
        assert!(Image::parse("bad.bin", data).err().unwrap().contains("damaged"));
        let mut data = fake(0xC301, 0x400);
        data.push(0);
        assert!(Image::parse("long.bin", data).is_err());
    }
}
