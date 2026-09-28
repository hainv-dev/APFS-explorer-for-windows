#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apfs;
mod browser;
mod browser_ui;
mod drives;

use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

struct Inspection {
    path: PathBuf,
    result: Result<apfs::ContainerInfo, String>,
}

#[derive(Default)]
struct Explorer {
    browser: browser_ui::BrowserPanel,
    current: Option<Inspection>,
    pending: Option<Receiver<Option<Inspection>>>,
    scan_pending: Option<Receiver<Result<drives::ScanReport, String>>>,
    scan_result: Option<Result<drives::ScanReport, String>>,
}

impl Explorer {
    fn storage_view(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::Image::new(egui::include_image!("../assets/fluent/volume.svg"))
                    .fit_to_exact_size(egui::vec2(32.0, 32.0)),
            );
            ui.label(egui::RichText::new("Storage").size(26.0).strong());
        });
        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.scan_pending.is_none(),
                    egui::Button::image_and_text(
                        egui::Image::new(egui::include_image!("../assets/fluent/refresh.svg"))
                            .fit_to_exact_size(egui::vec2(18.0, 18.0)),
                        if self.scan_result.is_some() {
                            "Rescan drives"
                        } else {
                            "Scan drives"
                        },
                    )
                    .min_size(egui::vec2(160.0, 38.0))
                    .fill(egui::Color32::from_rgb(213, 234, 252)),
                )
                .clicked()
            {
                self.scan_drives(context);
            }
            if ui
                .add_enabled(
                    self.pending.is_none(),
                    egui::Button::image_and_text(
                        egui::Image::new(egui::include_image!("../assets/fluent/open_file.svg"))
                            .fit_to_exact_size(egui::vec2(18.0, 18.0)),
                        "Open disk image...",
                    )
                    .min_size(egui::vec2(170.0, 38.0)),
                )
                .clicked()
            {
                self.open(context, None);
            }
        });
        ui.add_space(22.0);
        ui.horizontal(|ui| {
            ui.strong("Connected partitions");
            if let Some(Ok(report)) = &self.scan_result {
                ui.label(
                    egui::RichText::new(format!(
                        "{} APFS / {} disks",
                        report.drives.len(),
                        report.disk_count
                    ))
                    .color(egui::Color32::from_gray(110)),
                );
            }
        });
        ui.add_space(8.0);
        ui.separator();
        if self.scan_pending.is_some() {
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Scanning connected disks...");
            });
            return;
        }
        let Some(result) = &self.scan_result else {
            ui.add_space(32.0);
            ui.label(egui::RichText::new("No scan results yet").size(18.0));
            ui.label(
                egui::RichText::new("Windows disks have not been scanned in this session.")
                    .color(egui::Color32::from_gray(110)),
            );
            return;
        };
        match result {
            Err(error) => {
                ui.add_space(16.0);
                ui.colored_label(
                    egui::Color32::from_rgb(177, 48, 48),
                    "Scan could not complete",
                );
                ui.add(egui::Label::new(error).wrap());
            }
            Ok(report) => {
                for warning in &report.warnings {
                    ui.colored_label(egui::Color32::from_rgb(150, 90, 20), warning);
                }
                if report.drives.is_empty() {
                    ui.add_space(24.0);
                    ui.label(egui::RichText::new("No APFS partitions found").size(18.0));
                    ui.label("No APFS GPT partitions were detected on accessible Windows disks.");
                }
                for drive in &report.drives {
                    let partition = &drive.partition;
                    ui.push_id((partition.disk_number, partition.partition_number), |ui| {
                        ui.add_space(12.0);
                        egui::Frame::NONE
                            .fill(egui::Color32::WHITE)
                            .inner_margin(16)
                            .corner_radius(6)
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                egui::Color32::from_rgb(225, 230, 236),
                            ))
                            .show(ui, |ui| {
                                ui.set_width((ui.available_width()).max(0.0));
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Image::new(egui::include_image!(
                                            "../assets/fluent/volume.svg"
                                        ))
                                        .fit_to_exact_size(egui::vec2(36.0, 36.0)),
                                    );
                                    ui.vertical(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&partition.disk_name)
                                                    .size(17.0)
                                                    .strong(),
                                            )
                                            .wrap(),
                                        );
                                        ui.label(format!(
                                            "Disk {}  /  Partition {}",
                                            partition.disk_number, partition.partition_number
                                        ));
                                    });
                                });
                                ui.add_space(12.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.strong(format!(
                                        "{:.2} GiB",
                                        partition.size as f64 / 1_073_741_824.0
                                    ));
                                    ui.separator();
                                    ui.label("APFS");
                                    ui.separator();
                                    ui.colored_label(
                                        if drive.verification.is_ok() {
                                            egui::Color32::from_rgb(20, 115, 90)
                                        } else {
                                            egui::Color32::from_rgb(150, 90, 20)
                                        },
                                        if drive.verification.is_ok() {
                                            "Signature verified"
                                        } else {
                                            "Verification unavailable"
                                        },
                                    );
                                });
                                if let Err(error) = &drive.verification {
                                    ui.add(egui::Label::new(error).wrap());
                                }
                                ui.add_space(10.0);
                                let details_id = ui.make_persistent_id("partition_details");
                                let mut details_open = ui.data_mut(|data| {
                                    data.get_temp::<bool>(details_id).unwrap_or(false)
                                });
                                ui.horizontal_wrapped(|ui| {
                                    if ui
                                        .add_enabled(
                                            !self.browser.busy() && drive.verification.is_ok(),
                                            egui::Button::image_and_text(
                                                egui::Image::new(egui::include_image!(
                                                    "../assets/fluent/folder_open.svg"
                                                ))
                                                .fit_to_exact_size(egui::vec2(18.0, 18.0))
                                                .tint(egui::Color32::from_rgb(185, 132, 20)),
                                                "Browse volumes",
                                            )
                                            .min_size(egui::vec2(170.0, 36.0))
                                            .fill(egui::Color32::from_rgb(213, 234, 252)),
                                        )
                                        .clicked()
                                    {
                                        self.browser.open(context, partition.clone());
                                    }
                                    let chevron = if details_open {
                                        egui::include_image!("../assets/fluent/chevron_down.svg")
                                    } else {
                                        egui::include_image!("../assets/fluent/chevron_right.svg")
                                    };
                                    if ui
                                        .add(
                                            egui::Button::image_and_text(
                                                egui::Image::new(chevron)
                                                    .fit_to_exact_size(egui::vec2(12.0, 12.0)),
                                                "Details",
                                            )
                                            .min_size(egui::vec2(100.0, 36.0))
                                            .frame(false),
                                        )
                                        .clicked()
                                    {
                                        details_open = !details_open;
                                        ui.data_mut(|data| {
                                            data.insert_temp(details_id, details_open)
                                        });
                                    }
                                });
                                if details_open {
                                    ui.add_space(8.0);
                                    ui.separator();
                                    ui.vertical(|ui| {
                                        ui.label(format!("Offset: {} bytes", partition.offset));
                                        ui.label(format!(
                                            "Partition size: {} bytes",
                                            partition.size
                                        ));
                                        ui.label(format!(
                                            "Logical sector: {} bytes",
                                            partition.sector_size
                                        ));
                                    });
                                }
                            });
                    });
                }
            }
        }
        ui.add_space(20.0);
    }

    fn scan_drives(&mut self, context: &egui::Context) {
        if self.scan_pending.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.scan_pending = Some(receiver);
        self.scan_result = None;
        let context = context.clone();
        std::thread::spawn(move || {
            let _ = sender.send(drives::scan());
            context.request_repaint();
        });
    }

    fn open(&mut self, context: &egui::Context, path: Option<PathBuf>) {
        if self.pending.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        let context = context.clone();
        std::thread::spawn(move || {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .set_title("Open raw APFS partition image")
                    .add_filter("Raw images", &["img", "raw", "bin"])
                    .add_filter("All files", &["*"])
                    .pick_file()
            });
            let inspection = path.map(|path| Inspection {
                result: apfs::inspect(&path),
                path,
            });
            let _ = sender.send(inspection);
            context.request_repaint();
        });
    }
}

