use crate::{browser::Browser, drives::Partition};
use apfs_core::dir::DirEntry;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver};

type FolderPath = Vec<(u64, String)>;
type Location = (usize, FolderPath);
type Reply = (Option<Browser>, Result<Outcome, String>);

enum Outcome {
    Directory(Location, Vec<DirEntry>, HashMap<u64, Result<u64, String>>),
    Saved(String),
}

#[derive(Default)]
pub struct BrowserPanel {
    reader: Option<Browser>,
    pending: Option<Receiver<Reply>>,
    volumes: Vec<(String, bool)>,
    location: Option<Location>,
    entries: Vec<DirEntry>,
    sizes: HashMap<u64, Result<u64, String>>,
    cache: HashMap<(usize, u64), Vec<DirEntry>>,
    expanded: HashSet<(usize, u64)>,
    history: Vec<Location>,
    going_back: bool,
    selected: Option<u64>,
    query: String,
    descending: bool,
    message: String,
    error: Option<String>,
    transfer: Option<std::sync::Arc<crate::browser::Transfer>>,
    conflicts: Option<Receiver<crate::browser::ConflictRequest>>,
    conflict: Option<crate::browser::ConflictRequest>,
    apply_all: bool,
    temporary_files: std::sync::Arc<std::sync::Mutex<Vec<tempfile::TempDir>>>,
}

