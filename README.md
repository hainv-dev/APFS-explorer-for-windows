# APFS Explorer

<p align="center"><img src="icon.png" alt="APFS Explorer icon" width="160"></p>

A read-only APFS browser for Windows, built with Rust and egui/eframe.
Discover connected Apple File System partitions, browse unencrypted volumes,
and extract files or folders without installing an APFS filesystem driver.

**If APFS Explorer helps you access your files, please give this repository a star!**
Your support helps others discover the project and encourages further development.

> **Preview software:** Back up important data before use. The application opens
> source disks read-only, but it is not a full filesystem integrity checker or a
> guaranteed data-recovery solution. See the limitations below before extracting.

## Download

**No Rust installation or compilation is required to use the prebuilt Windows x64 application.**

- [Browse GitHub Releases](https://github.com/hainv-dev/APFS-explorer-for-windows/releases)
- [Download v0.1.1 Windows x64 ZIP (recommended)](https://github.com/hainv-dev/APFS-explorer-for-windows/releases/download/v0.1.1/APFS-Explorer-v0.1.1-windows-x64.zip)
- [Download v0.1.1 executable](https://github.com/hainv-dev/APFS-explorer-for-windows/releases/download/v0.1.1/apfs_explorer.exe)

1. Download and extract the ZIP to a local folder.
2. Run `apfs_explorer.exe`. Use **Run as administrator** if physical-disk access is denied.
3. Select **Scan drives**, then **Browse volumes** on a verified APFS partition.

Version 0.1.1 is a **pre-release** and includes the new application icon in the
window and Windows executable, with icon sizes from 16 to 256 pixels.
The executable is unsigned, so Windows may
display a security warning. Download only from this repository's Releases page;
SHA256 checksums are included in the release notes.

## Features

- Scan connected Windows disks for APFS GPT partitions and verify NXSB signatures.
- Explorer-style folder tree, breadcrumbs, local folder search and sortable names.
- File-type icons, file sizes and directory-entry timestamps.
- Stream large file extractions in 4 MiB chunks with progress and cancellation.
- Recursively extract folders with original names and explicit conflict decisions.
- In-app Merge / Replace / Skip / Cancel prompts with per-operation Apply to all.
- Open supported images, videos, PDF and text in external applications via temporary copies.
- Inspect the container header of raw APFS partition images.

## Requirements

- Windows with Windows PowerShell Storage cmdlets available.
- Administrator access for physical-disk reads when required by Windows.
- A graphics adapter/driver supported by WGPU; the Windows build enables Direct3D 12.
- Stable Rust and the MSVC build tools for building from source.

## Get the Source

```powershell
git clone https://github.com/hainv-dev/APFS-explorer-for-windows.git
cd APFS-explorer-for-windows
```

## Run

Install stable Rust with the MSVC toolchain and Visual Studio Build Tools
(Desktop development with C++, including the Windows SDK).
Use current stable Rust (tested with Rust 1.94).

```powershell
cargo run
```

Use **Open image...** or drop a raw APFS partition image into the window.
The application opens the image read-only and reads its initial container header.
Cancelling the file picker preserves the currently displayed image.

Use **Scan drives** to detect APFS GPT partitions on disks visible to Windows.
The background scanner uses Windows PowerShell Storage cmdlets (`Get-Disk` and
`Get-Partition`), then opens matching physical drives read-only to check NXSB
at each partition offset. No APFS driver is required.

Results distinguish APFS partition types from verified NXSB signatures.
If access is denied, start the application yourself with **Run as administrator**
and scan again. The application never requests elevation automatically.
Scanning does not mount or modify a physical drive.
Rescan after connecting or disconnecting a drive; results are a point-in-time snapshot.
Offline/inaccessible disks may generate warnings. Non-GPT layouts and disks not
exposed by Windows Storage are not detected. Signature checks are not integrity checks.

## Build

```powershell
cargo build --release
```

Executable: `target\release\apfs_explorer.exe`.

## Current Scope

- Native desktop window, image selection and drag-and-drop.
- Background image reading and visible error reporting.
- APFS NXSB signature, UUID, block size, block count and container/image sizes.
- Basic header geometry and image length checks.

## Browse Physical Volumes

After scanning, select **Browse volumes** on a verified APFS partition.
Select a volume in the left folder tree. Expand folders on demand or double-click
a folder in the right-hand list. Use the parent arrow, Back or clickable path
segments to navigate. Refresh reloads the current folder from the source.
The search field filters the current folder only; clicking Name reverses name sorting.
Select a regular file and use **Extract...**, or use its right-click menu,
then choose a new destination filename on Windows.
The Drives menu switches between scanned partitions. The tree pane is resizable.
Individual file extraction never overwrites an existing destination. Folder
extraction can replace conflicting files only after user approval, as described below.

The preview supports unencrypted volumes and individual uncompressed regular files
without an application-imposed size limit for Extract. Compressed files, encrypted volumes,
symlink extraction and metadata/resource-fork preservation are not supported.
Extract streams in 4 MiB chunks with byte progress and Cancel. Sparse regions are
written as zero bytes (not necessarily sparse on the destination). Output is only
published after completion; cancellation and errors remove the temporary output.
Cancellation is checked between chunks and before publication; an in-flight disk
operation or metadata query must finish first. Keep the app open until completion
or cancellation finishes. Forced termination may leave temporary output behind.
Destination free space and filesystem file-size limits still apply.
Open file retains its 64 MiB limit. No filesystem is mounted.
Do not unplug or modify the source while browsing; close and reopen after a change.

Navigation uses Apache-2.0 licensed `apfs-core` 0.2.6 for checkpoint, object-map,
B-tree and extent parsing with metadata checksum validation. The integration reads
the encryption flag directly at APSB offset 264 because this version of the library
uses offset 256 for its `fs_flags` accessor; that accessor is not used here.
Physical reads are sector-aligned and confined to the scanned partition.
Checksum validation of visited metadata is not a full filesystem integrity check.

Image opening remains header inspection only: raw single-device APFS partition
images at offset zero. Whole-disk images, compressed DMG and Fusion Drive are unsupported.
There are no source write, format or repair operations.

## Extract Folders

Select a folder in the file list and choose **Extract folder...** in the toolbar
or right-click menu. Choose a destination parent directory. The original folder
name is preserved, without any suffix. Existing folders prompt for Merge, Skip
or Cancel; existing regular files prompt for Replace, Skip or Cancel inside the
application. **Apply to all** remembers the selected Merge/Replace or Skip choice
for the current extraction only. Folder and file conflict policies are separate.
Escape or dismissing the modal cancels extraction; it never approves replacement.
Replacement occurs only after the new file has been fully copied.

Subdirectories and empty directories are preserved. Files use streaming extraction;
the byte progress resets for each file. Cancel applies to the entire folder operation.
Names that cannot be preserved on Windows cause an explicit error; they are not
silently normalized. Case-insensitive source-name collisions abort the operation.
Symlinks/special files, compressed
or encrypted files and directory cycles/depth over 64 are rejected, not skipped.
On error/cancel the current incomplete file is removed. Previously copied files
and created folders remain, including replacements explicitly approved by the user.
Do not modify the output directory during extraction; forced termination or external
file locks can prevent cleanup. Metadata/resource forks are not preserved.

## Open Temporary Copies

Select **Open file**, double-click a supported regular file, or use its context menu.
Images, videos, PDF and plain text are opened in the default Windows application
from a temporary copy, without asking for an extraction destination.
Files over 64 MiB are refused before creating the copy. Unknown sizes and compressed
APFS files remain unsupported. Executables, scripts, HTML and SVG are not launched.
Windows must have an application associated with the selected extension.

Copies are retained across volume changes and cleaned up when APFS Explorer exits
normally, not when the external viewer closes. Close external viewers first:
Windows file locks can prevent deletion. A crash or forced termination can leave
`apfs-preview-*` directories in the Windows temporary folder. Temporary copies
consume disk space and contain the source file's contents.

## Checks

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Optional read-only live Windows scan test:

```powershell
cargo test scans_connected_windows_disks -- --ignored --nocapture
```

Optional live browser test (reads a child directory and, when available, extracts
one small root file into an automatically deleted temporary directory):

```powershell
cargo test browses_connected_volume -- --ignored --nocapture
```

Unit tests cover synthetic headers, invalid signatures, truncated headers/images,
invalid block sizes, zero block counts and capacity overflow.
Real APFS images and interactive Windows workflows require additional testing.

## Credits

- [egui / eframe](https://github.com/emilk/egui) for the native desktop interface.
- [apfs-core](https://crates.io/crates/apfs-core) for APFS metadata and extent parsing.
- [Microsoft Fluent UI System Icons](https://github.com/microsoft/fluentui-system-icons)
	for the embedded SVG icons. Their MIT license is included in
	[assets/fluent/LICENSE](assets/fluent/LICENSE).

Third-party components retain their respective licenses. The Fluent icon license
applies to those assets, not automatically to this application's source code.

## Feedback and Support

Bug reports and reproducible examples are welcome through
[GitHub Issues](https://github.com/hainv-dev/APFS-explorer-for-windows/issues).
Include your Windows version, the operation that failed and the exact error message.
Do not upload private files, disk images, passwords or recovery keys.

**Found it useful? Star [APFS Explorer for Windows](https://github.com/hainv-dev/APFS-explorer-for-windows)**
to support the project and help other Windows users find it.