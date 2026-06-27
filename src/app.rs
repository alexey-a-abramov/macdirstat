use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Instant;

use eframe::egui;

use crate::format_size;
use crate::model::color::ColorMap;
use crate::model::tree::{FileTree, TreePath};
use crate::settings::Settings;
use crate::ui;

pub struct App {
    state: AppState,
    settings: Settings,
    settings_open: bool,
    #[cfg(target_os = "macos")]
    about_configured: bool,
}

enum AppState {
    Scanning {
        path: PathBuf,
        start_time: Instant,
        receiver: std::sync::mpsc::Receiver<FileTree>,
        progress: Arc<AtomicU64>,
        /// Set to true to ask the background scan thread to stop early.
        cancel: Arc<AtomicBool>,
    },
    Loaded(Box<LoadedState>),
}

/// Which visualization the right-hand panel shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Treemap,
    Sunburst,
    Largest,
}

struct LoadedState {
    tree: FileTree,
    color_map: ColorMap,
    selected: Option<TreePath>,
    scan_time_ms: f64,
    cached_layout_rect: Option<egui::Rect>,
    /// The subtree the cushion texture was last laid out for (treemap navigation).
    cached_view_root: TreePath,
    treemap_texture: Option<egui::TextureHandle>,
    pending_scan: Option<PathBuf>,
    open_settings_requested: bool,
    /// True when the displayed tree came from a scan the user stopped early.
    partial: bool,
    /// Optimized rescan requested (check existing nodes only, no full re-walk).
    pending_refresh: bool,
    /// Right-panel visualization mode.
    view_mode: ViewMode,
    /// Cached "largest folders" ranking, recomputed lazily after tree changes.
    largest: Option<Vec<crate::model::tree::DirSummary>>,
    /// "All File Types" report popover state.
    show_all_file_types: bool,
    file_types_search: String,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial_path: Option<String>) -> Self {
        let settings = Settings::load();
        // Scan the path given on the command line, otherwise the user's home
        // directory. Scanning starts immediately on a background thread, so the
        // window appears already showing scan progress.
        let path = initial_path
            .map(PathBuf::from)
            .unwrap_or_else(default_scan_dir);
        Self {
            state: spawn_scan(&settings, path),
            settings,
            settings_open: false,
            #[cfg(target_os = "macos")]
            about_configured: false,
        }
    }

    fn start_scan(&mut self, path: PathBuf) {
        self.state = spawn_scan(&self.settings, path);
    }

    /// Swap the current `Loaded` state out and kick off an optimized background
    /// refresh that checks existing nodes for existence/size changes without
    /// doing a full directory walk.  Falls back gracefully if not in Loaded state.
    fn start_refresh(&mut self) {
        let placeholder = AppState::Scanning {
            path: PathBuf::new(),
            start_time: Instant::now(),
            receiver: std::sync::mpsc::channel().1,
            progress: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        match std::mem::replace(&mut self.state, placeholder) {
            AppState::Loaded(loaded) => {
                self.state = spawn_refresh(&self.settings, loaded.tree);
            }
            other => {
                self.state = other;
            }
        }
    }

    /// Render the top menu bar. Present in every state so Settings and folder
    /// actions are always reachable.
    fn show_menu_bar(&mut self, ctx: &egui::Context) {
        let mut open_folder = false;
        let mut rescan = false;
        let mut quit = false;
        let mut open_settings = false;

        let can_rescan = matches!(self.state, AppState::Loaded(_));

        let mut set_mode: Option<ViewMode> = None;
        let cur_mode = if let AppState::Loaded(loaded) = &self.state {
            Some(loaded.view_mode)
        } else {
            None
        };

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open Folder\u{2026}").clicked() {
                        open_folder = true;
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(can_rescan, egui::Button::new("Rescan"))
                        .clicked()
                    {
                        rescan = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        quit = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Settings\u{2026}").clicked() {
                        open_settings = true;
                        ui.close_menu();
                    }
                });
                // A direct, hard-to-miss entry point as well.
                if ui
                    .button("\u{2699}\u{FE0F} Settings")
                    .on_hover_text("\u{2318},")
                    .clicked()
                {
                    open_settings = true;
                }

                // View-mode toggle (only meaningful once a scan is loaded).
                if let Some(cur) = cur_mode {
                    ui.separator();
                    if ui
                        .selectable_label(cur == ViewMode::Treemap, "\u{25A6} Treemap")
                        .clicked()
                    {
                        set_mode = Some(ViewMode::Treemap);
                    }
                    if ui
                        .selectable_label(cur == ViewMode::Sunburst, "\u{25C9} Sunburst")
                        .clicked()
                    {
                        set_mode = Some(ViewMode::Sunburst);
                    }
                    if ui
                        .selectable_label(cur == ViewMode::Largest, "\u{1F4CA} Largest")
                        .clicked()
                    {
                        set_mode = Some(ViewMode::Largest);
                    }
                }
            });
        });

        if open_settings {
            self.settings_open = true;
        }
        if let (Some(m), AppState::Loaded(loaded)) = (set_mode, &mut self.state) {
            loaded.view_mode = m;
        }
        if quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if open_folder {
            if let Some(path) = pick_folder() {
                self.start_scan(path);
            }
        }
        if rescan {
            self.start_refresh();
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Global blue selection highlight
        ctx.style_mut(|style| {
            style.visuals.selection.bg_fill = egui::Color32::from_rgb(56, 132, 244);
        });

        // Configure the native About panel text on the first frame.
        #[cfg(target_os = "macos")]
        if !self.about_configured {
            self.about_configured = true;
            configure_about_panel_text();
        }

        // Top menu bar — registered before the per-state panels so the central
        // panel is always added last.
        self.show_menu_bar(ctx);

        // ⌘, opens Settings (macOS convention), in any state.
        if ctx.input(|i| i.key_pressed(egui::Key::Comma) && i.modifiers.command) {
            self.settings_open = true;
        }

        // Check whether the background scan thread has finished.
        let mut completed: Option<(FileTree, f64, bool)> = None;
        if let AppState::Scanning {
            ref receiver,
            start_time,
            ref cancel,
            ..
        } = self.state
        {
            if let Ok(tree) = receiver.try_recv() {
                completed = Some((
                    tree,
                    start_time.elapsed().as_secs_f64() * 1000.0,
                    cancel.load(Ordering::Relaxed),
                ));
            }
        }
        if let Some((tree, scan_time_ms, partial)) = completed {
            log::info!(
                "Scan {}: {} — {} files, {} dirs, {} in {:.0}ms",
                if partial { "stopped" } else { "complete" },
                tree.root_path,
                tree.root.file_count,
                tree.root.dir_count,
                format_size(tree.root.size),
                scan_time_ms,
            );
            let color_map = ColorMap::from_extensions(&tree.extensions);
            self.state = AppState::Loaded(Box::new(LoadedState {
                tree,
                color_map,
                selected: None,
                scan_time_ms,
                cached_layout_rect: None,
                cached_view_root: Vec::new(),
                treemap_texture: None,
                pending_scan: None,
                open_settings_requested: false,
                partial,
                pending_refresh: false,
                view_mode: ViewMode::Treemap,
                largest: None,
                show_all_file_types: false,
                file_types_search: String::new(),
            }));
        }

        match &mut self.state {
            AppState::Scanning {
                path,
                progress,
                cancel,
                ..
            } => {
                let count = progress.load(Ordering::Relaxed);
                show_scanning_panes(ctx, path, count, cancel, &mut self.settings_open);
                ctx.request_repaint();
            }
            AppState::Loaded(loaded) => {
                handle_delete(loaded, ctx);
                loaded.as_mut().show_panels(ctx);
            }
        }

        // Handle ⌘O, ⌘R, pending scans/refreshes, and settings_open flag — outside
        // the match to avoid borrow conflicts with self.state.
        if let AppState::Loaded(loaded) = &mut self.state {
            let cmd_o = ctx.input(|i| i.key_pressed(egui::Key::O) && i.modifiers.command);
            let cmd_r = ctx.input(|i| i.key_pressed(egui::Key::R) && i.modifiers.command);
            if loaded.open_settings_requested {
                loaded.open_settings_requested = false;
                self.settings_open = true;
            }
            // ⌘R and the status-bar Rescan button both trigger optimized refresh.
            // ⌘O and breadcrumb/pending_scan trigger a full scan (new root or new settings).
            let do_refresh = cmd_r || std::mem::take(&mut loaded.pending_refresh);
            let full_scan_path = if cmd_o {
                pick_folder()
            } else {
                loaded.pending_scan.take()
            };
            // `loaded` last used above; NLL releases the borrow so &mut self is safe below.
            if let Some(path) = full_scan_path {
                self.start_scan(path);
            } else if do_refresh {
                self.start_refresh();
            }
        }

        // Settings window
        if self.settings_open {
            let mut open = self.settings_open;
            egui::Window::new("Settings")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.set_min_width(340.0);
                    ui.heading("Scan Settings");
                    ui.separator();
                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.settings.ignore_cloud_storage,
                        "Ignore cloud storage",
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            "Skips the cloud roots — these enumerate over the network and can be very slow:\n  \u{2022} ~/Library/CloudStorage  (Google Drive, OneDrive, Dropbox, Box\u{2026})\n  \u{2022} ~/Library/Mobile\u{202F}Documents  (iCloud Drive)\n  \u{2022} ~/Dropbox, ~/OneDrive, ~/Google Drive",
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );

                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Excluded folders").strong());
                    ui.label(
                        egui::RichText::new(
                            "Folders skipped entirely during scans (slow, network, or cloud trees).",
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                    ui.add_space(2.0);
                    let mut remove_idx: Option<usize> = None;
                    for (i, p) in self.settings.custom_excludes.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.small_button("\u{2715}").on_hover_text("Remove").clicked() {
                                remove_idx = Some(i);
                            }
                            ui.label(egui::RichText::new(p).small());
                        });
                    }
                    if let Some(i) = remove_idx {
                        self.settings.custom_excludes.remove(i);
                    }
                    if ui.button("\u{2795} Add folder\u{2026}").clicked()
                        && let Some(path) = pick_folder()
                    {
                        let s = path.to_string_lossy().into_owned();
                        if !self.settings.custom_excludes.contains(&s) {
                            self.settings.custom_excludes.push(s);
                        }
                    }

                    ui.add_space(8.0);
                    ui.checkbox(
                        &mut self.settings.skip_duplicate_inodes,
                        "Skip duplicate inodes",
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            "Avoids double-counting hardlinks and macOS firmlinks\n(e.g. /Users \u{2194} /System/Volumes/Data/Users)",
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                    ui.add_space(8.0);
                    ui.checkbox(
                        &mut self.settings.optimization_mode,
                        "Optimization mode",
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            "Skip files smaller than the threshold below.\nGreatly speeds up scans of directories with many tiny files.",
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                    ui.add_space(4.0);
                    ui.add_enabled_ui(self.settings.optimization_mode, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Min file size (MB):");
                            let mut mb_str = self.settings.min_file_size_mb.to_string();
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut mb_str)
                                    .desired_width(60.0),
                            );
                            if resp.changed() {
                                if let Ok(n) = mb_str.trim().parse::<u64>() {
                                    self.settings.min_file_size_mb = n.max(1);
                                }
                            }
                        });
                    });
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new("Advanced")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Scan threads:");
                                let mut t_str = self.settings.scan_threads.to_string();
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut t_str).desired_width(60.0),
                                );
                                if resp.changed()
                                    && let Ok(n) = t_str.trim().parse::<u64>()
                                {
                                    self.settings.scan_threads = n.min(256);
                                }
                                ui.label(
                                    egui::RichText::new("0 = auto")
                                        .small()
                                        .color(egui::Color32::GRAY),
                                );
                            });
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(
                                    "Worker threads used while scanning. 0 uses one per CPU core\n(best for fast SSDs). Lowering to 4\u{2013}8 can help on slow or\nnetwork volumes where too many threads contend for the disk.",
                                )
                                .small()
                                .color(egui::Color32::GRAY),
                            );
                        });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.settings = Settings::load();
                            self.settings_open = false;
                        }
                        if ui.button("Apply & Rescan").clicked() {
                            self.settings.save();
                            self.settings_open = false;
                            if let AppState::Loaded(loaded) = &mut self.state {
                                loaded.pending_scan =
                                    Some(PathBuf::from(&loaded.tree.root_path));
                            }
                        }
                    });
                });
            if !open {
                self.settings_open = false;
            }
        }
    }
}

