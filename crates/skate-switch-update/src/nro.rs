//! Checks that a file is a whole NRO and reads the version its NACP shows, as libnx lays them out
//! (`nro.h`, `nacp.h`).
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

/// `NroStart` (0x10 bytes) and `NroHeader` (0x70).
const HEADER: usize = 0x80;
/// `NroAssetHeader`: magic, version, then the icon's, the NACP's and the RomFS's offset and size.
const ASSETS: usize = 0x38;
const NACP_SIZE: u64 = 0x4000;
/// `NacpStruct::display_version`, 0x10 bytes.
const NACP_VERSION: usize = 0x3060;

fn u32_at(bytes: &[u8], at: usize) -> u64 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()).into()
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

/// The version the NACP of the NRO at `path` shows, once the file has an NRO header whose
/// segments are inside its image, and an asset section after the image with a whole NACP inside
/// the file.
pub(crate) fn version(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let mut header = [0; HEADER];
    file.read_exact(&mut header).map_err(|_| "shorter than an NRO header")?;
    if &header[0x10..0x14] != b"NRO0" {
        return Err("no NRO header".into());
    }
    let image = u32_at(&header, 0x18);
    if image < HEADER as u64 || image > length {
        return Err(format!("its image is {image} bytes, in a file of {length}"));
    }
    for segment in 0..3 {
        let (offset, size) = (u32_at(&header, 0x20 + segment * 8), u32_at(&header, 0x24 + segment * 8));
        if offset + size > image {
            return Err(format!("segment {segment} ends past the image"));
        }
    }
    let mut assets = [0; ASSETS];
    file.seek(SeekFrom::Start(image)).map_err(|e| e.to_string())?;
    file.read_exact(&mut assets).map_err(|_| "no asset section after the image")?;
    let (nacp, nacp_size) = (u64_at(&assets, 0x18), u64_at(&assets, 0x20));
    if &assets[..4] != b"ASET" || nacp_size != NACP_SIZE {
        return Err("no NACP".into());
    }
    let start = image.checked_add(nacp).filter(|start| start.checked_add(NACP_SIZE).is_some_and(|end| end <= length));
    let start = start.ok_or("its NACP ends past the file")?;
    let mut version = [0; 0x10];
    file.seek(SeekFrom::Start(start + NACP_VERSION as u64)).map_err(|e| e.to_string())?;
    file.read_exact(&mut version).map_err(|e| e.to_string())?;
    let text = version.split(|&byte| byte == 0).next().unwrap_or_default();
    String::from_utf8(text.to_vec()).map_err(|_| "its NACP version isn't text".into())
}

/// An NRO as elf2nro lays one out: header, three segments, then the asset section with an icon
/// and a NACP showing `version`.
#[cfg(test)]
pub(crate) fn fake(version: &str) -> Vec<u8> {
    let image = 0x1000_usize;
    let mut bytes = vec![0; image];
    bytes[0x10..0x14].copy_from_slice(b"NRO0");
    bytes[0x18..0x1C].copy_from_slice(&(image as u32).to_le_bytes());
    for (segment, (offset, size)) in [(0x0, 0x800), (0x800, 0x400), (0xC00, 0x400)].into_iter().enumerate() {
        bytes[0x20 + segment * 8..0x24 + segment * 8].copy_from_slice(&(offset as u32).to_le_bytes());
        bytes[0x24 + segment * 8..0x28 + segment * 8].copy_from_slice(&(size as u32).to_le_bytes());
    }
    let mut assets = vec![0; ASSETS];
    assets[..4].copy_from_slice(b"ASET");
    let (icon, icon_size) = (ASSETS as u64, 0x100_u64);
    assets[0x08..0x10].copy_from_slice(&icon.to_le_bytes());
    assets[0x10..0x18].copy_from_slice(&icon_size.to_le_bytes());
    assets[0x18..0x20].copy_from_slice(&(icon + icon_size).to_le_bytes());
    assets[0x20..0x28].copy_from_slice(&NACP_SIZE.to_le_bytes());
    bytes.extend(assets);
    bytes.extend(vec![0xFF; icon_size as usize]);
    let mut nacp = vec![0; NACP_SIZE as usize];
    nacp[NACP_VERSION..NACP_VERSION + version.len()].copy_from_slice(version.as_bytes());
    bytes.extend(nacp);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_of_a_whole_nro_and_refuses_damaged_ones() {
        let dir = std::env::temp_dir().join(format!("skate-switch-update-nro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("game.nro");
        let whole = fake("0.1.5");
        std::fs::write(&path, &whole).unwrap();
        assert_eq!(version(&path).as_deref(), Ok("0.1.5"));

        std::fs::write(&path, &whole[..whole.len() - 1]).unwrap();
        assert!(version(&path).is_err(), "a NACP cut short");
        let mut bad = whole.clone();
        bad[0x10] = b'X';
        std::fs::write(&path, &bad).unwrap();
        assert!(version(&path).is_err(), "no NRO0 magic");
        let mut bad = whole.clone();
        bad[0x2C..0x30].copy_from_slice(&0x2000_u32.to_le_bytes());
        std::fs::write(&path, &bad).unwrap();
        assert!(version(&path).is_err(), "a segment past the image");
        std::fs::write(&path, &whole[..0x1000]).unwrap();
        assert!(version(&path).is_err(), "no asset section");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