impl eframe::App for Explorer {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.browser.poll();
        if let Some(receiver) = &self.scan_pending {
            match receiver.try_recv() {
                Ok(result) => {
                    self.scan_result = Some(result);
                    self.scan_pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scan_result = Some(Err(
                        "Disk scanner stopped unexpectedly. Try scanning again.".into(),
                    ));
                    self.scan_pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(inspection) => {
                    if let Some(inspection) = inspection {
                        self.current = Some(inspection);
                    }
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.current = Some(Inspection {
                        path: PathBuf::new(),
                        result: Err("Image reader stopped unexpectedly. Please try again.".into()),
                    });
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        let dropped = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .find_map(|file| file.path.clone())
        });
        if let Some(path) = dropped {
            self.open(context, Some(path));
        }

        egui::TopBottomPanel::top("toolbar").show(context, |ui| {
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("APFS Explorer").size(18.0).strong());
                ui.separator();
                ui.menu_button("File", |ui| {
                    if ui
                        .add_enabled(self.pending.is_none(), egui::Button::new("Open image..."))
                        .clicked()
                    {
                        self.open(context, None);
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            self.pending.is_none() && self.current.is_some(),
                            egui::Button::new("Close image"),
                        )
                        .clicked()
                    {
                        self.current = None;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Drives", |ui| {
                    if let Some(Ok(report)) = &self.scan_result {
                        for drive in &report.drives {
                            if ui
                                .add_enabled(
                                    !self.browser.busy() && drive.verification.is_ok(),
                                    egui::Button::new(format!(
                                        "Disk {} - {}",
                                        drive.partition.disk_number, drive.partition.disk_name
                                    )),
                                )
                                .clicked()
                            {
                                self.browser.open(context, drive.partition.clone());
                                ui.close_menu();
                            }
                        }
                    } else {
                        ui.label("No scan results");
                    }
                });
                if ui
                    .add_enabled(
                        self.scan_pending.is_none(),
                        egui::Button::new("Scan drives"),
                    )
                    .clicked()
                {
                    self.scan_drives(context);
                }
                if self.scan_pending.is_some() {
                    ui.spinner();
                    ui.label("Scanning drives...");
                }
                if self.pending.is_some() {
                    ui.spinner();
                    ui.label("Opening...");
                }
            });
            ui.add_space(10.0);
        });