impl LoadedState {
    fn show_panels(&mut self, ctx: &egui::Context) {
        self.show_file_types_bar(ctx);
        self.show_status_bar(ctx);
        self.show_tree_panel(ctx);
        self.show_central_panel(ctx);
        self.show_all_file_types_window(ctx);
    }

    fn show_status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} Files",
                    format_file_count(self.tree.root.file_count as u64)
                ));
                ui.separator();
                ui.label(format!(
                    "{} Scanned in {:.0}ms",
                    format_size(self.tree.root.size),
                    self.scan_time_ms,
                ));
                if self.partial {
                    ui.separator();
                    ui.label(
                        egui::RichText::new("\u{26A0} Partial scan (stopped)")
                            .color(egui::Color32::from_rgb(220, 150, 40)),
                    )
                    .on_hover_text(
                        "Scan was stopped early — sizes are incomplete. Rescan for full results.",
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let has_selection = self.selected.is_some();

                    let trash_text = egui::RichText::new("\u{1F5D1}").color(if has_selection {
                        egui::Color32::from_rgb(220, 60, 60)
                    } else {
                        egui::Color32::from_rgb(160, 120, 120)
                    });
                    let trash_btn = ui.add_enabled(has_selection, egui::Button::new(trash_text));
                    if trash_btn.clicked()
                        && let Some(target) = self
                            .selected
                            .as_ref()
                            .and_then(|sp| DeleteTarget::from_selection(&self.tree, sp))
                        && native_confirm_delete(
                            target.name(),
                            target.size,
                            &target.fs_path,
                            target.is_dir,
                        )
                    {
                        execute_delete(self, &target);
                    }

                    let reveal_btn = ui.add_enabled(
                        has_selection,
                        egui::Button::new("\u{1F50D} Reveal in Finder"),
                    );
                    if reveal_btn.clicked()
                        && let Some(sel_path) = self.selected.as_ref()
                        && let Some(fs_path) = self.tree.build_fs_path(sel_path)
                    {
                        reveal_in_finder(&fs_path);
                    }

                    ui.separator();

                    if ui
                        .button("\u{21BA} Rescan")
                        .on_hover_text("\u{2318}R")
                        .clicked()
                    {
                        self.pending_refresh = true;
                    }

                    if ui
                        .button("\u{2699}\u{FE0F}")
                        .on_hover_text("Settings")
                        .clicked()
                    {
                        self.open_settings_requested = true;
                    }
                });
            });
        });
    }

    fn show_tree_panel(&mut self, ctx: &egui::Context) {
        let mut finder_action = None;
        egui::SidePanel::left("tree_view")
            .default_width(300.0)
            .min_width(250.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::side_top_panel(ctx.style().as_ref())
                    .inner_margin(egui::Margin::from(8)),
            )
            .show(ctx, |ui| {
                finder_action = ui::tree_view::show(ui, &self.tree.root, &mut self.selected);
            });
        handle_context_action(self, ctx, finder_action);
    }

    fn show_central_panel(&mut self, ctx: &egui::Context) {
        let mut finder_action = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut new_scan_path: Option<PathBuf> = None;
            self.show_breadcrumb(ui, &mut new_scan_path);
            ui.add_space(2.0);
            if let Some(path) = new_scan_path {
                self.pending_scan = Some(path);
            }

            // The right panel follows the selection: a folder shows its own
            // contents, a file shows its parent folder.
            let view_root = derived_view_root(&self.tree, &self.selected);

            match self.view_mode {
                ViewMode::Treemap => {
                    finder_action = ui::treemap_view::show(
                        ui,
                        &mut self.tree,
                        &view_root,
                        &mut self.selected,
                        &self.color_map,
                        &mut self.cached_layout_rect,
                        &mut self.cached_view_root,
                        &mut self.treemap_texture,
                    );
                }
                ViewMode::Sunburst => {
                    ui::sunburst_view::show(
                        ui,
                        &self.tree,
                        &view_root,
                        &mut self.selected,
                        &self.color_map,
                    );
                }
                ViewMode::Largest => {
                    if self.largest.is_none() {
                        self.largest = Some(self.tree.largest_directories(200));
                    }
                    if let Some(dirs) = self.largest.as_ref() {
                        ui::largest_view::show(ui, &self.tree, dirs, &mut self.selected);
                    }
                }
            }
        });
        handle_context_action(self, ctx, finder_action);
    }

    /// Bottom strip: the top file types by size, plus an "All File Types" button.
    fn show_file_types_bar(&mut self, ctx: &egui::Context) {
        if self.tree.extensions.is_empty() {
            return;
        }
        let total = self.tree.root.size.max(1);
        egui::TopBottomPanel::bottom("file_types_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                for stat in self.tree.extensions.iter().take(5) {
                    let pct = stat.bytes as f64 / total as f64 * 100.0;
                    let color = self.color_map.get(&stat.ext);
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, color);
                    ui.label(format!("{}  {:.0}%", stat.ext, pct));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let more = self.tree.extensions.len().saturating_sub(5);
                    let label = if more > 0 {
                        format!("\u{2295} {} more", more)
                    } else {
                        "All file types".to_string()
                    };
                    if ui.button(label).clicked() {
                        self.show_all_file_types = true;
                    }
                });
            });
            ui.add_space(2.0);
        });
    }

    /// The searchable "All File Types" report popover (counts, sizes, %).
    fn show_all_file_types_window(&mut self, ctx: &egui::Context) {
        if !self.show_all_file_types {
            return;
        }
        let total = self.tree.root.size.max(1);
        let mut open = true;
        egui::Window::new("All File Types")
            .open(&mut open)
            .default_width(440.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.file_types_search)
                        .hint_text("Search")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);
                let q = self.file_types_search.trim().to_lowercase();
                egui::ScrollArea::vertical()
                    .max_height(440.0)
                    .show(ui, |ui| {
                        egui::Grid::new("ext_grid")
                            .num_columns(4)
                            .striped(true)
                            .spacing(egui::vec2(14.0, 4.0))
                            .show(ui, |ui| {
                                for stat in &self.tree.extensions {
                                    if !q.is_empty() && !stat.ext.to_lowercase().contains(&q) {
                                        continue;
                                    }
                                    let pct = stat.bytes as f64 / total as f64 * 100.0;
                                    ui.horizontal(|ui| {
                                        let color = self.color_map.get(&stat.ext);
                                        let (dot, _) = ui.allocate_exact_size(
                                            egui::vec2(10.0, 10.0),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().circle_filled(dot.center(), 4.0, color);
                                        ui.label(&*stat.ext);
                                    });
                                    ui.label(format!("{} files", format_file_count(stat.count)));
                                    ui.label(format_size(stat.bytes));
                                    ui.label(format!("{:.1}%", pct));
                                    ui.end_row();
                                }
                            });
                    });
            });
        self.show_all_file_types = open;
    }

    fn show_breadcrumb(&self, ui: &mut egui::Ui, new_scan_path: &mut Option<PathBuf>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let segments: Vec<&str> = self
                .tree
                .root_path
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            let last_idx = segments.len().saturating_sub(1);

            let separator = |ui: &mut egui::Ui| {
                ui.label(
                    egui::RichText::new(" \u{203A} ")
                        .size(13.0)
                        .color(egui::Color32::GRAY),
                );
            };

            ui.label(egui::RichText::new("\u{1F4BB}").size(13.0));

            // "Macintosh HD" navigates to the volume root.
            if ui
                .link(egui::RichText::new("Macintosh HD").size(13.0))
                .on_hover_text("Scan /")
                .clicked()
            {
                *new_scan_path = Some(PathBuf::from("/"));
            }
            if !segments.is_empty() {
                separator(ui);
            }

            // Each segment is a clickable crumb that rescans that ancestor.
            // The final segment is the current location: bold, not a link.
            for (i, seg) in segments.iter().enumerate() {
                if i == last_idx {
                    let blue = egui::Color32::from_rgb(56, 132, 244);
                    ui.label(egui::RichText::new(*seg).size(14.0).strong().color(blue));
                } else {
                    if ui.link(egui::RichText::new(*seg).size(13.0)).clicked() {
                        let mut path = PathBuf::from("/");
                        for ancestor in &segments[..=i] {
                            path.push(ancestor);
                        }
                        *new_scan_path = Some(path);
                    }
                    separator(ui);
                }
            }
        });
    }
}

