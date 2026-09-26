use serde::Deserialize;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Partition {
    pub disk_number: u32,
    pub disk_name: String,
    pub partition_number: u32,
    pub offset: u64,
    pub size: u64,
    pub sector_size: u32,
    pub gpt_type: String,
}

pub struct DetectedDrive {
    pub partition: Partition,
    pub verification: Result<(), String>,
}

pub struct ScanReport {
    pub disk_count: usize,
    pub drives: Vec<DetectedDrive>,
    pub warnings: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Inventory {
    disk_count: usize,
    partitions: Vec<Partition>,
    warnings: Vec<String>,
}

fn is_apfs(guid: &str) -> bool {
    guid.trim_matches(['{', '}'])
        .eq_ignore_ascii_case("7c3457ef-0000-11aa-aa11-00306543ecac")
}

fn verify_header(reader: &mut (impl Read + Seek), partition: &Partition) -> Result<(), String> {
    let sector_size = partition.sector_size;
    if !(512..=65536).contains(&sector_size)
        || !sector_size.is_power_of_two()
        || !partition.offset.is_multiple_of(u64::from(sector_size))
        || partition.size < u64::from(sector_size)
        || partition.offset.checked_add(partition.size).is_none()
    {
        return Err("Invalid partition geometry.".into());
    }
    reader
        .seek(SeekFrom::Start(partition.offset))
        .map_err(|error| error.to_string())?;
    let mut sector = vec![0; sector_size as usize];
    reader
        .read_exact(&mut sector)
        .map_err(|error| format!("Cannot read partition: {error}"))?;
    if &sector[32..36] != b"NXSB" {
        return Err("APFS GPT type found, but NXSB signature is missing.".into());
    }
    Ok(())
}

#[cfg(windows)]
pub fn scan() -> Result<ScanReport, String> {
    use std::fs::OpenOptions;
    use std::os::windows::{fs::OpenOptionsExt, process::CommandExt};
    use std::process::Command;

    let script = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$disks = @(Get-Disk -ErrorAction Stop)
$partitions = @()
$warnings = @()
foreach ($disk in $disks) {
    if ($disk.PartitionStyle -ne 'GPT') { continue }
    try {
        foreach ($partition in @(Get-Partition -DiskNumber $disk.Number -ErrorAction Stop)) {
            $partitions += [pscustomobject]@{
                DiskNumber = [uint32]$disk.Number
                DiskName = [string]$disk.FriendlyName
                PartitionNumber = [uint32]$partition.PartitionNumber
                Offset = [uint64]$partition.Offset
                Size = [uint64]$partition.Size
                SectorSize = [uint32]$disk.LogicalSectorSize
                GptType = [string]$partition.GptType
            }
        }
    } catch { $warnings += "Disk $($disk.Number): $($_.Exception.Message)" }
}
ConvertTo-Json -Depth 4 -Compress -InputObject ([pscustomobject]@{
    DiskCount = $disks.Count
    Partitions = @($partitions)
    Warnings = @($warnings)
})
"#;
    let system_root = std::env::var_os("SystemRoot").ok_or("SystemRoot is not set.")?;
    let powershell = std::path::PathBuf::from(system_root)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .creation_flags(0x08000000)
        .output()
        .map_err(|error| format!("Cannot query Windows disks: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Windows disk query failed. Administrator access may be required. {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let inventory: Inventory = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Invalid Windows disk query response: {error}"))?;
    let drives = inventory
        .partitions
        .into_iter()
        .filter(|partition| is_apfs(&partition.gpt_type))
        .map(|partition| {
            let device = format!(r"\\.\PhysicalDrive{}", partition.disk_number);
            let verification = OpenOptions::new()
                .read(true)
                .share_mode(3)
                .open(device)
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::PermissionDenied {
                        "Access denied. Run APFS Explorer as Administrator to verify the signature."
                            .to_owned()
                    } else {
                        format!("Cannot open disk read-only: {error}")
                    }
                })
                .and_then(|mut file| verify_header(&mut file, &partition));
            DetectedDrive {
                partition,
                verification,
            }
        })
        .collect();
    Ok(ScanReport {
        disk_count: inventory.disk_count,
        drives,
        warnings: inventory.warnings,
    })
}

#[cfg(not(windows))]
pub fn scan() -> Result<ScanReport, String> {
    Err("Physical disk scanning is supported on Windows only.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[cfg(windows)]
    #[test]
    #[ignore = "Queries connected Windows disks; run explicitly on a local machine"]
    fn scans_connected_windows_disks() {
        let report = scan().expect("Windows disk inventory failed");
        println!(
            "Disks: {}; APFS partitions: {}; warnings: {}",
            report.disk_count,
            report.drives.len(),
            report.warnings.len()
        );
        for drive in report.drives {
            println!(
                "Disk {} partition {}: {:?}",
                drive.partition.disk_number, drive.partition.partition_number, drive.verification
            );
        }
        for warning in report.warnings {
            println!("Warning: {warning}");
        }
    }

    fn partition() -> Partition {
        Partition {
            disk_number: 0,
            disk_name: "Test disk".into(),
            partition_number: 1,
            offset: 4096,
            size: 4096,
            sector_size: 4096,
            gpt_type: "{7C3457EF-0000-11AA-AA11-00306543ECAC}".into(),
        }
    }

    #[test]
    fn matches_apfs_guid_only() {
        assert!(is_apfs(&partition().gpt_type));
        assert!(is_apfs("7c3457ef-0000-11aa-aa11-00306543ecac"));
        assert!(!is_apfs("ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"));
    }

    #[test]
    fn verifies_at_partition_offset_for_sector_sizes() {
        for sector_size in [512, 4096] {
            let mut partition = partition();
            partition.sector_size = sector_size;
            let mut bytes = vec![0; 8192];
            bytes[4128..4132].copy_from_slice(b"NXSB");
            assert!(verify_header(&mut Cursor::new(bytes), &partition).is_ok());
        }
    }

    #[test]
    fn rejects_missing_signature_short_reads_and_invalid_geometry() {
        assert!(verify_header(&mut Cursor::new(vec![0; 8192]), &partition()).is_err());
        assert!(verify_header(&mut Cursor::new(vec![0; 4100]), &partition()).is_err());
        let mut invalid = partition();
        invalid.sector_size = 0;
        assert!(verify_header(&mut Cursor::new(vec![0; 8192]), &invalid).is_err());
        invalid = partition();
        invalid.offset = 1;
        assert!(verify_header(&mut Cursor::new(vec![0; 8192]), &invalid).is_err());
    }

    #[test]
    fn reads_empty_inventory() {
        let inventory: Inventory =
            serde_json::from_str(r#"{"DiskCount":2,"Partitions":[],"Warnings":[]}"#).unwrap();
        assert_eq!(inventory.disk_count, 2);
        assert!(inventory.partitions.is_empty());
    }
}