impl BrowserPanel {
    pub fn active(&self) -> bool {
        self.location.is_some() || self.busy() || self.error.is_some()
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    fn launch(&mut self, context: &egui::Context, work: impl FnOnce() -> Reply + Send + 'static) {
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.error = None;
        let context = context.clone();
        std::thread::spawn(move || {
            let _ = sender.send(work());
            context.request_repaint();
        });
    }

    pub fn open(&mut self, context: &egui::Context, partition: Partition) {
        if self.busy() {
            return;
        }
        let temporary_files = self.temporary_files.clone();
        *self = Self {
            temporary_files,
            ..Self::default()
        };
        self.launch(context, move || match Browser::open(&partition) {
            Ok(mut reader) => {
                let index = reader
                    .volumes
                    .iter()
                    .position(|volume| !volume.encrypted)
                    .unwrap_or(0);
                let name = reader
                    .volumes
                    .get(index)
                    .map_or("Volume", |volume| volume.metadata.name())
                    .to_owned();
                let result = reader.list(index, 2).map(|entries| {
                    let sizes = reader.file_sizes(index, &entries);
                    Outcome::Directory((index, vec![(2, name)]), entries, sizes)
                });
                (Some(reader), result)
            }
            Err(error) => (None, Err(error)),
        });
    }

    pub fn poll(&mut self) {
        if self.conflict.is_none()
            && let Some(receiver) = &self.conflicts
            && let Ok(request) = receiver.try_recv()
        {
            self.conflict = Some(request);
            self.apply_all = false;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok((reader, result)) => {
                self.transfer = None;
                self.conflict = None;
                self.conflicts = None;
                self.pending = None;
                self.reader = reader;
                if let Some(reader) = &self.reader {
                    self.volumes = reader
                        .volumes
                        .iter()
                        .map(|volume| (volume.metadata.name().to_owned(), volume.encrypted))
                        .collect();
                }
                match result {
                    Ok(Outcome::Directory(location, entries, sizes)) => {
                        self.sizes = sizes;
                        if let Some(previous) = self.location.replace(location.clone())
                            && previous != location
                            && !self.going_back
                        {
                            self.history.push(previous);
                        }
                        for (oid, _) in &location.1 {
                            self.expanded.insert((location.0, *oid));
                        }
                        if let Some((oid, _)) = location.1.last() {
                            self.cache.insert((location.0, *oid), entries.clone());
                        }
                        self.entries = entries;
                        self.selected = None;
                        self.query.clear();
                        self.message = "Ready".into();
                    }
                    Ok(Outcome::Saved(message)) => self.message = message,
                    Err(error) => self.error = Some(error),
                }
                self.going_back = false;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.transfer = None;
                self.conflict = None;
                self.conflicts = None;
                self.pending = None;
                self.error = Some("Reader stopped. Close the volume and reopen it.".into());
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn navigate(&mut self, context: &egui::Context, location: Location) {
        let Some(mut reader) = self.reader.take() else {
            return;
        };
        self.launch(context, move || {
            let parent = location.1.last().map_or(2, |entry| entry.0);
            let result = reader.list(location.0, parent).map(|entries| {
                let sizes = reader.file_sizes(location.0, &entries);
                Outcome::Directory(location, entries, sizes)
            });
            (Some(reader), result)
        });
    }

    fn show_conflict(&mut self, context: &egui::Context) {
        let Some(request) = &self.conflict else {
            return;
        };
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::new("extract_conflict")).show(context, |ui| {
            ui.set_width((context.screen_rect().width() - 64.0).clamp(240.0, 480.0));
            ui.heading(if request.directory {
                "Folder already exists"
            } else {
                "File already exists"
            });
            ui.add_space(10.0);
            ui.add(egui::Label::new(request.path.display().to_string()).wrap());
            ui.add_space(10.0);
            ui.label(if request.directory {
                "Merge folders? Conflicting files will be handled separately."
            } else {
                "Replace the destination file after the new copy is complete?"
            });
            ui.checkbox(
                &mut self.apply_all,
                if request.directory {
                    "Apply to all folder conflicts in this extraction"
                } else {
                    "Apply to all file conflicts in this extraction"
                },
            );
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(if request.directory {
                        "Merge"
                    } else {
                        "Replace"
                    })
                    .clicked()
                {
                    decision = Some(Some(true));
                }
                if ui.button("Skip").clicked() {
                    decision = Some(Some(false));
                }
                if ui.button("Cancel extraction").clicked() {
                    decision = Some(None);
                }
            });
        });
        if modal.should_close() && decision.is_none() {
            decision = Some(None);
        }
        if let Some(proceed) = decision
            && let Some(request) = self.conflict.take()
        {
            let _ = request.reply.send(crate::browser::ConflictAnswer {
                proceed,
                apply_all: self.apply_all,
            });
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        self.show_conflict(context);
        let busy = self.busy();
        let mut target = None;
        let mut extraction = None;
        let mut opening = None;
        let mut close = false;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !busy && !self.history.is_empty(),
                    nav_button(egui::include_image!("../assets/fluent/back.svg")),
                )
                .on_hover_text("Back")
                .clicked()
            {
                target = self.history.pop();
                self.going_back = true;
            }
            let can_up = self
                .location
                .as_ref()
                .is_some_and(|location| location.1.len() > 1);
            if ui
                .add_enabled(
                    !busy && can_up,
                    nav_button(egui::include_image!("../assets/fluent/up.svg")),
                )
                .on_hover_text("Parent folder")
                .clicked()
            {
                target = self.location.clone().map(|(volume, mut path)| {
                    path.pop();
                    (volume, path)
                });
            }
            if ui
                .add_enabled(
                    !busy && self.location.is_some(),
                    nav_button(egui::include_image!("../assets/fluent/refresh.svg")),
                )
                .on_hover_text("Refresh folder")
                .clicked()
            {
                target = self.location.clone();
            }
            let address_width = ui.available_width();
            egui::Frame::NONE
                .fill(egui::Color32::WHITE)
                .stroke(egui::Stroke::new(
                    1.0,
                    egui::Color32::from_rgb(218, 224, 232),
                ))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(10, 3))
                .show(ui, |ui| {
                    ui.set_width((address_width - 22.0).max(80.0));
                    ui.set_height(28.0);
                    ui.spacing_mut().interact_size.y = 28.0;
                    ui.spacing_mut().button_padding = egui::vec2(6.0, 3.0);
                    egui::ScrollArea::horizontal()
                        .id_salt("address_path")
                        .max_height(28.0)
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), 28.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    if let Some((volume, path)) = &self.location {
                                        for (index, (_, name)) in path.iter().enumerate() {
                                            if index > 0 {
                                                ui.add(
                                                    egui::Image::new(egui::include_image!(
                                                        "../assets/fluent/chevron_right.svg"
                                                    ))
                                                    .fit_to_exact_size(egui::vec2(10.0, 10.0)),
                                                );
                                            }
                                            if ui
                                                .add_enabled(
                                                    !busy,
                                                    egui::Button::new(
                                                        egui::RichText::new(name).strong(),
                                                    )
                                                    .frame(false)
                                                    .min_size(egui::vec2(0.0, 28.0)),
                                                )
                                                .clicked()
                                            {
                                                target = Some((*volume, path[..=index].to_vec()));
                                            }
                                        }
                                    } else {
                                        ui.label("Volumes");
                                    }
                                },
                            );
                        });
                });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(10.0, 6.0);
            ui.spacing_mut().interact_size.y = 34.0;
            let selected = self.entries.iter().find(|entry| {
                Some(entry.file_id) == self.selected && matches!(entry.flags & 15, 4 | 8)
            });
            if ui
                .add_enabled(
                    !busy
                        && selected.is_some_and(|entry| {
                            entry.flags & 15 == 8 && preview_extension(&entry.name).is_some()
                        }),
                    action_button(
                        egui::include_image!("../assets/fluent/open_file.svg"),
                        "Open file",
                    ),
                )
                .on_hover_text(
                    "Open a temporary copy in the default Windows application (up to 64 MiB)",
                )
                .clicked()
            {
                opening = selected.map(|entry| (entry.file_id, entry.name.clone()));
            }
            if ui
                .add_enabled(
                    !busy && selected.is_some(),
                    action_button(
                        egui::include_image!("../assets/fluent/extract.svg"),
                        if selected.is_some_and(|entry| entry.flags & 15 == 4) {
                            "Extract folder..."
                        } else {
                            "Extract file..."
                        },
                    ),
                )
                .clicked()
            {
                extraction = selected
                    .map(|entry| (entry.file_id, entry.name.clone(), entry.flags & 15 == 4));
            }
            egui::menu::menu_custom_button(
                ui,
                action_button(
                    egui::include_image!("../assets/fluent/volume.svg"),
                    "Volume",
                ),
                |ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Close volume"))
                        .clicked()
                    {
                        close = true;
                        ui.close_menu();
                    }
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.query.is_empty()
                    && ui
                        .button("\u{00d7}")
                        .on_hover_text("Clear search")
                        .clicked()
                {
                    self.query.clear();
                }
                let search_width = (ui.available_width() - 60.0).clamp(100.0, 280.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search this folder")
                        .desired_width(search_width)
                        .margin(egui::vec2(10.0, 7.0)),
                );
                if busy {
                    ui.spinner();
                }
            });
        });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::from_rgb(170, 40, 40), error);
        }
        if let Some(transfer) = &self.transfer {
            use std::sync::atomic::Ordering;
            let completed = transfer.completed.load(Ordering::Relaxed);
            let total = transfer.total.load(Ordering::Relaxed);
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(if total == 0 {
                        0.0
                    } else {
                        completed as f32 / total as f32
                    })
                    .desired_width((ui.available_width() - 120.0).max(80.0))
                    .text(if total == 0 {
                        "Preparing extraction...".into()
                    } else {
                        format!(
                            "Current file: {} / {}",
                            format_size(completed),
                            format_size(total)
                        )
                    }),
                );
                if ui
                    .add_enabled(
                        !transfer.cancelled.load(Ordering::Relaxed),
                        egui::Button::new("Cancel"),
                    )
                    .clicked()
                {
                    transfer.cancelled.store(true, Ordering::Relaxed);
                }
            });
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }
        ui.separator();
        egui::TopBottomPanel::bottom("browser_footer").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let matches = self
                    .entries
                    .iter()
                    .filter(|entry| {
                        entry
                            .name
                            .to_lowercase()
                            .contains(&self.query.to_lowercase())
                    })
                    .count();
                ui.small(format!("{} of {} items", matches, self.entries.len()));
                if self.selected.is_some() {
                    ui.separator();
                    ui.small("1 selected");
                }
                ui.separator();
                ui.add(egui::Label::new(egui::RichText::new(&self.message).small()).truncate());
            });
        });
        let height = ui.available_height().max(100.0);
        egui::SidePanel::left("folder_tree_v2")
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(245, 247, 250))
                    .inner_margin(12),
            )
            .resizable(true)
            .default_width(275.0)
            .width_range(220.0..=420.0)
            .show_inside(ui, |ui| {
                ui.set_min_height(height);
                ui.label(
                    egui::RichText::new("Connected volumes")
                        .small()
                        .color(egui::Color32::from_gray(100)),
                );
                egui::ScrollArea::both()
                    .id_salt("tree_scroll")
                    .max_height(height - 28.0)
                    .show(ui, |ui| {
                        for (volume, (name, encrypted)) in self.volumes.iter().enumerate() {
                            if *encrypted {
                                ui.label(format!("{name} (locked)"));
                                continue;
                            }
                            tree(
                                ui,
                                volume,
                                &vec![(2, name.clone())],
                                &self.cache,
                                &mut self.expanded,
                                &self.location,
                                busy,
                                &mut target,
                            );
                        }
                    });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::WHITE)
                    .inner_margin(16),
            )
            .show_inside(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let title = self
                        .location
                        .as_ref()
                        .and_then(|location| location.1.last())
                        .map_or("Volumes", |entry| entry.1.as_str());
                    ui.label(egui::RichText::new(title).size(21.0).strong());
                    ui.label(
                        egui::RichText::new(format!("{} items", self.entries.len()))
                            .color(egui::Color32::from_gray(115)),
                    );
                });
                ui.add_space(12.0);
                {
                    let mut entries: Vec<_> = self
                        .entries
                        .iter()
                        .filter(|entry| {
                            entry
                                .name
                                .to_lowercase()
                                .contains(&self.query.to_lowercase())
                        })
                        .collect();
                    entries.sort_by(|left, right| {
                        let folders = (left.flags & 15 != 4).cmp(&(right.flags & 15 != 4));
                        let names = left.name.to_lowercase().cmp(&right.name.to_lowercase());
                        folders.then(if self.descending {
                            names.reverse()
                        } else {
                            names
                        })
                    });
                    if entries.is_empty() && !busy {
                        ui.add_space(30.0);
                        ui.label(if self.query.is_empty() {
                            "This folder is empty"
                        } else {
                            "No matching items"
                        });
                    }
                    let body_height =
                        (ui.available_height() - 32.0 - ui.spacing().item_spacing.y).max(0.0);
                    egui_extras::TableBuilder::new(ui)
                        .id_salt("explorer_details_sizes")
                        .striped(true)
                        .resizable(true)
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .column(egui_extras::Column::remainder().at_least(180.0).clip(true))
                        .column(
                            egui_extras::Column::initial(100.0)
                                .range(75.0..=180.0)
                                .clip(true),
                        )
                        .column(
                            egui_extras::Column::initial(155.0)
                                .range(100.0..=220.0)
                                .clip(true),
                        )
                        .column(
                            egui_extras::Column::initial(95.0)
                                .range(70.0..=160.0)
                                .clip(true),
                        )
                        .min_scrolled_height(0.0)
                        .max_scroll_height(body_height)
                        .header(32.0, |mut header| {
                            header.col(|ui| {
                                if ui
                                    .button(if self.descending {
                                        "Name  \u{2193}"
                                    } else {
                                        "Name  \u{2191}"
                                    })
                                    .clicked()
                                {
                                    self.descending = !self.descending;
                                }
                            });
                            header.col(|ui| {
                                ui.strong("Type");
                            });
                            header.col(|ui| {
                                ui.strong("Date added")
                                    .on_hover_text("Date added to this APFS directory (UTC)");
                            });
                            header.col(|ui| {
                                ui.strong("Size");
                            });
                        })
                        .body(|body| {
                            body.rows(34.0, entries.len(), |mut row| {
                                let entry = entries[row.index()];
                                let folder = entry.flags & 15 == 4;
                                row.set_selected(self.selected == Some(entry.file_id));
                                row.col(|ui| {
                                    let response = item_row(
                                        ui,
                                        &entry.name,
                                        folder,
                                        false,
                                        self.selected == Some(entry.file_id),
                                        ui.available_width(),
                                    );
                                    if response.clicked() {
                                        self.selected = Some(entry.file_id);
                                    }
                                    let location = self.location.as_ref().map(|(volume, path)| {
                                        let mut path = path.clone();
                                        path.push((entry.file_id, entry.name.clone()));
                                        (*volume, path)
                                    });
                                    if response.double_clicked() && folder && !busy {
                                        target = location.clone();
                                    }
                                    if response.double_clicked()
                                        && !folder
                                        && !busy
                                        && preview_extension(&entry.name).is_some()
                                    {
                                        opening = Some((entry.file_id, entry.name.clone()));
                                    }
                                    response.context_menu(|ui| {
                                        if entry.flags & 15 == 8
                                            && preview_extension(&entry.name).is_some()
                                            && ui
                                                .add_enabled(!busy, egui::Button::new("Open file"))
                                                .clicked()
                                        {
                                            opening = Some((entry.file_id, entry.name.clone()));
                                            ui.close_menu();
                                        }
                                        if folder
                                            && ui
                                                .add_enabled(
                                                    !busy,
                                                    egui::Button::new("Open folder"),
                                                )
                                                .clicked()
                                        {
                                            target = location;
                                            ui.close_menu();
                                        }
                                        if matches!(entry.flags & 15, 4 | 8)
                                            && ui
                                                .add_enabled(
                                                    !busy,
                                                    egui::Button::new(if folder {
                                                        "Extract folder..."
                                                    } else {
                                                        "Extract file..."
                                                    }),
                                                )
                                                .clicked()
                                        {
                                            extraction =
                                                Some((entry.file_id, entry.name.clone(), folder));
                                            ui.close_menu();
                                        }
                                    });
                                });
                                row.col(|ui| {
                                    ui.label(entry_type(entry));
                                });
                                row.col(|ui| {
                                    ui.label(added_date(entry.date_added));
                                });
                                row.col(|ui| match self.sizes.get(&entry.file_id) {
                                    Some(Ok(size)) => {
                                        ui.label(format_size(*size))
                                            .on_hover_text(format!("{size} bytes"));
                                    }
                                    Some(Err(error)) => {
                                        ui.label("Unknown").on_hover_text(error);
                                    }
                                    None => {
                                        ui.label("-");
                                    }
                                });
                            })
                        });
                }
            });
        if close {
            let temporary_files = self.temporary_files.clone();
            *self = Self {
                temporary_files,
                ..Self::default()
            };
            return;
        }
        if !busy {
            if let Some(location) = target {
                self.navigate(context, location);
            } else if let Some((oid, name)) = opening {
                let size = self
                    .sizes
                    .get(&oid)
                    .cloned()
                    .unwrap_or_else(|| Err("Cannot determine file size.".into()));
                match size.and_then(check_preview_size) {
                    Err(error) => self.error = Some(error),
                    Ok(()) => {
                        if let (Some(mut reader), Some((volume, _)), Some(extension)) = (
                            self.reader.take(),
                            self.location.clone(),
                            preview_extension(&name),
                        ) {
                            let temporary_files = self.temporary_files.clone();
                            self.launch(context, move || {
                                let result = (|| -> Result<Outcome, String> {
                                    let directory = tempfile::Builder::new()
                                        .prefix("apfs-preview-")
                                        .tempdir()
                                        .map_err(|error| error.to_string())?;
                                    let path =
                                        directory.path().join(format!("preview.{extension}"));
                                    reader.extract(volume, oid, &path)?;
                                    let mut files = temporary_files
                                        .lock()
                                        .map_err(|_| "Temporary file manager unavailable")?;
                                    open::that(&path)
                                        .map_err(|error| format!("Cannot open file: {error}"))?;
                                    files.push(directory);
                                    Ok(Outcome::Saved(
                                        "Opened temporary copy; cleanup on application exit."
                                            .into(),
                                    ))
                                })();
                                (Some(reader), result)
                            });
                        }
                    }
                }
            } else if let Some((oid, name, folder)) = extraction
                && let (Some(mut reader), Some((volume, _))) =
                    (self.reader.take(), self.location.clone())
            {
                let (sender, receiver) = mpsc::channel();
                self.conflicts = Some(receiver);
                let transfer =
                    std::sync::Arc::new(crate::browser::Transfer::with_conflicts(sender));
                self.transfer = Some(transfer.clone());
                self.launch(context, move || {
                    if folder {
                        let result = match rfd::FileDialog::new()
                            .set_title("Choose parent folder for extracted APFS folder")
                            .pick_folder()
                        {
                            Some(parent) => reader
                                .extract_folder(volume, oid, &name, &parent, &transfer)
                                .map(|path| {
                                    Outcome::Saved(format!(
                                        "Folder saved: {} | {} skipped",
                                        path.display(),
                                        transfer.skipped.load(std::sync::atomic::Ordering::Relaxed)
                                    ))
                                }),
                            None => Ok(Outcome::Saved("Extraction cancelled".into())),
                        };
                        return (Some(reader), result);
                    }
                    let result = match rfd::FileDialog::new()
                        .set_title("Extract to a new Windows file")
                        .set_file_name(safe_name(&name))
                        .save_file()
                    {
                        Some(path) => reader
                            .extract_stream(volume, oid, &path, &transfer, None)
                            .map(|()| Outcome::Saved(format!("Saved: {}", path.display()))),
                        None => Ok(Outcome::Saved("Extraction cancelled".into())),
                    };
                    (Some(reader), result)
                });
            }
        }
    }
}