/// Snapshot of a node's metadata needed for deletion.
struct DeleteTarget {
    sel_path: TreePath,
    fs_path: std::path::PathBuf,
    is_dir: bool,
    size: u64,
    file_count: u32,
    dir_count: u32,
}

impl DeleteTarget {
    /// Resolve the selected node into a DeleteTarget, or None if the path is invalid.
    fn from_selection(tree: &FileTree, sel_path: &[usize]) -> Option<Self> {
        let fs_path = tree.build_fs_path(sel_path)?;
        let node = tree.root.resolve_path(sel_path)?;
        Some(Self {
            sel_path: sel_path.to_vec(),
            fs_path,
            is_dir: node.is_dir,
            size: node.size,
            file_count: node.file_count,
            dir_count: node.dir_count,
        })
    }

    fn name(&self) -> &str {
        self.fs_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
    }
}

/// Handle Delete/Backspace when something is selected.
/// ⌘Delete: delete immediately (no confirmation).
/// Delete alone: show native confirmation dialog.
fn handle_delete(loaded: &mut LoadedState, ctx: &egui::Context) {
    let Some(sel_path) = loaded.selected.as_ref() else {
        return;
    };
    let (cmd_delete, bare_delete) = ctx.input(|i| {
        let del = i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace);
        let cmd = i.modifiers.command;
        (del && cmd, del && !cmd)
    });
    if !(cmd_delete || bare_delete) {
        return;
    }
    let Some(target) = DeleteTarget::from_selection(&loaded.tree, sel_path) else {
        return;
    };
    if !cmd_delete
        && !native_confirm_delete(target.name(), target.size, &target.fs_path, target.is_dir)
    {
        return;
    }
    execute_delete(loaded, &target);
}

