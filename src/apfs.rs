use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug)]
pub struct ContainerInfo {
    pub block_size: u32,
    pub block_count: u64,
    pub image_bytes: u64,
    pub uuid: String,
}

impl ContainerInfo {
    pub fn capacity(&self) -> u64 {
        u64::from(self.block_size) * self.block_count
    }
}

pub fn inspect(path: &Path) -> Result<ContainerInfo, String> {
    let mut file = File::open(path).map_err(|error| format!("Cannot open image: {error}"))?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("Select a regular raw partition image.".into());
    }
    let image_bytes = file.metadata().map_err(|error| error.to_string())?.len();
    let mut header = [0_u8; 4096];
    file.read_exact(&mut header)
        .map_err(|error| format!("Cannot read container header: {error}"))?;
    parse_header(&header, image_bytes)
}

fn parse_header(header: &[u8], image_bytes: u64) -> Result<ContainerInfo, String> {
    if header.len() < 4096 {
        return Err("Container header is truncated.".into());
    }
    if &header[32..36] != b"NXSB" {
        return Err("No APFS container at offset 0. Select a raw APFS partition image; whole-disk GPT images and compressed DMG files are not supported yet.".into());
    }
    let block_size = u32::from_le_bytes(header[36..40].try_into().unwrap());
    let block_count = u64::from_le_bytes(header[40..48].try_into().unwrap());
    if !(4096..=65536).contains(&block_size) || !block_size.is_power_of_two() {
        return Err("Invalid APFS block size.".into());
    }
    let capacity = u64::from(block_size)
        .checked_mul(block_count)
        .filter(|capacity| *capacity > 0)
        .ok_or("Invalid APFS block count.")?;
    if capacity > image_bytes {
        return Err("Image is shorter than the declared container size; incomplete or multi-device images are not supported.".into());
    }
    let uuid = header[72..88]
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            let prefix = if [4, 6, 8, 10].contains(&index) {
                "-"
            } else {
                ""
            };
            format!("{prefix}{byte:02x}")
        })
        .collect();
    Ok(ContainerInfo {
        block_size,
        block_count,
        image_bytes,
        uuid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> [u8; 4096] {
        let mut bytes = [0; 4096];
        bytes[32..36].copy_from_slice(b"NXSB");
        bytes[36..40].copy_from_slice(&4096_u32.to_le_bytes());
        bytes[40..48].copy_from_slice(&2_u64.to_le_bytes());
        bytes[72..88].copy_from_slice(&[0x12; 16]);
        bytes
    }

    #[test]
    fn reads_container_fields() {
        let info = parse_header(&header(), 8192).unwrap();
        assert_eq!(info.block_size, 4096);
        assert_eq!(info.block_count, 2);
        assert_eq!(info.capacity(), 8192);
        assert_eq!(info.uuid, "12121212-1212-1212-1212-121212121212");
    }

    #[test]
    fn rejects_short_header_and_wrong_signature() {
        assert!(parse_header(&[0; 80], 8192).is_err());
        assert!(parse_header(&[0; 4096], 8192).is_err());
    }

    #[test]
    fn rejects_invalid_geometry() {
        let mut bytes = header();
        assert!(parse_header(&bytes, 4096).is_err());
        bytes[36..40].copy_from_slice(&4097_u32.to_le_bytes());
        assert!(parse_header(&bytes, u64::MAX).is_err());
        bytes[36..40].copy_from_slice(&4096_u32.to_le_bytes());
        bytes[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse_header(&bytes, u64::MAX).is_err());
        bytes[40..48].copy_from_slice(&0_u64.to_le_bytes());
        assert!(parse_header(&bytes, 8192).is_err());
    }
}