fn check_preview_size(size: u64) -> Result<(), String> {
    if size > 64 * 1024 * 1024 {
        Err("File too large to open. Maximum supported size is 64 MiB.".into())
    } else {
        Ok(())
    }
}

fn preview_extension(name: &str) -> Option<String> {
    let extension = std::path::Path::new(name)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "txt"
            | "log"
            | "md"
            | "csv"
            | "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "bmp"
            | "webp"
            | "tif"
            | "tiff"
            | "heic"
            | "mp4"
            | "mkv"
            | "mov"
            | "avi"
            | "webm"
            | "wmv"
            | "m4v"
            | "pdf"
    )
    .then_some(extension)
}

fn action_button(source: egui::ImageSource<'static>, label: &'static str) -> egui::Button<'static> {
    egui::Button::image_and_text(
        egui::Image::new(source).fit_to_exact_size(egui::vec2(18.0, 18.0)),
        label,
    )
    .min_size(egui::vec2(132.0, 34.0))
    .corner_radius(5)
}

fn nav_button(source: egui::ImageSource<'static>) -> egui::Button<'static> {
    egui::Button::image(egui::Image::new(source).fit_to_exact_size(egui::vec2(18.0, 18.0)))
        .frame(false)
        .min_size(egui::vec2(34.0, 34.0))
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        value /= 1024.0;
        unit = next;
        if value < 1024.0 {
            break;
        }
    }
    format!("{value:.1} {unit}")
}