fn execute_delete(loaded: &mut LoadedState, target: &DeleteTarget) {
    let result = if target.is_dir {
        std::fs::remove_dir_all(&target.fs_path)
    } else {
        std::fs::remove_file(&target.fs_path)
    };
    match result {
        Ok(()) => {
            loaded.tree.subtract_from_ancestors(
                &target.sel_path,
                target.size,
                target.file_count,
                target.dir_count,
            );
            loaded.tree.remove_at_path(&target.sel_path);
            loaded.tree.rebuild_extensions();
            loaded.color_map = ColorMap::from_extensions(&loaded.tree.extensions);
            loaded.selected = next_selection_after_delete(&loaded.tree.root, &target.sel_path);
            loaded.cached_layout_rect = None;
            loaded.cached_view_root.clear();
            loaded.treemap_texture = None;
            loaded.largest = None; // ranking is now stale
        }
        Err(e) => {
            log::error!("Failed to delete {:?}: {}", target.fs_path, e);
        }
    }
}

/// Render the three-pane scanning layout (same panel IDs as the Loaded state)
/// with live progress and a Stop control. Clicking Stop flips `cancel`, which
/// the background scan thread observes and unwinds, keeping the partial tree.
fn show_scanning_panes(
    ctx: &egui::Context,
    path: &std::path::Path,
    file_count: u64,
    cancel: &AtomicBool,
    settings_open: &mut bool,
) {
    let stopping = cancel.load(Ordering::Relaxed);

    egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} files discovered\u{2026}",
                format_file_count(file_count)
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("\u{2699}\u{FE0F}")
                    .on_hover_text("Settings")
                    .clicked()
                {
                    *settings_open = true;
                }
                ui.separator();
                if compact_stop_button(ui, stopping).clicked() {
                    request_stop(cancel);
                }
            });
        });
    });

    egui::SidePanel::left("tree_view")
        .default_width(300.0)
        .min_width(250.0)
        .show_separator_line(false)
        .frame(
            egui::Frame::side_top_panel(ctx.style().as_ref()).inner_margin(egui::Margin::from(8)),
        )
        .show(ctx, |ui| {
            ui::tree_view::show_branding(ui);
        });

    egui::CentralPanel::default().show(ctx, |ui| {
        let folder_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or(""));

        ui.vertical_centered(|ui| {
            // Push the block to roughly the vertical center of the canvas.
            let top = ((ui.available_height() - 170.0) * 0.5).max(0.0);
            ui.add_space(top);

            ui.add(egui::Spinner::new().size(30.0));
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(format!("Scanning \u{201C}{folder_name}\u{201D}\u{2026}"))
                    .size(18.0),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} files discovered",
                    format_file_count(file_count)
                ))
                .size(14.0)
                .color(egui::Color32::GRAY),
            );
            ui.add_space(24.0);

            // Prominent Stop button.
            let (label, fill) = if stopping {
                ("Stopping\u{2026}", egui::Color32::from_rgb(110, 86, 86))
            } else {
                ("\u{25A0}  Stop", egui::Color32::from_rgb(204, 64, 64))
            };
            let btn = egui::Button::new(
                egui::RichText::new(label)
                    .size(15.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            )
            .fill(fill)
            .min_size(egui::vec2(150.0, 40.0));
            if ui
                .add_enabled(!stopping, btn)
                .on_hover_text("Stop scanning and show what was found so far")
                .clicked()
            {
                request_stop(cancel);
            }

            if stopping {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(
                        "Finishing up \u{2014} showing what was found so far\u{2026}",
                    )
                    .size(12.0)
                    .color(egui::Color32::GRAY),
                );
            }
        });
    });
}

