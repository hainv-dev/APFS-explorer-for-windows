use apfs_core::{ApfsContainer, dir, volume::ApfsVolume};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Default)]
pub struct Transfer {
    pub completed: AtomicU64,
    pub total: AtomicU64,
    pub started: std::sync::Mutex<Option<std::time::Instant>>,
    pub cancelled: AtomicBool,
    pub conflicts: Option<std::sync::mpsc::Sender<ConflictRequest>>,
    pub skipped: AtomicU64,
    pub files_total: AtomicU64,
    pub files_handled: AtomicU64,
    pub files_counted: AtomicBool,
    pub folder_started: std::sync::Mutex<Option<std::time::Instant>>,
    policies: std::sync::Mutex<[Option<bool>; 2]>,
    normalize_all: AtomicBool,
}

pub struct ConflictRequest {
    pub path: std::path::PathBuf,
    pub directory: bool,
    pub invalid_name: Option<String>,
    pub reply: std::sync::mpsc::Sender<ConflictAnswer>,
}

pub struct ConflictAnswer {
    pub proceed: Option<bool>,
    pub apply_all: bool,
    pub rename: Option<String>,
    pub auto_normalize: bool,
}

impl Transfer {
    pub fn with_conflicts(sender: std::sync::mpsc::Sender<ConflictRequest>) -> Self {
        Self {
            conflicts: Some(sender),
            ..Self::default()
        }
    }
    fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(
                "Extraction cancelled; incomplete file removed. Previously copied files were kept."
                    .into(),
            )
        } else {
            Ok(())
        }
    }
}

pub struct PartitionReader<R> {
    inner: R,
    base: u64,
    length: u64,
    position: u64,
    sector_size: usize,
}

impl<R: Read + Seek> Read for PartitionReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = (self.length - self.position).min(buffer.len() as u64) as usize;
        if count == 0 {
            return Ok(0);
        }
        let sector = self.sector_size as u64;
        let aligned = self.position / sector * sector;
        let skip = (self.position - aligned) as usize;
        let count = count.min(1024 * 1024 - skip);
        let read_length = (skip + count).div_ceil(self.sector_size) * self.sector_size;
        if aligned + read_length as u64 > self.length {
            return Err(io::Error::other("Unaligned partition boundary"));
        }
        self.inner.seek(SeekFrom::Start(
            self.base
                .checked_add(aligned)
                .ok_or_else(|| io::Error::other("Offset overflow"))?,
        ))?;
        let mut aligned_buffer = vec![0; read_length];
        self.inner.read_exact(&mut aligned_buffer)?;
        buffer[..count].copy_from_slice(&aligned_buffer[skip..skip + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl<R: Read + Seek> Seek for PartitionReader<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
            SeekFrom::End(value) => i128::from(self.length) + i128::from(value),
        };
        if position < 0 || position > i128::from(self.length) {
            return Err(io::Error::other("Seek outside partition"));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

pub(crate) fn windows_component(name: &str) -> Result<String, String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err("Invalid directory entry name".into());
    }
    let mut output: String = name
        .chars()
        .map(|character| {
            if character.is_control() || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    output = output.trim_end_matches([' ', '.']).to_owned();
    if output.is_empty() {
        return Err("Empty Windows filename after normalization".into());
    }
    let stem = output.split('.').next().unwrap_or("").to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2"
                    | "3"
                    | "4"
                    | "5"
                    | "6"
                    | "7"
                    | "8"
                    | "9"
                    | "\u{b9}"
                    | "\u{b2}"
                    | "\u{b3}"
            )
        })
    }) {
        output.insert(0, '_');
    }
    Ok(output)
}

pub(crate) fn exact_component(name: &str) -> Result<String, String> {
    let normalized = windows_component(name)?;
    if normalized != name {
        return Err(format!(
            "Cannot preserve this name on Windows: {name:?}. Choose a Windows-safe name or skip this entry."
        ));
    }
    Ok(normalized)
}