fn entry_type(entry: &DirEntry) -> String {
    match entry.flags & 15 {
        4 => "Folder".into(),
        10 => "Symbolic link".into(),
        8 => std::path::Path::new(&entry.name)
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| value.len() <= 12)
            .map_or_else(
                || "File".into(),
                |value| format!("{} file", value.to_uppercase()),
            ),
        _ => "Other".into(),
    }
}

fn added_date(timestamp: u64) -> String {
    if timestamp == 0 {
        return "-".into();
    }
    chrono::DateTime::from_timestamp(
        (timestamp / 1_000_000_000) as i64,
        (timestamp % 1_000_000_000) as u32,
    )
    .map_or_else(
        || "-".into(),
        |value| value.format("%Y-%m-%d  %H:%M").to_string(),
    )
}

fn item_row(
    ui: &mut egui::Ui,
    name: &str,
    folder: bool,
    expanded: bool,
    selected: bool,
    width: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            3.0,
            if selected {
                egui::Color32::from_rgb(218, 236, 252)
            } else {
                egui::Color32::from_rgb(237, 242, 247)
            },
        );
    }
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let (file_source, file_color) = file_icon(name);
    let source = if folder && expanded {
        egui::include_image!("../assets/fluent/folder_open.svg")
    } else if folder {
        egui::include_image!("../assets/fluent/folder.svg")
    } else {
        file_source
    };
    egui::Image::new(source)
        .tint(if folder {
            egui::Color32::from_rgb(185, 132, 20)
        } else {
            file_color
        })
        .paint_at(
            ui,
            egui::Rect::from_center_size(
                egui::pos2(rect.left() + 18.0, rect.center().y),
                egui::vec2(20.0, 20.0),
            ),
        );
    let display_name = single_line_name(name);
    let job = name_layout(
        &display_name,
        (rect.width() - 44.0).max(0.0),
        ui.visuals().text_color(),
    );
    let galley = ui.fonts(|fonts| fonts.layout_job(job));
    let position = egui::pos2(rect.left() + 36.0, rect.center().y - galley.size().y / 2.0);
    painter.galley(position, galley, ui.visuals().text_color());
    response.on_hover_text(display_name)
}