/// A small Stop button for the status bar, mirroring the prominent one.
fn compact_stop_button(ui: &mut egui::Ui, stopping: bool) -> egui::Response {
    let (label, fill) = if stopping {
        ("Stopping\u{2026}", egui::Color32::from_rgb(110, 86, 86))
    } else {
        ("\u{25A0} Stop", egui::Color32::from_rgb(204, 64, 64))
    };
    let btn = egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE)).fill(fill);
    ui.add_enabled(!stopping, btn)
}

/// Ask the running scan to stop; the partial tree gathered so far is kept.
fn request_stop(cancel: &AtomicBool) {
    if !cancel.swap(true, Ordering::Relaxed) {
        log::info!("Scan stop requested by user");
    }
}

/// After deleting the node at `deleted_path`, determine what to select next.
fn next_selection_after_delete(
    root: &crate::model::tree::FileNode,
    deleted_path: &[usize],
) -> Option<TreePath> {
    let (&deleted_idx, parent_path) = deleted_path.split_last()?;

    let parent = root.resolve_path(parent_path)?;
    let child_count = parent.children.len();

    if child_count == 0 {
        if parent_path.is_empty() {
            None
        } else {
            Some(parent_path.to_vec())
        }
    } else if deleted_idx < child_count {
        let mut path = parent_path.to_vec();
        path.push(deleted_idx);
        Some(path)
    } else {
        let mut path = parent_path.to_vec();
        path.push(child_count - 1);
        Some(path)
    }
}