fn destination_metadata(path: &std::path::Path) -> Result<Option<std::fs::Metadata>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(format!(
                        "Reparse-point destination refused: {}",
                        path.display()
                    ));
                }
            }
            if metadata.file_type().is_symlink() {
                return Err("Symbolic-link destination refused".into());
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn conflict_choice(
    path: &std::path::Path,
    directory: bool,
    transfer: &Transfer,
) -> Result<bool, String> {
    transfer.check()?;
    let index = usize::from(directory);
    if let Some(choice) = transfer
        .policies
        .lock()
        .map_err(|_| "Conflict policy unavailable")?[index]
    {
        return Ok(choice);
    }
    let (reply, receiver) = std::sync::mpsc::channel();
    transfer
        .conflicts
        .as_ref()
        .ok_or("Conflict requires a UI decision")?
        .send(ConflictRequest {
            path: path.to_path_buf(),
            directory,
            invalid_name: None,
            reply,
        })
        .map_err(|_| "Conflict UI closed")?;
    loop {
        transfer.check()?;
        match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(answer) => {
                let Some(choice) = answer.proceed else {
                    transfer.cancelled.store(true, Ordering::Relaxed);
                    return Err("Extraction cancelled. Previously copied files were kept.".into());
                };
                if answer.apply_all {
                    transfer
                        .policies
                        .lock()
                        .map_err(|_| "Conflict policy unavailable")?[index] = Some(choice);
                }
                transfer.check()?;
                return Ok(choice);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err("Conflict UI closed; extraction stopped.".into()),
        }
    }
}

fn choose_component(
    name: &str,
    parent: &std::path::Path,
    directory: bool,
    transfer: &Transfer,
) -> Result<Option<String>, String> {
    transfer.check()?;
    if let Ok(component) = exact_component(name) {
        return Ok(Some(component));
    }
    if transfer.normalize_all.load(Ordering::Relaxed) {
        if let Ok(component) = windows_component(name) {
            return Ok(Some(component));
        }
    }
    let (reply, receiver) = std::sync::mpsc::channel();
    transfer
        .conflicts
        .as_ref()
        .ok_or("Invalid filename requires a UI decision")?
        .send(ConflictRequest {
            path: parent.to_path_buf(),
            directory,
            invalid_name: Some(name.to_owned()),
            reply,
        })
        .map_err(|_| "Conflict UI closed")?;
    loop {
        transfer.check()?;
        match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(answer) => {
                transfer.check()?;
                match answer.proceed {
                    Some(true) => {
                        if answer.auto_normalize {
                            let component = windows_component(name)?;
                            if answer.apply_all {
                                transfer.normalize_all.store(true, Ordering::Relaxed);
                            }
                            return Ok(Some(component));
                        }
                        return exact_component(answer.rename.as_deref().unwrap_or("")).map(Some);
                    }
                    Some(false) => return Ok(None),
                    None => {
                        transfer.cancelled.store(true, Ordering::Relaxed);
                        return Err(
                            "Extraction cancelled. Previously copied files were kept.".into()
                        );
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err("Conflict UI closed; extraction stopped.".into()),
        }
    }
}

fn prepare_directory(path: &std::path::Path, transfer: &Transfer) -> Result<bool, String> {
    if let Some(metadata) = destination_metadata(path)? {
        if !metadata.is_dir() {
            return Err(format!(
                "A file occupies the destination folder path: {}",
                path.display()
            ));
        }
        conflict_choice(path, true, transfer)
    } else {
        std::fs::create_dir(path).map_err(|error| error.to_string())?;
        Ok(true)
    }
}

fn resolve_file_size(size: Option<u64>, flags: u32, has_extents: bool) -> Result<u64, String> {
    if flags & 0x40000000 != 0 {
        return Err("Dataless file: contents are not available locally.".into());
    }
    if flags & 0x20 != 0 {
        return Err("Compressed files are not supported.".into());
    }
    match size {
        Some(size) => Ok(size),
        None if !has_extents => Ok(0),
        None => Err("Missing file size but data extents exist; refusing to truncate file.".into()),
    }
}

pub struct Volume {
    pub metadata: ApfsVolume,
    pub encrypted: bool,
}

pub struct Browser {
    reader: PartitionReader<File>,
    pub volumes: Vec<Volume>,
    block_size: usize,
    partition: crate::drives::Partition,
}

fn unsupported_entry(flags: u64) -> bool {
    !matches!(flags & 15, 4 | 8)
}

#[derive(Default)]
struct FolderScan {
    counts: std::collections::HashMap<u64, u64>,
    result: Option<Result<(), String>>,
    waiting_for_count: bool,
}

fn wait_for_folder_count(
    scan: &std::sync::Condvar,
    state: &std::sync::Mutex<FolderScan>,
    oid: u64,
    transfer: &Transfer,
) -> Result<u64, String> {
    let mut state = state.lock().map_err(|_| "Folder scan unavailable")?;
    state.waiting_for_count = true;
    scan.notify_all();
    loop {
        if let Err(error) = transfer.check() {
            state.waiting_for_count = false;
            return Err(error);
        }
        if let Some(Err(error)) = &state.result {
            let error = error.clone();
            state.waiting_for_count = false;
            return Err(error);
        }
        if let Some(&count) = state.counts.get(&oid) {
            state.waiting_for_count = false;
            return Ok(count);
        }
        if state.result.is_some() {
            state.waiting_for_count = false;
            return Err("Folder was not counted".into());
        }
        state = scan
            .wait_timeout(state, std::time::Duration::from_millis(100))
            .map_err(|_| "Folder scan unavailable")?
            .0;
    }
}

impl Browser {
    fn count_folder_files(
        &mut self,
        volume: usize,
        root: u64,
        transfer: &Transfer,
        shared: &(std::sync::Mutex<FolderScan>, std::sync::Condvar),
        stop: &AtomicBool,
    ) -> Result<(), String> {
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![(root, 0_usize, None)];
        while let Some((directory, depth, children)) = stack.pop() {
            transfer.check()?;
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let mut state = shared.0.lock().map_err(|_| "Folder scan unavailable")?;
            while !state.waiting_for_count
                && transfer
                    .files_total
                    .load(Ordering::Relaxed)
                    .saturating_sub(transfer.files_handled.load(Ordering::Relaxed))
                    >= 4096
            {
                transfer.check()?;
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                state = shared
                    .1
                    .wait_timeout(state, std::time::Duration::from_millis(100))
                    .map_err(|_| "Folder scan unavailable")?
                    .0;
            }
            drop(state);
            if let Some((mut count, children)) = children {
                let mut state = shared.0.lock().map_err(|_| "Folder scan unavailable")?;
                for child in children {
                    count += state.counts[&child];
                }
                state.counts.insert(directory, count);
                shared.1.notify_all();
            } else {
                if depth > 64 || !visited.insert(directory) {
                    return Err("Directory cycle or depth limit reached.".into());
                }
                let mut children = Vec::new();
                let mut direct_files = 0;
                for entry in self.list(volume, directory)? {
                    match entry.flags & 15 {
                        4 => children.push(entry.file_id),
                        8 => direct_files += 1,
                        _ => {}
                    }
                }
                transfer
                    .files_total
                    .fetch_add(direct_files, Ordering::Relaxed);
                shared.1.notify_all();
                stack.push((directory, depth, Some((direct_files, children.clone()))));
                for child in children {
                    stack.push((child, depth + 1, None));
                }
            }
        }
        Ok(())
    }

    pub fn extract_folder(
        &mut self,
        volume: usize,
        oid: u64,
        name: &str,
        parent: &std::path::Path,
        transfer: &Transfer,
    ) -> Result<Option<std::path::PathBuf>, String> {
        transfer.check()?;
        let Some(component) = choose_component(name, parent, true, transfer)? else {
            transfer.skipped.store(1, Ordering::Relaxed);
            return Ok(None);
        };
        let destination = parent.join(component);
        if !prepare_directory(&destination, transfer)? {
            transfer.skipped.store(1, Ordering::Relaxed);
            return Ok(None);
        }
        *transfer.folder_started.lock().unwrap() = Some(std::time::Instant::now());
        let mut scanner = Browser::open(&self.partition)?;
        let shared = (
            std::sync::Mutex::new(FolderScan::default()),
            std::sync::Condvar::new(),
        );
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let result = scanner.count_folder_files(volume, oid, transfer, &shared, &stop);
                let mut state = shared.0.lock().unwrap();
                if result.is_ok() && !stop.load(Ordering::Relaxed) {
                    transfer.files_counted.store(true, Ordering::Relaxed);
                }
                state.result = Some(result);
                shared.1.notify_all();
            });
            let result = (|| {
                let mut state = shared.0.lock().map_err(|_| "Folder scan unavailable")?;
                while transfer.files_total.load(Ordering::Relaxed) < 1000 && state.result.is_none()
                {
                    transfer.check()?;
                    state = shared
                        .1
                        .wait_timeout(state, std::time::Duration::from_millis(100))
                        .map_err(|_| "Folder scan unavailable")?
                        .0;
                }
                if let Some(Err(error)) = &state.result {
                    return Err(error.clone());
                }
                drop(state);
                *transfer.folder_started.lock().unwrap() = Some(std::time::Instant::now());
                let mut stack = vec![(oid, destination.clone(), 0_usize)];
                let mut skipped = 0_u64;
                let mut visited = std::collections::HashSet::new();
                while let Some((directory, destination, depth)) = stack.pop() {
                    transfer.check()?;
                    if depth > 64 || !visited.insert(directory) {
                        return Err("Directory cycle or depth limit reached.".into());
                    }
                    transfer.completed.store(0, Ordering::Relaxed);
                    transfer.total.store(0, Ordering::Relaxed);
                    let entries = self.list(volume, directory)?;
                    let mut names = std::collections::HashSet::new();
                    for entry in entries {
                        transfer.check()?;
                        if unsupported_entry(u64::from(entry.flags)) {
                            skipped += 1;
                            continue;
                        }
                        let Some(component) = choose_component(
                            &entry.name,
                            &destination,
                            entry.flags & 15 == 4,
                            transfer,
                        )?
                        else {
                            skipped += 1;
                            transfer.files_handled.fetch_add(
                                if entry.flags & 15 == 4 {
                                    wait_for_folder_count(
                                        &shared.1,
                                        &shared.0,
                                        entry.file_id,
                                        transfer,
                                    )?
                                } else {
                                    u64::from(entry.flags & 15 == 8)
                                },
                                Ordering::Relaxed,
                            );
                            continue;
                        };
                        if !names.insert(component.to_uppercase()) {
                            return Err(format!("Windows filename collision: {}", entry.name));
                        }
                        let path = destination.join(component);
                        match entry.flags & 15 {
                            4 => {
                                if prepare_directory(&path, transfer)? {
                                    stack.push((entry.file_id, path, depth + 1));
                                } else {
                                    skipped += 1;
                                    transfer.files_handled.fetch_add(
                                        wait_for_folder_count(
                                            &shared.1,
                                            &shared.0,
                                            entry.file_id,
                                            transfer,
                                        )?,
                                        Ordering::Relaxed,
                                    );
                                }
                            }
                            8 => {
                                if let Some(metadata) = destination_metadata(&path)? {
                                    if !metadata.is_file() {
                                        return Err(format!(
                                            "Destination is not a regular file: {}",
                                            path.display()
                                        ));
                                    }
                                    if !conflict_choice(&path, false, transfer)? {
                                        skipped += 1;
                                        transfer.files_handled.fetch_add(1, Ordering::Relaxed);
                                        continue;
                                    }
                                    let staging = tempfile::tempdir_in(
                                        path.parent().ok_or("Missing parent")?,
                                    )
                                    .map_err(|error| error.to_string())?;
                                    let staged = staging.path().join("content");
                                    self.extract_stream(
                                        volume,
                                        entry.file_id,
                                        &staged,
                                        transfer,
                                        None,
                                    )?;
                                    transfer.check()?;
                                    if destination_metadata(&path)?
                                        .is_some_and(|metadata| !metadata.is_file())
                                    {
                                        return Err(
                                            "Destination changed type during extraction.".into()
                                        );
                                    }
                                    std::fs::rename(&staged, &path)
                                        .map_err(|error| error.to_string())?;
                                } else {
                                    self.extract_stream(
                                        volume,
                                        entry.file_id,
                                        &path,
                                        transfer,
                                        None,
                                    )
                                    .map_err(|error| format!("{}: {error}", entry.name))?;
                                }
                                transfer.files_handled.fetch_add(1, Ordering::Relaxed);
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                transfer.check()?;
                let _ = wait_for_folder_count(&shared.1, &shared.0, oid, transfer)?;
                transfer.skipped.store(skipped, Ordering::Relaxed);
                Ok(Some(destination))
            })();
            stop.store(true, Ordering::Relaxed);
            result
        })
    }

    pub fn file_sizes(
        &mut self,
        volume_index: usize,
        entries: &[dir::DirEntry],
    ) -> std::collections::HashMap<u64, Result<u64, String>> {
        let mut sizes = std::collections::HashMap::new();
        let Some(volume) = self.volumes.get(volume_index) else {
            return sizes;
        };
        for entry in entries.iter().filter(|entry| entry.flags & 15 == 8) {
            let size = dir::load_inode(
                &mut self.reader,
                &volume.metadata,
                entry.file_id,
                self.block_size,
            )
            .map_err(|error| error.to_string())
            .and_then(|inode| {
                if inode.bsd_flags & 0x20 != 0 {
                    return Err("Compressed logical size not available".into());
                }
                let has_extents = if inode.size.is_none() {
                    if apfs_core::xattr::decmpfs_header(
                        &mut self.reader,
                        &volume.metadata,
                        entry.file_id,
                        self.block_size,
                    )
                    .map_err(|error| error.to_string())?
                    .is_some()
                    {
                        return Err("Compressed logical size not available".into());
                    }
                    !apfs_core::extent::list_extents(
                        &mut self.reader,
                        &volume.metadata,
                        inode.private_id,
                        self.block_size,
                    )
                    .map_err(|error| error.to_string())?
                    .is_empty()
                } else {
                    false
                };
                resolve_file_size(inode.size, inode.bsd_flags, has_extents)
            });
            sizes.insert(entry.file_id, size);
        }
        sizes
    }
    pub fn extract(
        &mut self,
        volume_index: usize,
        oid: u64,
        destination: &std::path::Path,
    ) -> Result<(), String> {
        self.extract_stream(
            volume_index,
            oid,
            destination,
            &Transfer::default(),
            Some(64 * 1024 * 1024),
        )
    }

    pub fn extract_stream(
        &mut self,
        volume_index: usize,
        oid: u64,
        destination: &std::path::Path,
        transfer: &Transfer,
        limit: Option<u64>,
    ) -> Result<(), String> {
        transfer.check()?;
        let volume = self.volumes.get(volume_index).ok_or("Volume not found")?;
        if volume.encrypted {
            return Err("Encrypted volumes are not supported.".into());
        }
        let inode = dir::load_inode(&mut self.reader, &volume.metadata, oid, self.block_size)
            .map_err(|error| error.to_string())?;
        if inode.mode & 0xf000 != 0x8000 {
            return Err("Only regular files can be extracted.".into());
        }
        if inode.bsd_flags & 0x20 != 0 {
            return Err("Compressed files are not supported in this preview.".into());
        }
        if apfs_core::xattr::decmpfs_header(
            &mut self.reader,
            &volume.metadata,
            oid,
            self.block_size,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Compressed files are not supported in this preview.".into());
        }
        let extents = apfs_core::extent::list_extents(
            &mut self.reader,
            &volume.metadata,
            inode.private_id,
            self.block_size,
        )
        .map_err(|error| error.to_string())?;
        let size = resolve_file_size(inode.size, inode.bsd_flags, !extents.is_empty())?;
        if limit.is_some_and(|limit| size > limit) {
            return Err("File too large to open. Maximum supported size is 64 MiB.".into());
        }
        let runs: Vec<_> = extents
            .iter()
            .map(|extent| (extent.logical_offset, extent.len, extent.phys_block_num))
            .collect();
        let source_length = self.reader.length;
        stream_to_new_file(
            &mut self.reader,
            destination,
            &runs,
            size,
            self.block_size as u64,
            source_length,
            transfer,
        )
    }

    pub fn open(partition: &crate::drives::Partition) -> Result<Self, String> {
        if !(512..=65536).contains(&partition.sector_size)
            || !partition.sector_size.is_power_of_two()
            || !partition
                .offset
                .is_multiple_of(u64::from(partition.sector_size))
            || !partition
                .size
                .is_multiple_of(u64::from(partition.sector_size))
        {
            return Err("Invalid physical partition alignment".into());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(3);
        }
        let file = options
            .open(format!(r"\\.\PhysicalDrive{}", partition.disk_number))
            .map_err(|error| error.to_string())?;
        let reader = PartitionReader {
            inner: file,
            base: partition.offset,
            length: partition.size,
            position: 0,
            sector_size: partition.sector_size as usize,
        };
        let mut container = ApfsContainer::open(reader).map_err(|error| error.to_string())?;
        let block_size = container.superblock().block_size as usize;
        let addresses = container
            .volume_superblock_addrs()
            .map_err(|error| error.to_string())?;
        let mut reader = container.into_reader();
        let mut volumes = Vec::new();
        for address in addresses {
            let offset = address
                .checked_mul(block_size as u64)
                .ok_or("Volume offset overflow")?;
            reader
                .seek(SeekFrom::Start(offset))
                .map_err(|error| error.to_string())?;
            let mut block = vec![0; block_size];
            reader
                .read_exact(&mut block)
                .map_err(|error| error.to_string())?;
            let metadata = ApfsVolume::parse(&block).map_err(|error| error.to_string())?;
            let flags = u64::from_le_bytes(
                block[264..272]
                    .try_into()
                    .map_err(|_| "Invalid volume flags")?,
            );
            volumes.push(Volume {
                metadata,
                encrypted: flags & 1 == 0,
            });
        }
        Ok(Self {
            reader,
            volumes,
            block_size,
            partition: partition.clone(),
        })
    }

    pub fn list(&mut self, volume_index: usize, parent: u64) -> Result<Vec<dir::DirEntry>, String> {
        let volume = self.volumes.get(volume_index).ok_or("Volume not found")?;
        if volume.encrypted {
            return Err("Encrypted volumes are not supported.".into());
        }
        let mut entries =
            dir::list_dir(&mut self.reader, &volume.metadata, parent, self.block_size)
                .map_err(|error| error.to_string())?;
        entries.sort_by_key(|entry| (entry.flags & 15 != 4, entry.name.to_lowercase()));
        Ok(entries)
    }
}

#[allow(clippy::too_many_arguments)]
fn stream_to_new_file(
    reader: &mut (impl Read + Seek),
    destination: &std::path::Path,
    runs: &[(u64, u64, u64)],
    size: u64,
    block_size: u64,
    source_length: u64,
    transfer: &Transfer,
) -> Result<(), String> {
    use std::io::Write;
    if !destination.is_absolute() || destination.to_string_lossy().starts_with(r"\\.\") {
        return Err("Choose a regular destination file using the save dialog.".into());
    }
    let parent = destination.parent().ok_or("Missing destination folder")?;
    if destination.exists() {
        return Err("Destination already exists; no files overwritten.".into());
    }
    *transfer.started.lock().unwrap() = Some(std::time::Instant::now());
    transfer.total.store(size, Ordering::Relaxed);
    transfer.completed.store(0, Ordering::Relaxed);
    transfer.check()?;
    let mut end = 0;
    for &(offset, length, physical) in runs {
        if length == 0 || offset < end {
            return Err("Invalid or overlapping file extents.".into());
        }
        end = offset.checked_add(length).ok_or("Extent length overflow")?;
        if offset < size && physical != 0 {
            let start = physical
                .checked_mul(block_size)
                .ok_or("Physical offset overflow")?;
            if start
                .checked_add(length.min(size - offset))
                .is_none_or(|end| end > source_length)
            {
                return Err("File extent exceeds source partition.".into());
            }
        }
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    let mut buffer = vec![0; 4 * 1024 * 1024];
    let mut position = 0;
    let mut index = 0;
    while position < size {
        transfer.check()?;
        while index < runs.len() && runs[index].0 + runs[index].1 <= position {
            index += 1;
        }
        let mut available = size - position;
        let mut source = None;
        if let Some(&(offset, length, physical)) = runs.get(index) {
            if position < offset {
                available = available.min(offset - position);
            } else {
                available = available.min(offset + length - position);
                if physical != 0 {
                    source = Some(
                        physical
                            .checked_mul(block_size)
                            .and_then(|base| base.checked_add(position - offset))
                            .ok_or("Physical offset overflow")?,
                    );
                }
            }
        }
        let count = available.min(buffer.len() as u64) as usize;
        if let Some(source) = source {
            reader
                .seek(SeekFrom::Start(source))
                .map_err(|error| error.to_string())?;
            reader
                .read_exact(&mut buffer[..count])
                .map_err(|error| error.to_string())?;
        } else {
            buffer[..count].fill(0);
        }
        temporary
            .write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
        position += count as u64;
        transfer.completed.store(position, Ordering::Relaxed);
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    transfer.check()?;
    temporary.persist_noclobber(destination).map_err(|error| {
        format!(
            "Cannot save file (existing files are never overwritten): {}",
            error.error
        )
    })?;
    Ok(())
}

#[cfg(test)]
fn save_new_file(destination: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let mut source = io::Cursor::new([&[0_u8][..], bytes].concat());
    let runs = if bytes.is_empty() {
        vec![]
    } else {
        vec![(0, bytes.len() as u64, 1)]
    };
    stream_to_new_file(
        &mut source,
        destination,
        &runs,
        bytes.len() as u64,
        1,
        bytes.len() as u64 + 1,
        &Transfer::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_extraction_skips_special_entry_types() {
        assert!(!unsupported_entry(4));
        assert!(!unsupported_entry(8));
        assert!(unsupported_entry(10));
        assert!(unsupported_entry(2));
        assert!(unsupported_entry(0));
        assert!(unsupported_entry(0x1a));
    }
    #[test]
    fn folder_scanner_publishes_counts_and_errors() {
        let shared = (
            std::sync::Mutex::new(FolderScan::default()),
            std::sync::Condvar::new(),
        );
        let transfer = Transfer::default();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut state = shared.0.lock().unwrap();
                state.counts.insert(5, 12);
                shared.1.notify_all();
            });
            assert_eq!(
                wait_for_folder_count(&shared.1, &shared.0, 5, &transfer).unwrap(),
                12
            );
        });
        shared.0.lock().unwrap().result = Some(Err("Scan failed".into()));
        assert_eq!(
            wait_for_folder_count(&shared.1, &shared.0, 6, &transfer).unwrap_err(),
            "Scan failed"
        );
    }
    #[test]
    fn missing_stream_requires_empty_uncompressed_local_file() {
        assert_eq!(resolve_file_size(None, 0, false).unwrap(), 0);
        assert!(resolve_file_size(None, 0, true).is_err());
        assert!(resolve_file_size(None, 0x20, false).is_err());
        assert!(resolve_file_size(None, 0x40000000, false).is_err());
        assert_eq!(resolve_file_size(Some(100), 0, true).unwrap(), 100);
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("empty.php");
        save_new_file(&path, &[]).unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    }
    #[test]
    fn conflict_policies_are_scoped_and_cancellable() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let transfer = Transfer::with_conflicts(sender);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let request = receiver.recv().unwrap();
                assert!(!request.directory);
                request
                    .reply
                    .send(ConflictAnswer {
                        proceed: Some(true),
                        apply_all: true,
                        rename: None,
                        auto_normalize: false,
                    })
                    .unwrap();
                let request = receiver.recv().unwrap();
                assert!(request.directory);
                request
                    .reply
                    .send(ConflictAnswer {
                        proceed: Some(false),
                        apply_all: true,
                        rename: None,
                        auto_normalize: false,
                    })
                    .unwrap();
            });
            let path = std::path::Path::new("conflict");
            assert!(conflict_choice(path, false, &transfer).unwrap());
            assert!(conflict_choice(path, false, &transfer).unwrap());
            assert!(!conflict_choice(path, true, &transfer).unwrap());
            assert!(!conflict_choice(path, true, &transfer).unwrap());
        });
        assert_eq!(*Transfer::default().policies.lock().unwrap(), [None, None]);
        transfer.cancelled.store(true, Ordering::Relaxed);
        assert!(conflict_choice(std::path::Path::new("conflict"), false, &transfer).is_err());
    }
    #[test]
    fn preserves_exact_folder_names() {
        assert_eq!(exact_component("My Folder").unwrap(), "My Folder");
        for name in ["name.", "name ", "a:b", "NUL.txt", "../escape"] {
            assert!(exact_component(name).is_err());
        }
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join(exact_component("My Folder").unwrap());
        assert!(prepare_directory(&target, &Transfer::default()).unwrap());
        assert!(target.is_dir());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
    }
    #[test]
    fn invalid_names_can_be_renamed_or_skipped() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let transfer = Transfer::with_conflicts(sender);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for (name, decision) in [
                    ("bad:name", (Some(true), Some("good_name"))),
                    ("bad:name", (Some(false), None)),
                    ("bad:name", (Some(true), Some("still:bad"))),
                ] {
                    let request = receiver.recv().unwrap();
                    assert_eq!(request.invalid_name.as_deref(), Some(name));
                    request
                        .reply
                        .send(ConflictAnswer {
                            proceed: decision.0,
                            apply_all: false,
                            rename: decision.1.map(str::to_owned),
                            auto_normalize: false,
                        })
                        .unwrap();
                }
            });
            let parent = std::path::Path::new("parent");
            assert_eq!(
                choose_component("bad:name", parent, false, &transfer).unwrap(),
                Some("good_name".into())
            );
            assert_eq!(
                choose_component("bad:name", parent, false, &transfer).unwrap(),
                None
            );
            assert!(choose_component("bad:name", parent, false, &transfer).is_err());
        });
    }
    #[test]
    fn auto_normalize_is_scoped_to_one_extraction() {
        let parent = std::path::Path::new("parent");
        let (sender, receiver) = std::sync::mpsc::channel();
        let transfer = Transfer::with_conflicts(sender);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let request = receiver.recv().unwrap();
                assert_eq!(request.invalid_name.as_deref(), Some("a:b.txt"));
                request
                    .reply
                    .send(ConflictAnswer {
                        proceed: Some(true),
                        apply_all: true,
                        rename: None,
                        auto_normalize: true,
                    })
                    .unwrap();
                let request = receiver.recv().unwrap();
                assert_eq!(request.invalid_name.as_deref(), Some("..."));
                request
                    .reply
                    .send(ConflictAnswer {
                        proceed: Some(false),
                        apply_all: false,
                        rename: None,
                        auto_normalize: false,
                    })
                    .unwrap();
            });
            assert_eq!(
                choose_component("a:b.txt", parent, false, &transfer).unwrap(),
                Some("a_b.txt".into())
            );
            assert_eq!(
                choose_component("NUL.txt", parent, false, &transfer).unwrap(),
                Some("_NUL.txt".into())
            );
            assert_eq!(
                choose_component("name. ", parent, true, &transfer).unwrap(),
                Some("name".into())
            );
            assert_eq!(
                choose_component("ordinary.txt", parent, false, &transfer).unwrap(),
                Some("ordinary.txt".into())
            );
            assert_eq!(
                choose_component("...", parent, false, &transfer).unwrap(),
                None
            );
        });
        let (sender, receiver) = std::sync::mpsc::channel();
        let fresh_transfer = Transfer::with_conflicts(sender);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let request = receiver.recv().unwrap();
                assert_eq!(request.invalid_name.as_deref(), Some("a:b.txt"));
                request
                    .reply
                    .send(ConflictAnswer {
                        proceed: Some(false),
                        apply_all: false,
                        rename: None,
                        auto_normalize: false,
                    })
                    .unwrap();
            });
            assert_eq!(
                choose_component("a:b.txt", parent, false, &fresh_transfer).unwrap(),
                None
            );
        });
    }
    #[test]
    fn auto_normalize_once_prompts_for_the_next_invalid_name() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let transfer = Transfer::with_conflicts(sender);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for _ in 0..2 {
                    let request = receiver.recv().unwrap();
                    request
                        .reply
                        .send(ConflictAnswer {
                            proceed: Some(true),
                            apply_all: false,
                            rename: None,
                            auto_normalize: true,
                        })
                        .unwrap();
                }
            });
            let parent = std::path::Path::new("parent");
            assert_eq!(
                choose_component("a:b", parent, false, &transfer).unwrap(),
                Some("a_b".into())
            );
            assert_eq!(
                choose_component("c:d", parent, false, &transfer).unwrap(),
                Some("c_d".into())
            );
        });
    }
    #[test]
    fn folder_export_names_are_single_windows_components() {
        for name in ["", ".", "..", "..."] {
            assert!(windows_component(name).is_err());
        }
        for (input, expected) in [
            ("../escape", ".._escape"),
            ("a:b.txt", "a_b.txt"),
            ("NUL.txt", "_NUL.txt"),
            ("COM1", "_COM1"),
            ("LPT\u{b2}", "_LPT\u{b2}"),
            ("name. ", "name"),
            ("normal.txt", "normal.txt"),
        ] {
            let output = windows_component(input).unwrap();
            assert_eq!(output, expected);
            assert_eq!(std::path::Path::new(&output).components().count(), 1);
        }
        assert_eq!(
            windows_component("a:b").unwrap(),
            windows_component("a?b").unwrap()
        );
    }
    #[test]
    fn streams_large_file_with_bounded_reads() {
        struct Source {
            position: u64,
            largest: usize,
        }
        impl Read for Source {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                self.largest = self.largest.max(output.len());
                output.fill(0x5a);
                self.position += output.len() as u64;
                Ok(output.len())
            }
        }
        impl Seek for Source {
            fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
                if let SeekFrom::Start(position) = from {
                    self.position = position;
                    Ok(position)
                } else {
                    Err(io::Error::other("Unexpected seek"))
                }
            }
        }
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("large.bin");
        let size = 65 * 1024 * 1024 + 17;
        let transfer = Transfer::default();
        let mut source = Source {
            position: 0,
            largest: 0,
        };
        stream_to_new_file(
            &mut source,
            &path,
            &[(0, size, 1)],
            size,
            4096,
            size + 4096,
            &transfer,
        )
        .unwrap();
        assert!(source.largest <= 4 * 1024 * 1024);
        assert_eq!(transfer.completed.load(Ordering::Relaxed), size);
        let mut file = File::open(path).unwrap();
        assert_eq!(file.metadata().unwrap().len(), size);
        let mut buffer = vec![0; 65536];
        loop {
            let count = file.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            assert!(buffer[..count].iter().all(|byte| *byte == 0x5a));
        }
    }

    #[test]
    fn sparse_ranges_and_eof_are_correct() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("sparse.bin");
        let mut source = io::Cursor::new(vec![0, 10, 11, 12, 13]);
        stream_to_new_file(
            &mut source,
            &path,
            &[(2, 2, 1), (4, 2, 0), (6, 3, 3)],
            8,
            1,
            5,
            &Transfer::default(),
        )
        .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), [0, 0, 10, 11, 0, 0, 12, 13]);
    }

    #[test]
    fn cancellation_and_read_errors_remove_partial_files() {
        struct Cancelling<'a> {
            transfer: &'a Transfer,
        }
        impl Read for Cancelling<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                bytes.fill(7);
                self.transfer.cancelled.store(true, Ordering::Relaxed);
                Ok(bytes.len())
            }
        }
        impl Seek for Cancelling<'_> {
            fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
                if let SeekFrom::Start(position) = from {
                    Ok(position)
                } else {
                    Err(io::Error::other("seek"))
                }
            }
        }
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("output.bin");
        let transfer = Transfer::default();
        let mut source = Cancelling {
            transfer: &transfer,
        };
        assert!(
            stream_to_new_file(
                &mut source,
                &path,
                &[(0, 9_000_000, 1)],
                9_000_000,
                1,
                9_000_001,
                &transfer
            )
            .unwrap_err()
            .contains("cancelled")
        );
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        assert!(
            stream_to_new_file(
                &mut io::Cursor::new(vec![0; 2]),
                &path,
                &[(0, 100, 1)],
                100,
                1,
                101,
                &Transfer::default()
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        assert!(
            stream_to_new_file(
                &mut io::Cursor::new(vec![0; 10]),
                &path,
                &[(0, 5, 1), (3, 2, 1)],
                6,
                1,
                10,
                &Transfer::default()
            )
            .is_err()
        );
        assert!(!path.exists());
    }
    #[test]
    fn reads_unaligned_bytes_with_sector_alignment() {
        struct Aligned(io::Cursor<Vec<u8>>);
        impl Read for Aligned {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                assert!(self.0.position().is_multiple_of(512));
                assert!(bytes.len().is_multiple_of(512));
                self.0.read(bytes)
            }
        }
        impl Seek for Aligned {
            fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
                self.0.seek(from)
            }
        }
        let bytes: Vec<u8> = (0..2048).map(|index| (index % 251) as u8).collect();
        let mut reader = PartitionReader {
            inner: Aligned(io::Cursor::new(bytes.clone())),
            base: 512,
            length: 1024,
            position: 0,
            sector_size: 512,
        };
        reader.seek(SeekFrom::Start(3)).unwrap();
        let mut output = vec![0; 701];
        reader.read_exact(&mut output).unwrap();
        assert_eq!(output, bytes[515..1216]);
    }
    #[test]
    fn saves_without_overwriting() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("output.bin");
        save_new_file(&path, b"original").unwrap();
        assert!(save_new_file(&path, b"replacement").is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"original");
    }
    #[test]
    fn confines_reads_and_seeks() {
        let mut reader = PartitionReader {
            inner: io::Cursor::new(vec![0, 1, 2, 3, 4]),
            base: 1,
            length: 3,
            position: 0,
            sector_size: 1,
        };
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, vec![1, 2, 3]);
        assert!(reader.seek(SeekFrom::Start(4)).is_err());
        assert!(reader.seek(SeekFrom::End(-4)).is_err());
    }

    #[test]
    #[ignore = "Read-only test on locally connected APFS disks"]
    fn browses_connected_volume() {
        let scan = crate::drives::scan().unwrap();
        assert!(!scan.drives.is_empty(), "No APFS partition connected");
        for drive in scan.drives {
            let mut browser = Browser::open(&drive.partition).unwrap();
            for index in 0..browser.volumes.len() {
                println!(
                    "Volume: {}; encrypted: {}",
                    browser.volumes[index].metadata.name(),
                    browser.volumes[index].encrypted
                );
                if !browser.volumes[index].encrypted {
                    let entries = browser.list(index, 2).unwrap();
                    println!("Root entries: {}", entries.len());
                    if let Some(folder) = entries.iter().find(|entry| entry.flags & 15 == 4) {
                        let children = browser.list(index, folder.file_id).unwrap();
                        println!("Child directory entries: {}", children.len());
                    }
                    let mut extracted = false;
                    for entry in entries.iter().filter(|entry| entry.flags & 15 == 8) {
                        let volume = &browser.volumes[index].metadata;
                        let inode = dir::load_inode(
                            &mut browser.reader,
                            volume,
                            entry.file_id,
                            browser.block_size,
                        )
                        .unwrap();
                        if inode
                            .size
                            .is_some_and(|size| size > 0 && size < 1024 * 1024)
                            && inode.bsd_flags & 0x20 == 0
                        {
                            let folder = tempfile::tempdir().unwrap();
                            let destination = folder.path().join("sample.bin");
                            browser.extract(index, entry.file_id, &destination).unwrap();
                            assert_eq!(
                                std::fs::metadata(destination).unwrap().len(),
                                inode.size.unwrap()
                            );
                            let expected = apfs_core::extent::read_data(
                                &mut browser.reader,
                                &browser.volumes[index].metadata,
                                &inode,
                                browser.block_size,
                            )
                            .unwrap();
                            assert_eq!(
                                std::fs::read(folder.path().join("sample.bin")).unwrap(),
                                expected
                            );
                            extracted = true;
                            break;
                        }
                    }
                    println!("Small-file extraction exercised: {extracted}");
                }
            }
        }
    }
}