fn file_kind(name: &str) -> &'static str {
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "txt" | "md" | "log" | "rtf" | "csv" => "text",
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "svg" | "heic" | "tif" | "tiff" => {
            "image"
        }
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" | "m4v" => "video",
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "aiff" => "audio",
        "pdf" => "pdf",
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" => "archive",
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "html" | "htm" | "css" | "json" | "xml"
        | "toml" | "yaml" | "yml" | "cs" | "cpp" | "c" | "h" | "java" | "php" | "sql" => "code",
        _ => "document",
    }
}

fn file_icon(name: &str) -> (egui::ImageSource<'static>, egui::Color32) {
    use egui::Color32;
    match file_kind(name) {
        "text" => (
            egui::include_image!("../assets/fluent/text.svg"),
            Color32::from_rgb(68, 111, 165),
        ),
        "image" => (
            egui::include_image!("../assets/fluent/image.svg"),
            Color32::from_rgb(35, 137, 100),
        ),
        "video" => (
            egui::include_image!("../assets/fluent/video.svg"),
            Color32::from_rgb(120, 83, 177),
        ),
        "audio" => (
            egui::include_image!("../assets/fluent/audio.svg"),
            Color32::from_rgb(187, 76, 123),
        ),
        "pdf" => (
            egui::include_image!("../assets/fluent/document.svg"),
            Color32::from_rgb(201, 60, 57),
        ),
        "archive" => (
            egui::include_image!("../assets/fluent/archive.svg"),
            Color32::from_rgb(163, 112, 42),
        ),
        "code" => (
            egui::include_image!("../assets/fluent/code.svg"),
            Color32::from_rgb(30, 130, 150),
        ),
        _ => (
            egui::include_image!("../assets/fluent/document.svg"),
            Color32::from_rgb(78, 124, 164),
        ),
    }
}