fn handle_context_action(
    loaded: &mut LoadedState,
    ctx: &egui::Context,
    action: Option<(TreePath, ui::ContextAction)>,
) {
    let Some((path, action)) = action else { return };
    match action {
        ui::ContextAction::Delete => {
            if let Some(target) = DeleteTarget::from_selection(&loaded.tree, &path) {
                if native_confirm_delete(target.name(), target.size, &target.fs_path, target.is_dir)
                {
                    execute_delete(loaded, &target);
                }
            }
        }
        ui::ContextAction::CopyPath => {
            if let Some(fs_path) = loaded.tree.build_fs_path(&path) {
                ctx.copy_text(fs_path.to_string_lossy().into_owned());
            }
        }
        _ => {
            if let Some(fs_path) = loaded.tree.build_fs_path(&path) {
                let is_dir = loaded
                    .tree
                    .root
                    .resolve_path(&path)
                    .map(|n| n.is_dir)
                    .unwrap_or(false);
                handle_finder_action(action, &fs_path, is_dir);
            }
        }
    }
}

fn reveal_in_finder(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
    {
        log::warn!("Failed to reveal {:?} in Finder: {}", path, e);
    }
}

fn open_in_finder(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("open").arg(path).spawn() {
        log::warn!("Failed to open {:?} in Finder: {}", path, e);
    }
}