        egui::TopBottomPanel::bottom("status").show(context, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(egui::Color32::from_rgb(20, 115, 90), "READ ONLY");
                ui.separator();
                ui.label("APFS Explorer  |  Preview 0.2");
            });
        });

        egui::CentralPanel::default().show(context, |ui| {
            if self.browser.active() {
                self.browser.show(ui, context);
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.storage_view(ui, context);
                match &self.current {
                    None => {}
                    Some(inspection) => {
                        ui.heading("Container");
                        ui.add_space(8.0);
                        ui.add(egui::Label::new(inspection.path.display().to_string()).wrap());
                        ui.add_space(16.0);
                        match &inspection.result {
                            Ok(info) => {
                                egui::Grid::new("container_fields")
                                    .num_columns(2)
                                    .spacing([24.0, 12.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for (label, value) in [
                                            ("Format", "APFS / NXSB".to_owned()),
                                            ("UUID", info.uuid.clone()),
                                            ("Block size", format!("{} bytes", info.block_size)),
                                            ("Block count", info.block_count.to_string()),
                                            ("Container size", format!("{} bytes", info.capacity())),
                                            ("Image size", format!("{} bytes", info.image_bytes)),
                                        ] {
                                            ui.label(label);
                                            ui.add(egui::Label::new(value).wrap());
                                            ui.end_row();
                                        }
                                    });
                                ui.add_space(20.0);
                                ui.separator();
                                ui.label("Header detected; checksum and filesystem integrity not verified.");
                                ui.label("Image browsing is not available in this preview.");
                            }
                            Err(error) => {
                                ui.colored_label(egui::Color32::from_rgb(170, 35, 45), "Cannot inspect image");
                                ui.add(egui::Label::new(error).wrap());
                            }
                        }
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    #[test]
    fn renders_storage_states_at_two_sizes() {
        for width in [700.0, 1180.0] {
            for state in 0..6 {
                let context = egui::Context::default();
                egui_extras::install_image_loaders(&context);
                let mut app = Explorer::default();
                match state {
                    1 => {
                        let (_, receiver) = mpsc::channel();
                        app.scan_pending = Some(receiver);
                    }
                    2 => app.scan_result = Some(Err("Windows disk query failed".into())),
                    3 => {
                        app.scan_result = Some(Ok(drives::ScanReport {
                            disk_count: 2,
                            drives: Vec::new(),
                            warnings: Vec::new(),
                        }))
                    }
                    4 | 5 => {
                        app.scan_result = Some(Ok(drives::ScanReport {
                            disk_count: 4,
                            warnings: Vec::new(),
                            drives: vec![drives::DetectedDrive {
                                partition: drives::Partition {
                                    disk_number: 0,
                                    partition_number: 2,
                                    disk_name: "External APFS disk".into(),
                                    offset: 4096,
                                    size: 1_000_000_000,
                                    sector_size: 512,
                                    gpt_type: String::new(),
                                },
                                verification: if state == 4 {
                                    Ok(())
                                } else {
                                    Err("Access denied. Administrator access is required.".into())
                                },
                            }],
                        }))
                    }
                    _ => {}
                }
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 760.0),
                        )),
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            app.storage_view(ui, context);
                        });
                    },
                );
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "APFS Explorer",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            viewport: egui::ViewportBuilder::default()
                .with_icon(
                    eframe::icon_data::from_png_bytes(include_bytes!("../icon.png"))
                        .expect("Invalid application icon"),
                )
                .with_inner_size([1180.0, 760.0])
                .with_min_inner_size([700.0, 480.0]),
            ..Default::default()
        },
        Box::new(|creation| {
            egui_extras::install_image_loaders(&creation.egui_ctx);
            creation.egui_ctx.set_visuals(egui::Visuals::light());
            let mut fonts = egui::FontDefinitions::default();
            if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
                fonts
                    .font_data
                    .insert("Segoe UI".into(), egui::FontData::from_owned(bytes).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "Segoe UI".into());
            }
            creation.egui_ctx.set_fonts(fonts);
            creation.egui_ctx.style_mut(|style| {
                style.spacing.item_spacing = egui::vec2(10.0, 8.0);
                style.spacing.button_padding = egui::vec2(12.0, 7.0);
                style.spacing.interact_size.y = 32.0;
                style
                    .text_styles
                    .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
                style
                    .text_styles
                    .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
                style.visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
                style.visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
                style.visuals.widgets.inactive.fg_stroke =
                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(46, 51, 58));
                style.visuals.widgets.noninteractive.bg_stroke =
                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(226, 230, 235));
                style.visuals.faint_bg_color = egui::Color32::from_rgb(248, 250, 252);
                style.visuals.extreme_bg_color = egui::Color32::WHITE;
                style.visuals.panel_fill = egui::Color32::from_rgb(249, 250, 252);
                style.visuals.selection.bg_fill = egui::Color32::from_rgb(213, 234, 252);
                style.visuals.selection.stroke =
                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(24, 92, 148));
                style.visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(232, 239, 246);
            });
            Ok(Box::<Explorer>::default())
        }),
    )
}