fn single_line_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn name_layout(name: &str, width: f32, color: egui::Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::simple_singleline(
        name.to_owned(),
        egui::FontId::proportional(14.0),
        color,
    );
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_control() || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn tree(
    ui: &mut egui::Ui,
    volume: usize,
    path: &FolderPath,
    cache: &HashMap<(usize, u64), Vec<DirEntry>>,
    expanded: &mut HashSet<(usize, u64)>,
    location: &Option<Location>,
    busy: bool,
    target: &mut Option<Location>,
) {
    if path.len() > 64 {
        return;
    }
    let Some((oid, name)) = path.last() else {
        return;
    };
    let key = (volume, *oid);
    ui.push_id(key, |ui| {
        ui.horizontal(|ui| {
            let can_expand = cache
                .get(&key)
                .is_none_or(|entries| entries.iter().any(|entry| entry.flags & 15 == 4));
            let arrow = ui
                .add_enabled_ui(!busy && can_expand, |ui| {
                    let (_, response) =
                        ui.allocate_exact_size(egui::vec2(24.0, 28.0), egui::Sense::click());
                    if can_expand {
                        let source = if expanded.contains(&key) {
                            egui::include_image!("../assets/fluent/chevron_down.svg")
                        } else {
                            egui::include_image!("../assets/fluent/chevron_right.svg")
                        };
                        egui::Image::new(source)
                            .tint(egui::Color32::from_gray(100))
                            .paint_at(
                                ui,
                                egui::Rect::from_center_size(
                                    response.rect.center(),
                                    egui::vec2(12.0, 12.0),
                                ),
                            );
                    }
                    response.on_hover_text(if expanded.contains(&key) {
                        "Collapse folder"
                    } else {
                        "Expand folder"
                    })
                })
                .inner;
            if arrow.clicked() && !expanded.remove(&key) {
                expanded.insert(key);
                if !cache.contains_key(&key) {
                    *target = Some((volume, path.clone()));
                }
            }
            let selected = location.as_ref().is_some_and(|current| {
                current.0 == volume && current.1.last().map(|entry| entry.0) == Some(*oid)
            });
            if ui
                .add_enabled_ui(!busy, |ui| {
                    item_row(
                        ui,
                        name,
                        true,
                        expanded.contains(&key),
                        selected,
                        ui.available_width().max(100.0),
                    )
                })
                .inner
                .clicked()
            {
                *target = Some((volume, path.clone()));
            }
        });
        if expanded.contains(&key)
            && let Some(entries) = cache.get(&key)
        {
            ui.indent("children", |ui| {
                for entry in entries.iter().filter(|entry| entry.flags & 15 == 4) {
                    if path.iter().any(|item| item.0 == entry.file_id) {
                        continue;
                    }
                    let mut child = path.clone();
                    child.push((entry.file_id, entry.name.clone()));
                    tree(ui, volume, &child, cache, expanded, location, busy, target);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_limits_and_types() {
        assert!(check_preview_size(64 * 1024 * 1024).is_ok());
        assert!(check_preview_size(64 * 1024 * 1024 + 1).is_err());
        for name in ["photo.JPG", "clip.mp4", "book.pdf", "note.txt"] {
            assert!(preview_extension(name).is_some());
        }
        for name in [
            "run.exe",
            "script.cmd",
            "page.html",
            "image.svg",
            "shortcut.lnk",
        ] {
            assert!(preview_extension(name).is_none());
        }
    }

    #[test]
    fn temporary_files_deleted_after_session_ends() {
        let mut panel = BrowserPanel::default();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.txt");
        std::fs::write(&path, b"preview").unwrap();
        panel.temporary_files.lock().unwrap().push(directory);
        let temporary_files = panel.temporary_files.clone();
        panel = BrowserPanel {
            temporary_files,
            ..Default::default()
        };
        assert!(path.exists());
        drop(panel);
        assert!(!path.exists());
    }
    #[test]
    fn selects_common_file_icons() {
        for (name, kind) in [
            ("note.TXT", "text"),
            ("photo.JPG", "image"),
            ("clip.mp4", "video"),
            ("song.mp3", "audio"),
            ("book.PDF", "pdf"),
            ("files.zip", "archive"),
            ("index.html", "code"),
            ("README", "document"),
            ("file.unknown", "document"),
        ] {
            assert_eq!(file_kind(name), kind);
        }
        for bytes in [
            include_bytes!("../assets/fluent/text.svg").as_slice(),
            include_bytes!("../assets/fluent/image.svg").as_slice(),
            include_bytes!("../assets/fluent/video.svg").as_slice(),
            include_bytes!("../assets/fluent/audio.svg").as_slice(),
            include_bytes!("../assets/fluent/archive.svg").as_slice(),
            include_bytes!("../assets/fluent/code.svg").as_slice(),
        ] {
            let image = egui_extras::image::load_svg_bytes(bytes).unwrap();
            assert!(image.pixels.iter().any(|pixel| pixel.a() > 0));
        }
    }
    #[test]
    fn file_names_stay_on_one_row() {
        let original = "01. Welcome\r\n(Viewed)\t\u{2028}file.html";
        let display = single_line_name(original);
        assert!(
            !display
                .chars()
                .any(|character| character.is_control() || character == '\u{2028}')
        );
        assert!(original.contains('\n'));
        let context = egui::Context::default();
        let _ = context.run(egui::RawInput::default(), |context| {
            for width in [60.0, 500.0] {
                let galley = context.fonts(|fonts| {
                    fonts.layout_job(name_layout(&display, width, egui::Color32::BLACK))
                });
                assert_eq!(galley.rows.len(), 1);
                assert!(galley.size().y <= 30.0);
                assert!(galley.size().x <= width + 1.0);
            }
        });
    }
    #[test]
    fn file_and_folder_icons_preserve_tint_colors() {
        for bytes in [
            include_bytes!("../assets/fluent/folder.svg").as_slice(),
            include_bytes!("../assets/fluent/folder_open.svg").as_slice(),
            include_bytes!("../assets/fluent/document.svg").as_slice(),
        ] {
            let image = egui_extras::image::load_svg_bytes(bytes).unwrap();
            assert!(image.pixels.iter().any(|pixel| pixel.a() > 0));
            assert!(image.pixels.iter().any(|pixel| pixel.a() == 255));
            for pixel in image.pixels.iter().filter(|pixel| pixel.a() == 255) {
                assert_eq!(pixel.r(), 255);
                assert_eq!(pixel.g(), 255);
                assert_eq!(pixel.b(), 255);
            }
        }
    }
    #[test]
    fn fluent_svg_assets_render() {
        for bytes in [
            include_bytes!("../assets/fluent/folder.svg").as_slice(),
            include_bytes!("../assets/fluent/folder_open.svg").as_slice(),
            include_bytes!("../assets/fluent/document.svg").as_slice(),
            include_bytes!("../assets/fluent/chevron_right.svg").as_slice(),
            include_bytes!("../assets/fluent/chevron_down.svg").as_slice(),
            include_bytes!("../assets/fluent/back.svg").as_slice(),
            include_bytes!("../assets/fluent/up.svg").as_slice(),
            include_bytes!("../assets/fluent/refresh.svg").as_slice(),
            include_bytes!("../assets/fluent/extract.svg").as_slice(),
            include_bytes!("../assets/fluent/open_file.svg").as_slice(),
            include_bytes!("../assets/fluent/volume.svg").as_slice(),
        ] {
            let image = egui_extras::image::load_svg_bytes(bytes).unwrap();
            assert!(image.pixels.iter().any(|pixel| pixel.a() > 0));
        }
    }
    #[test]
    fn formats_file_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(1_073_741_824), "1.0 GiB");
    }
    #[test]
    fn dates_use_utc_and_unknown_is_explicit() {
        assert_eq!(added_date(0), "-");
        assert_eq!(added_date(1_000_000_000), "1970-01-01  00:00");
    }
    #[test]
    fn back_does_not_add_current_location_to_history() {
        let root = (0, vec![(2, "Volume".into())]);
        let child = (0, vec![(2, "Volume".into()), (10, "Folder".into())]);
        let mut panel = BrowserPanel {
            location: Some(child),
            going_back: true,
            ..Default::default()
        };
        let (sender, receiver) = mpsc::channel();
        panel.pending = Some(receiver);
        sender
            .send((
                None,
                Ok(Outcome::Directory(root.clone(), Vec::new(), HashMap::new())),
            ))
            .unwrap();
        panel.poll();
        assert_eq!(panel.location, Some(root));
        assert!(panel.history.is_empty());
        assert!(!panel.busy());
    }
    #[test]
    fn sanitizes_export_suggestion() {
        assert_eq!(safe_name("a/b:c?d"), "a_b_c_d");
    }
    #[test]
    fn empty_browser_renders_at_small_and_large_sizes() {
        for size in [egui::vec2(640.0, 480.0), egui::vec2(1200.0, 800.0)] {
            let context = egui::Context::default();
            let mut panel = BrowserPanel::default();
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| panel.show(ui, context));
                },
            );
        }
    }
}