fn handle_finder_action(action: ui::ContextAction, path: &std::path::Path, is_dir: bool) {
    match action {
        ui::ContextAction::OpenInFinder => {
            if is_dir {
                open_in_finder(path);
            } else {
                // For files, "Open in Finder" reveals the file in its parent folder
                reveal_in_finder(path);
            }
        }
        ui::ContextAction::RevealInFinder => reveal_in_finder(path),
        ui::ContextAction::CopyPath | ui::ContextAction::Delete => {}
    }
}

fn format_file_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        // Format with comma separators
        let s = count.to_string();
        let mut result = String::new();
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                result.push(',');
            }
            result.push(c);
        }
        result.chars().rev().collect()
    } else {
        count.to_string()
    }
}

/// Show a native macOS alert for delete confirmation. Returns true if user clicked "Delete".
fn native_confirm_delete(name: &str, size: u64, fs_path: &std::path::Path, is_dir: bool) -> bool {
    let kind = if is_dir { "directory" } else { "file" };
    let escaped_name = applescript_escape(name);
    let escaped_path = applescript_escape(&fs_path.display().to_string());
    let size_str = format_size(size);

    let mut message = format!("{} ({})\n{}", escaped_name, size_str, escaped_path);
    if is_dir {
        message.push_str("\n\nThis will permanently delete the directory and all its contents.");
    }

    let script = format!(
        r#"display alert "Delete this {}?" message "{}" as critical buttons {{"Cancel", "Delete"}} default button "Cancel""#,
        kind, message,
    );

    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.contains("button returned:Delete")
        }
        _ => false,
    }
}

/// Escape a string for use inside AppleScript double-quoted strings.
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Set text fields in the native About panel via the app's info dictionary.
#[cfg(target_os = "macos")]
fn configure_about_panel_text() {
    use crate::objc_ffi::*;

    unsafe {
        let bundle_cls = objc_getClass(c"NSBundle".as_ptr());
        let main_bundle = send0(bundle_cls, sel_registerName(c"mainBundle".as_ptr()));
        let info = send0(main_bundle, sel_registerName(c"infoDictionary".as_ptr()));
        let set_sel = sel_registerName(c"setObject:forKey:".as_ptr());

        send2_void(
            info,
            set_sel,
            nsstring("MacDirStat"),
            nsstring("CFBundleName"),
        );

        let version = env!("CARGO_PKG_VERSION");
        send2_void(
            info,
            set_sel,
            nsstring(version),
            nsstring("CFBundleShortVersionString"),
        );

        send2_void(
            info,
            set_sel,
            nsstring(
                "Author: Michael Strömberg\n\
                 \u{00A9} 2026 \u{2014} Licensed under GPL-3.0\n\n\
                 github.com/MichaelStromberg/macdirstat\n\
                 crates.io/crates/macdirstat",
            ),
            nsstring("NSHumanReadableCopyright"),
        );
    }
}

/// Spawn a background optimized refresh and return a `Scanning` state tracking it.
/// The existing tree is moved to the background thread, refreshed in-place
/// (deleted nodes removed, file sizes updated), then sent back.
fn spawn_refresh(settings: &Settings, tree: FileTree) -> AppState {
    let (tx, rx) = std::sync::mpsc::channel();
    let progress = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let progress_clone = Arc::clone(&progress);
    let cancel_clone = Arc::clone(&cancel);
    let path = PathBuf::from(&tree.root_path);
    let excluded = settings.excluded_paths();

    log::info!("Optimized refresh started: {}", path.display());
    std::thread::spawn(move || {
        let refreshed = tree.refresh_exists(&excluded, &progress_clone, &cancel_clone);
        let _ = tx.send(refreshed);
    });

    AppState::Scanning {
        path,
        start_time: Instant::now(),
        receiver: rx,
        progress,
        cancel,
    }
}

/// Spawn a background scan of `path` and return the `Scanning` state that tracks it.
fn spawn_scan(settings: &Settings, path: PathBuf) -> AppState {
    let (tx, rx) = std::sync::mpsc::channel();
    let progress = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let progress_clone = Arc::clone(&progress);
    let cancel_clone = Arc::clone(&cancel);
    let path_clone = path.clone();
    let excluded = settings.excluded_paths();

    let skip_duplicates = settings.skip_duplicate_inodes;
    let min_file_size_bytes = settings.min_file_size_bytes();
    let scan_threads = settings.scan_threads as usize;
    log::info!(
        "Scan started: {} (skip_duplicates={}, min_file_size={} MB, scan_threads={}, {} excluded paths)",
        path.display(),
        skip_duplicates,
        min_file_size_bytes / (1024 * 1024),
        if scan_threads == 0 {
            "auto".to_string()
        } else {
            scan_threads.to_string()
        },
        excluded.len(),
    );
    std::thread::spawn(move || {
        let tree = FileTree::scan(
            &path_clone,
            &excluded,
            &progress_clone,
            skip_duplicates,
            min_file_size_bytes,
            &cancel_clone,
            scan_threads,
        );
        let _ = tx.send(tree);
    });

    AppState::Scanning {
        path,
        start_time: Instant::now(),
        receiver: rx,
        progress,
        cancel,
    }
}

/// The directory scanned at startup when no path is given on the command line.
/// The folder the right-hand view should show, given the current selection:
/// a selected directory shows its own contents; a selected file shows its
/// parent folder; no selection shows the scan root.
fn derived_view_root(tree: &FileTree, selected: &Option<TreePath>) -> TreePath {
    match selected {
        None => Vec::new(),
        Some(p) => match tree.root.resolve_path(p) {
            Some(n) if n.is_dir => p.clone(),
            Some(_) => p[..p.len().saturating_sub(1)].to_vec(),
            None => Vec::new(),
        },
    }
}

fn default_scan_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users"))
}

/// Folder picker — used from the File menu and ⌘O.
fn pick_folder() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Select folder to scan")
        .pick_folder()
}
