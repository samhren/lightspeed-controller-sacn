

mod model;
mod engine;
mod audio;
mod scanner;
mod midi;
mod db;

use eframe::egui;
use model::{AppState, PixelStrip, Mask};
use engine::LightingEngine;
use db::Database;
use std::fs;
use std::process::Command;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, Receiver};
use std::time::{Duration, Instant};
struct ViewState {
    offset: egui::Vec2,
    scale: f32,
    drag_id: Option<u64>, 
    drag_type: DragType,
}

#[derive(PartialEq, Clone, Copy)]
enum DragType {
    None,
    Strip,
    Mask,
    ResizeMask(usize), // 0: Top, 1: Right, 2: Bottom, 3: Left (Local space)
}

#[derive(Clone, Copy, PartialEq)]
enum MidiFilter {
    All,
    Linked,      // Has MIDI button assigned
    NotLinked,   // No MIDI button assigned
}

impl Default for MidiFilter {
    fn default() -> Self {
        Self::All
    }
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            offset: egui::vec2(0.0, 0.0),
            scale: 1.0, 
            drag_id: None, 
            drag_type: DragType::None,
        }
    }
}

fn main() -> eframe::Result<()> {
    env_logger::init();

    // Load app icon
    let icon_data = load_icon();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1200.0, 800.0])
        .with_drag_and_drop(true);

    if let Some(icon) = icon_data {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Lightspeed Controller",
        options,
        Box::new(|_cc| Box::new(MyApp::default())),
    )
}

fn load_icon() -> Option<egui::IconData> {
    // Try to load the generated icon PNG
    let icon_bytes = include_bytes!("../generated_icon.png");

    // Decode the PNG
    let image = image::load_from_memory(icon_bytes).ok()?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();

    Some(egui::IconData {
        rgba: rgba.into_raw(),
        width: width as u32,
        height: height as u32,
    })
}

struct MyApp {
    state: AppState,
    engine: LightingEngine,
    view: ViewState,
    status: String,
    is_first_frame: bool,
    // Scenes UI state
    new_scene_open: bool,
    new_scene_name: String,
    new_scene_kind: String, // "Masks" or "Global"
    new_scene_category: String,
    // Scene Manager
    scene_manager_open: bool,
    scene_manager_category_filter: Option<String>,
    // Main Scenes Panel Filter
    main_scenes_category_filter: Option<String>,
    main_scenes_midi_filter: MidiFilter,
    // Database
    db: Database,
    last_change_time: Option<Instant>,
    save_debounce: Duration,
    // Import/Export UI state
    import_dialog_open: bool,
    import_merge_mode: bool,
    import_file_path: Option<PathBuf>,
    // MIDI
    midi_sender: Sender<midi::MidiCommand>,
    midi_receiver: Receiver<midi::MidiEvent>,
    midi_connected: bool,
    last_midi_detection: Option<Instant>,
    // Scene Reordering
    dragged_scene_id: Option<u64>,
}

impl Default for MyApp {
    fn default() -> Self {
        let mut state = AppState::default();
        let mut status = "Ready".to_owned();

        // Open database
        let db_path = user_db_path();
        if let Err(e) = ensure_parent_dir(&db_path) {
            eprintln!("Failed to create config directory: {}", e);
        }

        let mut db = match Database::open(&db_path) {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Failed to open database: {}", e);
                status = format!("Database error: {}", e);
                // Seed default state if database fails
                state.strips.push(PixelStrip::default());
                state.masks.push(model::Mask {
                    id: 1,
                    mask_type: "scanner".into(),
                    x: 0.5,
                    y: 0.5,
                    params: std::collections::HashMap::new(),
                });

                // Create a dummy database (will retry on next launch)
                Database::open(&db_path).unwrap_or_else(|_| {
                    panic!("Fatal: Cannot create database at {:?}", db_path)
                })
            }
        };

        // Load state from database
        match db.load_state() {
            Ok(loaded) => {
                state = loaded;
                // MIGRATION: Move deprecated `global` into `global_effects` if needed
                for scene in &mut state.scenes {
                    if scene.kind == "Global" && scene.global_effects.is_empty() && scene.global.is_some() {
                         if let Some(old_global) = scene.global.take() {
                             scene.global_effects.push(model::GlobalEffectConfig {
                                 effect: old_global,
                                 targets: None, // Apply to all
                             });
                             println!("Migrated scene '{}' global effect", scene.name);
                         }
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to load state from database: {}", e);
                if status == "Ready" {
                    status = format!("Load error: {}", e);
                }
                // Seed defaults if load fails
                state.strips.push(PixelStrip::default());
                state.masks.push(model::Mask {
                    id: 1,
                    mask_type: "scanner".into(),
                    x: 0.5,
                    y: 0.5,
                    params: std::collections::HashMap::new(),
                });
            }
        }
        
        // Init MIDI
        let (tx_event, rx_event) = std::sync::mpsc::channel();
        let tx_cmd = midi::start_midi_service(tx_event);

        // Send initial colors
        let _ = tx_cmd.send(midi::MidiCommand::ClearAll);
        // Small delay to ensure clear processes if needed, but channel order is preserved usually.
        
        for s in &state.scenes {
            if let (Some(btn), Some(col)) = (s.launchpad_btn, s.launchpad_color) {
                 let cmd = if s.launchpad_is_cc {
                     midi::MidiCommand::SetButtonColor { cc: btn, color: col }
                 } else {
                     midi::MidiCommand::SetPadColor { note: btn, color: col }
                 };
                 let _ = tx_cmd.send(cmd);
            }
        }

        Self {
            state,
            engine: LightingEngine::new(),
            view: ViewState::default(),
            status,
            is_first_frame: true,
            new_scene_open: false,
            new_scene_name: "New Scene".into(),
            new_scene_kind: "Masks".into(),
            new_scene_category: "Uncategorized".into(),
            scene_manager_open: false,
            scene_manager_category_filter: None,
            main_scenes_category_filter: None,
            main_scenes_midi_filter: MidiFilter::All,
            db,
            last_change_time: None,
            save_debounce: Duration::from_secs(5),
            import_dialog_open: false,
            import_merge_mode: false,
            import_file_path: None,
            midi_sender: tx_cmd,
            midi_receiver: rx_event,
            midi_connected: false,
            last_midi_detection: None,
            dragged_scene_id: None,
        }
    }
}

impl MyApp {
    fn save_state(&mut self) {
        match self.db.save_state(&self.state) {
            Ok(_) => {
                self.status = "Saved to database".into();
                self.last_change_time = None; // Reset debounce timer
            }
            Err(e) => {
                self.status = format!("Save failed: {}", e);
                eprintln!("Database save error: {}", e);
            }
        }
    }

    fn mark_state_changed(&mut self) {
        self.last_change_time = Some(Instant::now());
    }

    fn export_to_json(&mut self) {
        // Use native file dialog to choose save location
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("lightspeed_export.json")
            .add_filter("JSON", &["json"])
            .save_file()
        {
            match self.db.export_to_json() {
                Ok(json) => {
                    match fs::write(&path, json) {
                        Ok(_) => {
                            self.status = format!("Exported to {}", path.display());
                        }
                        Err(e) => {
                            self.status = format!("Export failed: {}", e);
                            eprintln!("Failed to write export file: {}", e);
                        }
                    }
                }
                Err(e) => {
                    self.status = format!("Export error: {}", e);
                    eprintln!("Failed to export from database: {}", e);
                }
            }
        }
    }

    fn import_from_json(&mut self) {
        // Use native file dialog to choose file
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_file()
        {
            self.import_file_path = Some(path);
            self.import_dialog_open = true;
        }
    }

    fn do_import(&mut self) {
        if let Some(path) = &self.import_file_path {
            match fs::read_to_string(path) {
                Ok(json) => {
                    match self.db.import_from_json(&json, self.import_merge_mode) {
                        Ok(_) => {
                            // Reload state from database
                            match self.db.load_state() {
                                Ok(state) => {
                                    self.state = state;
                                    self.status = "Import successful".into();
                                    // Restart engine with new state
                                    self.engine = LightingEngine::new();
                                }
                                Err(e) => {
                                    self.status = format!("Failed to reload after import: {}", e);
                                    eprintln!("Failed to reload state: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            self.status = format!("Import failed: {}", e);
                            eprintln!("Import error: {}", e);
                        }
                    }
                }
                Err(e) => {
                    self.status = format!("Failed to read file: {}", e);
                    eprintln!("Failed to read import file: {}", e);
                }
            }
        }
    }
}

fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn user_config_path() -> PathBuf {
    // Cross-platform-ish config path
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home::home_dir() {
            return home
                .join("Library")
                .join("Application Support")
                .join("Lightspeed")
                .join("lighting_config.json");
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(base) = std::env::var_os("APPDATA") {
            return PathBuf::from(base)
                .join("Lightspeed")
                .join("lighting_config.json");
        }
    }

    // Linux / fallback: XDG or ~/.config
    if let Ok(base) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(base)
            .join("lightspeed")
            .join("lighting_config.json")
    } else if let Some(home) = home::home_dir() {
        home.join(".config").join("lightspeed").join("lighting_config.json")
    } else {
        // Last resort: current directory
        PathBuf::from("lighting_config.json")
    }
}

fn user_db_path() -> PathBuf {
    // Cross-platform database path (same location as config but .db extension)
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home::home_dir() {
            return home
                .join("Library")
                .join("Application Support")
                .join("Lightspeed")
                .join("lighting_config.db");
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(base) = std::env::var_os("APPDATA") {
            return PathBuf::from(base)
                .join("Lightspeed")
                .join("lighting_config.db");
        }
    }

    // Linux / fallback: XDG or ~/.config
    if let Ok(base) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(base)
            .join("lightspeed")
            .join("lighting_config.db")
    } else if let Some(home) = home::home_dir() {
        home.join(".config").join("lightspeed").join("lighting_config.db")
    } else {
        // Last resort: current directory
        PathBuf::from("lighting_config.db")
    }
}

fn reveal_in_file_manager(path: &Path) {
    // Prefer revealing the file; if not present yet, open the folder
    let target = if path.exists() { path.to_path_buf() } else { path.parent().unwrap_or(Path::new(".")).to_path_buf() };

    #[cfg(target_os = "macos")]
    {
        // On macOS, use `open -R` to reveal the file (or just open the directory)
        let _ = if target.is_file() {
            Command::new("open").args(["-R", &target.to_string_lossy()]).spawn()
        } else {
            Command::new("open").arg(&target).spawn()
        };
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let _ = if target.is_file() {
            Command::new("explorer").arg("/select,").arg(&target).spawn()
        } else {
            Command::new("explorer").arg(&target).spawn()
        };
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // On Linux/Unix, open the containing folder via xdg-open
        let dir = if target.is_dir() { target } else { target.parent().unwrap_or(Path::new(".")).to_path_buf() };
        let _ = Command::new("xdg-open").arg(&dir).spawn();
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle keyboard shortcuts
        ctx.input(|i| {
            // Command+S (Mac) or Ctrl+S (Windows/Linux) to save
            if (i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(egui::Key::S) {
                self.save_state();
            }
        });

        // 1. Detection Logic (Runs on Main Thread)
        // Only run MIDI detection if midi_enabled is true
        if self.state.midi_enabled {
            let now = std::time::Instant::now();
            let should_check = self.last_midi_detection
                .map(|t| now.duration_since(t) > std::time::Duration::from_secs(2))
                .unwrap_or(true);

            if should_check {
                self.last_midi_detection = Some(now);

                if !self.midi_connected {
                    // DETECT
                    if let Some(payload) = midi::detect_launchpad() {
                        println!("Launchpad detected on Main Thread! Handing off to worker...");
                        let _ = self.midi_sender.send(midi::MidiCommand::Connect(Box::new(payload)));
                        self.midi_connected = true;
                    }
                } else {
                    // WATCHDOG: Check if still connected
                    // We create a temporary input to check ports.
                    // This is lightweight enough to do every few seconds.
                    let is_present = if let Ok(mut watcher) = midir::MidiInput::new("Watchdog") {
                        watcher.ignore(midir::Ignore::None);
                        watcher.ports().iter().any(|p| {
                            let name = watcher.port_name(p).unwrap_or_default();
                            name.contains("Launchpad")
                        })
                    } else {
                        false
                    };

                    if !is_present {
                        println!("Watchdog: Launchpad disappeared from port list. Disconnecting...");
                        let _ = self.midi_sender.send(midi::MidiCommand::Disconnect);
                        self.midi_connected = false;
                    }
                }
            }
        } else if self.midi_connected {
            // MIDI was disabled while connected - disconnect
            let _ = self.midi_sender.send(midi::MidiCommand::Disconnect);
            self.midi_connected = false;
        }

        // Handle MIDI Input
        while let Ok(event) = self.midi_receiver.try_recv() {
            match event {
                midi::MidiEvent::NoteOn { note, velocity: _ } => {
                     // Check for scene mapped to this note (and is NOT cc)
                     if let Some(s) = self.state.scenes.iter().find(|s| !s.launchpad_is_cc && s.launchpad_btn == Some(note)) {
                         self.state.selected_scene_id = Some(s.id);
                     }
                }
                midi::MidiEvent::ControlChange { controller, value: _ } => {
                     // Check for scene mapped to this CC
                     if let Some(s) = self.state.scenes.iter().find(|s| s.launchpad_is_cc && s.launchpad_btn == Some(controller)) {
                         self.state.selected_scene_id = Some(s.id);
                     }
                }
                midi::MidiEvent::Connected => {
                    println!("Launchpad connected! Refreshing button colors...");
                    self.midi_connected = true;
                    // Clear all buttons
                    let _ = self.midi_sender.send(midi::MidiCommand::ClearAll);
                    // Resend all scene button colors
                    for s in &self.state.scenes {
                        if let (Some(btn), Some(col)) = (s.launchpad_btn, s.launchpad_color) {
                            let cmd = if s.launchpad_is_cc {
                                midi::MidiCommand::SetButtonColor { cc: btn, color: col }
                            } else {
                                midi::MidiCommand::SetPadColor { note: btn, color: col }
                            };
                            let _ = self.midi_sender.send(cmd);
                        }
                    }
                }
                midi::MidiEvent::Disconnected => {
                    println!("Launchpad disconnected. Will retry connection...");
                    self.midi_connected = false;
                    self.last_midi_detection = Some(std::time::Instant::now()); // Delay retry slightly
                }
            }
        }

        // Import confirmation dialog
        if self.import_dialog_open {
            egui::Window::new("Import from JSON")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Import will update your current configuration.");
                    ui.label("Make sure you have saved any changes first!");

                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.import_merge_mode, false, "Replace All");
                        ui.radio_value(&mut self.import_merge_mode, true, "Merge (add scenes/strips)");
                    });

                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.import_dialog_open = false;
                        }

                        if ui.button("Import").clicked() {
                            self.do_import();
                            self.import_dialog_open = false;
                        }
                    });
                });
        }

        // Scene Manager Window
        if self.scene_manager_open {
            egui::Window::new("Scene Manager")
                .default_width(800.0)
                .default_height(600.0)
                .resizable(true)
                .show(ctx, |ui| {
                    // Collect unique categories
                    let mut categories: Vec<String> = self.state.scenes
                        .iter()
                        .map(|s| s.category.clone())
                        .collect::<std::collections::HashSet<_>>()
                        .into_iter()
                        .collect();
                    categories.sort();

                    // Top bar with close button and category filters
                    ui.horizontal(|ui| {
                        ui.heading("Scene Manager");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("✖ Close").clicked() {
                                self.scene_manager_open = false;
                            }
                        });
                    });

                    ui.separator();

                    // Category filter buttons
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Filter:");
                        if ui.selectable_label(self.scene_manager_category_filter.is_none(), "All").clicked() {
                            self.scene_manager_category_filter = None;
                        }
                        for cat in &categories {
                            let is_selected = self.scene_manager_category_filter.as_ref() == Some(cat);
                            if ui.selectable_label(is_selected, cat).clicked() {
                                self.scene_manager_category_filter = Some(cat.clone());
                            }
                        }
                    });

                    ui.separator();

                    // Main scrollable content area
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {

                    // MIDI Board Visualization - Collapsible
                    egui::CollapsingHeader::new("🎹 Launchpad Mapping")
                        .default_open(false)
                        .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(400.0)
                            .show(ui, |ui| {
                        let cell_size = 40.0;
                        let _spacing = 2.0;

                        // Build a map of button -> scene
                        let mut pad_map: std::collections::HashMap<u8, &model::Scene> = std::collections::HashMap::new();

                        for scene in &self.state.scenes {
                            if let Some(btn) = scene.launchpad_btn {
                                if btn > 0 && !scene.launchpad_is_cc {
                                    pad_map.insert(btn, scene);
                                }
                            }
                        }

                        // Draw 8x8 grid of pads (top to bottom)
                        for row in 0..8 {
                            ui.horizontal(|ui| {
                                for col in 0..8 {
                                    // Launchpad layout: bottom row = 11-18, top row = 81-88
                                    // row 0 = top (81-88), row 7 = bottom (11-18)
                                    let note = ((8 - row) * 10 + 1 + col) as u8;
                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(cell_size, cell_size),
                                        egui::Sense::hover()
                                    );

                                    // Determine color and label
                                    let (bg_color, text, text_color) = if let Some(scene) = pad_map.get(&note) {
                                        let color = scene.launchpad_color.unwrap_or(0);
                                        let rgb = launchpad_color_to_rgb(color);
                                        (
                                            egui::Color32::from_rgb(rgb.0, rgb.1, rgb.2),
                                            scene.name.chars().next().unwrap_or('?').to_string(),
                                            if rgb.0 as u32 + rgb.1 as u32 + rgb.2 as u32 > 384 {
                                                egui::Color32::BLACK
                                            } else {
                                                egui::Color32::WHITE
                                            }
                                        )
                                    } else {
                                        (
                                            egui::Color32::from_gray(40),
                                            note.to_string(),
                                            egui::Color32::GRAY
                                        )
                                    };

                                    ui.painter().rect_filled(rect, 2.0, bg_color);
                                    ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::from_gray(100)));

                                    let galley = ui.painter().layout_no_wrap(
                                        text,
                                        egui::FontId::proportional(12.0),
                                        text_color
                                    );
                                    let text_pos = rect.center() - egui::vec2(galley.size().x / 2.0, galley.size().y / 2.0);
                                    ui.painter().galley(text_pos, galley);

                                    if response.hovered() {
                                        if let Some(scene) = pad_map.get(&note) {
                                            response.on_hover_text(format!("{}\nNote: {}", scene.name, note));
                                        } else {
                                            response.on_hover_text(format!("Note: {}", note));
                                        }
                                    }
                                }
                            });
                        }
                    });
                        });

                    ui.separator();

                    // Grid of scene cards
                    ui.heading("Scenes");
                    let card_width = 180.0;
                    let card_height = 100.0;
                    let spacing = 10.0;
                    let available_width = ui.available_width();
                    let cols = ((available_width + spacing) / (card_width + spacing)).max(1.0) as usize;

                    // Filter scenes by category
                    let filtered_scenes: Vec<_> = self.state.scenes
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| {
                            self.scene_manager_category_filter.as_ref()
                                .map(|filter| &s.category == filter)
                                .unwrap_or(true)
                        })
                        .collect();

                    // Display in grid
                    for row_scenes in filtered_scenes.chunks(cols) {
                        ui.horizontal(|ui| {
                            for (_idx, scene) in row_scenes {
                                    let is_selected = self.state.selected_scene_id == Some(scene.id);

                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(card_width, card_height),
                                        egui::Sense::click()
                                    );

                                    if response.clicked() {
                                        self.state.selected_scene_id = Some(scene.id);
                                    }

                                    let visuals = if is_selected {
                                        ui.style().visuals.widgets.active
                                    } else if response.hovered() {
                                        ui.style().visuals.widgets.hovered
                                    } else {
                                        ui.style().visuals.widgets.inactive
                                    };

                                    ui.painter().rect(
                                        rect,
                                        3.0,
                                        visuals.bg_fill,
                                        visuals.bg_stroke
                                    );

                                    let mut text_rect = rect.shrink(8.0);

                                    // Scene name
                                    let name_galley = ui.painter().layout_no_wrap(
                                        scene.name.clone(),
                                        egui::FontId::proportional(16.0),
                                        visuals.text_color()
                                    );
                                    ui.painter().galley(
                                        egui::pos2(text_rect.left(), text_rect.top()),
                                        name_galley
                                    );

                                    // Category badge
                                    let category_text = format!("📁 {}", scene.category);
                                    let cat_galley = ui.painter().layout_no_wrap(
                                        category_text,
                                        egui::FontId::proportional(12.0),
                                        ui.style().visuals.weak_text_color()
                                    );
                                    ui.painter().galley(
                                        egui::pos2(text_rect.left(), text_rect.top() + 25.0),
                                        cat_galley
                                    );

                                    // Kind badge (Masks/Global)
                                    let kind_text = scene.kind.clone();
                                    let kind_galley = ui.painter().layout_no_wrap(
                                        kind_text,
                                        egui::FontId::proportional(11.0),
                                        ui.style().visuals.weak_text_color()
                                    );
                                    ui.painter().galley(
                                        egui::pos2(text_rect.left(), text_rect.bottom() - 15.0),
                                        kind_galley
                                    );

                                    ui.add_space(spacing);
                                }
                            });
                            ui.add_space(spacing);
                        }

                    if filtered_scenes.is_empty() {
                        ui.centered_and_justified(|ui| {
                            ui.label("No scenes in this category");
                        });
                    }
                    }); // End of main scroll area
                });
        }

        // Menu Bar
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {

            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Save Config").clicked() {
                        self.save_state();
                        ui.close_menu();
                    }

                    ui.separator();

                    if ui.button("Export to JSON...").clicked() {
                        self.export_to_json();
                        ui.close_menu();
                    }

                    if ui.button("Import from JSON...").clicked() {
                        self.import_from_json();
                        ui.close_menu();
                    }

                    ui.separator();

                    if ui.button("Reveal Config in Finder").clicked() {
                        let p = user_db_path();
                        reveal_in_file_manager(&p);
                        self.status = "Opened config location".into();
                        ui.close_menu();
                    }
                });
            });
        });
        
        // Update Loop (Physics/Networking)
        self.engine.update(&mut self.state);

        egui::CentralPanel::default().show(ctx, |ui| {
            // HEADER AND STATUS
            ui.horizontal(|ui| {
                ui.heading("Lightspeed");
                ui.separator();
                
                // Unified Sync Status
                let (source, bpm) = self.engine.get_sync_info();
                let source_color = if source.starts_with("LINK") { egui::Color32::GREEN } 
                                   else if source == "AUDIO" { egui::Color32::from_rgb(100, 200, 255) } // Cyan/Blue
                                   else { egui::Color32::LIGHT_GRAY };
                
                ui.label(egui::RichText::new(source).color(source_color).strong());
                
                let beat = self.engine.get_beat();
                let beat_in_bar = ((beat % 4.0).floor() as i32) + 1;
                
                // Beat Indicator using progress bar or text
                // Let's use text for now as requested "transparent"
                ui.separator();
                ui.label(egui::RichText::new(format!("{:.1} BPM", bpm)).size(18.0).strong());
                ui.label(egui::RichText::new(format!("Beat: {}", beat_in_bar)).size(18.0));
                
                // Small visual metronome?
                let phase = beat.fract();
                if phase < 0.2 {
                    ui.label("🔴");
                } else {
                    ui.label("⚪");
                }

                ui.separator();

                if ui.button("Save Config").clicked() {
                    self.save_state();
                    self.status = "Saved".into();
                }
                ui.label(&self.status);
            });
            ui.separator(); // This separator is *after* the horizontal block.

            ui.columns(2, |columns| {
                // LEFT PANEL: CONTROLS
                columns[0].vertical(|ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        
                        // GLOBAL
                        ui.collapsing("Global Settings", |ui| {
                            ui.horizontal(|ui| {
                                 ui.label("Master Speed");
                                 ui.add(egui::Slider::new(&mut self.engine.speed, 0.1..=5.0));
                            });
                            ui.horizontal(|ui| {
                                 ui.label("Audio Latency (ms)");
                                 ui.add(egui::Slider::new(&mut self.state.audio.latency_ms, -200.0..=500.0));
                            });
                            ui.horizontal(|ui| {
                                 ui.checkbox(&mut self.state.audio.use_flywheel, "Beat Smoothing (Flywheel)");
                            });
                            ui.separator();
                            ui.label("Hybrid Sync (Audio)");
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut self.state.audio.hybrid_sync, "Enable Audio Snap");
                                if self.state.audio.hybrid_sync {
                                     ui.add(egui::Slider::new(&mut self.state.audio.sensitivity, 0.0..=1.0).text("Sens"));
                                }
                            });
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut self.state.midi_enabled, "Enable MIDI (Launchpad)");
                            });
                        });
                        
                        ui.collapsing("Network Output", |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Universe");
                                ui.add(egui::DragValue::new(&mut self.state.network.universe).speed(1).clamp_range(1..=63999));
                            });
                            
                            ui.checkbox(&mut self.state.network.use_multicast, "Multicast (Broadcast)");
                            
                            if !self.state.network.use_multicast {
                                ui.horizontal(|ui| {
                                    ui.label("IP Address");
                                    ui.text_edit_singleline(&mut self.state.network.unicast_ip);
                                });
                            }
                        });
                        
                        ui.separator();

                        // Scenes UI will be shown after Strips to keep Strips on top

                        // STRIPS
                        ui.horizontal(|ui| {
                            ui.heading("Strips");
                            if ui.button("➕ Add Strip").clicked() {
                                let mut s = PixelStrip::default();
                                s.id = rand::random();
                                self.state.strips.push(s);
                                self.mark_state_changed();
                            }
                        });
                        
                        let mut delete_strip_idx = None;
                        for (idx, s) in self.state.strips.iter_mut().enumerate() {
                            ui.push_id(s.id, |ui| {
                                ui.collapsing(format!("Strip::{}", s.id), |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label("Position:");
                                        ui.add(egui::DragValue::new(&mut s.x).speed(0.01).prefix("X: "));
                                        ui.add(egui::DragValue::new(&mut s.y).speed(0.01).prefix("Y: "));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Direction:");
                                        ui.checkbox(&mut s.flipped, "Flip 180°");
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Config:");
                                        ui.add(egui::DragValue::new(&mut s.universe).prefix("Uni: ").clamp_range(1..=63999));
                                        ui.add(egui::DragValue::new(&mut s.start_channel).prefix("Ch: "));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Layout:");
                                        ui.add(egui::DragValue::new(&mut s.pixel_count).prefix("Count: "));
                                        ui.add(egui::Slider::new(&mut s.spacing, 0.001..=0.05).text("Spacing"));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Protocol:");
                                        egui::ComboBox::from_id_source(format!("proto_{}", s.id))
                                            .selected_text(&s.color_order)
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(&mut s.color_order, "RGB".to_string(), "RGB");
                                                ui.selectable_value(&mut s.color_order, "GRB".to_string(), "GRB");
                                                ui.selectable_value(&mut s.color_order, "BGR".to_string(), "BGR");
                                            });
                                    });
                                    
                                    if ui.button("🗑 Delete Strip").clicked() {
                                        delete_strip_idx = Some(idx);
                                    }
                                });
                            });
                        }
                        if let Some(idx) = delete_strip_idx {
                            self.state.strips.remove(idx);
                        }

                        ui.separator();
                        // STRIPS are shown above; now show Scenes with embedded Masks editors
                        ui.heading("Scenes");
                        ui.horizontal(|ui| {
                            if ui.button("➕ Add Scene").clicked() {
                                self.new_scene_open = true;
                                self.new_scene_name = format!("Scene {}", self.state.scenes.len() + 1);
                                self.new_scene_kind = "Masks".into();
                                self.new_scene_category = "Uncategorized".into();
                            }
                            if ui.button("📋 Scene Manager").clicked() {
                                self.scene_manager_open = true;
                            }
                            if !self.state.scenes.is_empty() {
                                if ui.button("Select None").clicked() { self.state.selected_scene_id = None; }
                            }
                        });

                        // Collect existing categories once and reuse everywhere
                        let existing_categories: Vec<String> = if !self.state.scenes.is_empty() {
                            let mut cats: Vec<String> = self.state.scenes
                                .iter()
                                .map(|s| s.category.clone())
                                .collect::<std::collections::HashSet<_>>()
                                .into_iter()
                                .collect();
                            cats.sort();
                            cats
                        } else {
                            vec![]
                        };

                        // Filters for main panel
                        if !self.state.scenes.is_empty() {

                            ui.horizontal_wrapped(|ui| {
                                ui.label("Category:");
                                if ui.selectable_label(self.main_scenes_category_filter.is_none(), "All").clicked() {
                                    self.main_scenes_category_filter = None;
                                }
                                for cat in &existing_categories {
                                    let is_selected = self.main_scenes_category_filter.as_ref() == Some(cat);
                                    if ui.selectable_label(is_selected, cat).clicked() {
                                        self.main_scenes_category_filter = Some(cat.clone());
                                    }
                                }
                            });

                            // MIDI filter
                            ui.horizontal_wrapped(|ui| {
                                ui.label("MIDI:");
                                if ui.selectable_label(self.main_scenes_midi_filter == MidiFilter::All, "All").clicked() {
                                    self.main_scenes_midi_filter = MidiFilter::All;
                                }
                                if ui.selectable_label(self.main_scenes_midi_filter == MidiFilter::Linked, "Linked").clicked() {
                                    self.main_scenes_midi_filter = MidiFilter::Linked;
                                }
                                if ui.selectable_label(self.main_scenes_midi_filter == MidiFilter::NotLinked, "Not Linked").clicked() {
                                    self.main_scenes_midi_filter = MidiFilter::NotLinked;
                                }
                            });
                        }
                        if self.new_scene_open {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Name:");
                                    ui.text_edit_singleline(&mut self.new_scene_name);
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Category:");
                                    egui::ComboBox::from_id_source("new_scene_category")
                                        .selected_text(&self.new_scene_category)
                                        .show_ui(ui, |ui| {
                                            for cat in &existing_categories {
                                                ui.selectable_value(&mut self.new_scene_category, cat.clone(), cat);
                                            }
                                        });
                                    ui.text_edit_singleline(&mut self.new_scene_category);
                                });
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut self.new_scene_kind, "Masks".into(), "Masks");
                                    ui.selectable_value(&mut self.new_scene_kind, "Global".into(), "Global effect");
                                });
                                ui.horizontal(|ui| {
                                    if ui.button("Create").clicked() {
                                        let id = rand::random();
                                        let scene = if self.new_scene_kind == "Masks" {
                                            model::Scene {
                                                id,
                                                name: self.new_scene_name.clone(),
                                                kind: "Masks".into(),
                                                category: self.new_scene_category.clone(),
                                                masks: vec![],
                                                global: None,
                                                global_effects: vec![],
                                                launchpad_btn: None,
                                                launchpad_color: None,
                                                launchpad_is_cc: false
                                            }
                                        } else {
                                            let mut ge = model::GlobalEffect::default();
                                            ge.params.insert("speed".into(), 0.2.into());
                                            model::Scene {
                                                 id,
                                                 name: self.new_scene_name.clone(),
                                                 kind: "Global".into(),
                                                 category: self.new_scene_category.clone(),
                                                 masks: vec![],
                                                 global: None,
                                                 global_effects: vec![model::GlobalEffectConfig {
                                                     effect: ge,
                                                     targets: None,
                                                 }],
                                                 launchpad_btn: None,
                                                 launchpad_color: None,
                                                 launchpad_is_cc: false
                                            }
                                        };
                                        self.state.scenes.push(scene);
                                        self.state.selected_scene_id = Some(id);
                                        self.new_scene_open = false;
                                    }
                                    if ui.button("Cancel").clicked() { self.new_scene_open = false; }
                                });
                            });
                        }

                        // Scenes list with per-scene editors
                        let mut delete_scene_idx: Option<usize> = None;
                        let mut duplicate_scene_idx: Option<usize> = None;
                        let mut swap_request: Option<(usize, usize)> = None;
                        let mut floating_scene: Option<model::Scene> = None;
                        let mut needs_save = false;
                        let sender = self.midi_sender.clone();
                        
                        // Pre-calculate dragged index to avoid borrow issues
                        let dragged_scene_index = self.dragged_scene_id.and_then(|id| {
                            self.state.scenes.iter().position(|s| s.id == id)
                        });

                        // Collect used IDs to prevent duplicates
                        let mut used_ids = std::collections::HashMap::new();
                        for s in &self.state.scenes {
                            if let Some(btn) = s.launchpad_btn {
                                if btn != 0 {
                                    used_ids.insert((s.launchpad_is_cc, btn), s.id);
                                }
                            }
                        }

                        // Collect strip info needed for UI (id, index)
                        let available_strips: Vec<(u64, usize)> = self.state.strips.iter().enumerate().map(|(i, s)| (s.id, i)).collect();

                        for (si, scene) in self.state.scenes.iter_mut().enumerate() {
                            // Apply category filter
                            if let Some(ref filter) = self.main_scenes_category_filter {
                                if &scene.category != filter {
                                    continue;
                                }
                            }

                            // Apply MIDI filter
                            match self.main_scenes_midi_filter {
                                MidiFilter::Linked => {
                                    if scene.launchpad_btn.is_none() {
                                        continue;
                                    }
                                }
                                MidiFilter::NotLinked => {
                                    if scene.launchpad_btn.is_some() {
                                        continue;
                                    }
                                }
                                MidiFilter::All => {}
                            }

                            ui.push_id(scene.id, |ui| {
                                ui.separator();

                                    // Floating Drag Logic
                                let is_being_dragged = self.dragged_scene_id == Some(scene.id);
                                let row_rect = if is_being_dragged {
                                    floating_scene = Some(scene.clone());
                                    // Placeholder
                                    let resp = ui.horizontal(|ui| {
                                        ui.add(egui::Label::new("       ").sense(egui::Sense::hover())); // Spacing
                                        ui.label(egui::RichText::new(&scene.name).italics().color(egui::Color32::DARK_GRAY));
                                    }).response;
                                    Some(resp.rect)
                                } else {
                                    let selected = self.state.selected_scene_id == Some(scene.id);
                                    let inner_resp = ui.horizontal(|ui| {
                                        // Drag Handle
                                        let resp = ui.add(egui::Button::new("::").frame(false).sense(egui::Sense::drag()));
                                        if resp.drag_started() {
                                            self.dragged_scene_id = Some(scene.id);
                                        }
                                        
                                        // Make sure we update the floating scene if we just started dragging
                                        if self.dragged_scene_id == Some(scene.id) {
                                             // Next frame it will be caught by is_being_dragged above
                                        }

                                        if ui.selectable_label(selected, &scene.name).clicked() {
                                            self.state.selected_scene_id = Some(scene.id);
                                        }
                                        ui.text_edit_singleline(&mut scene.name);
                                        if ui.button("📋").on_hover_text("Duplicate").clicked() { duplicate_scene_idx = Some(si); }
                                        if ui.button("X").clicked() { delete_scene_idx = Some(si); }
                                    });
                                    Some(inner_resp.response.rect)
                                };

                                // Check for drag-over (Live Reorder) on the entire row
                                if let Some(r) = row_rect {
                                    if let Some(from_idx) = dragged_scene_index {
                                        if from_idx != si && r.contains(ui.input(|i| i.pointer.interact_pos().unwrap_or_default())) {
                                            swap_request = Some((from_idx, si));
                                        }
                                    }
                                }
                                
                                if !is_being_dragged {
                                    let selected = self.state.selected_scene_id == Some(scene.id);
                                    if selected {
                                // Category Editor
                                ui.horizontal(|ui| {
                                    ui.label("Category:");
                                    if egui::ComboBox::from_id_source(format!("scene_cat_{}", scene.id))
                                        .selected_text(&scene.category)
                                        .show_ui(ui, |ui| {
                                            for cat in &existing_categories {
                                                ui.selectable_value(&mut scene.category, cat.clone(), cat);
                                            }
                                        }).inner.is_some() {
                                        needs_save = true;
                                    }
                                    if ui.text_edit_singleline(&mut scene.category).changed() {
                                        needs_save = true;
                                    }
                                });
                                // Launchpad Config
                                ui.horizontal(|ui| {
                                    ui.label("Launchpad Pad:");

                                    // Always use Notes (not CC)
                                    scene.launchpad_is_cc = false;

                                    // Generate valid note values (8 rows, 8 columns)
                                    let mut valid_notes = Vec::new();
                                    for row in 0..8 {
                                        for col in 0..8 {
                                            let note = ((8 - row) * 10 + 1 + col) as u8;
                                            valid_notes.push(note);
                                        }
                                    }

                                    let old_btn = scene.launchpad_btn;
                                    let current_note = scene.launchpad_btn.unwrap_or(0);
                                    let display_text = if current_note == 0 {
                                        "None".to_string()
                                    } else {
                                        format!("Note {}", current_note)
                                    };

                                    let mut changed = false;
                                    let mut new_note = current_note;

                                    egui::ComboBox::from_id_source(format!("lp_note_{}", scene.id))
                                        .selected_text(display_text)
                                        .show_ui(ui, |ui| {
                                            if ui.selectable_value(&mut new_note, 0, "None").clicked() {
                                                changed = true;
                                            }
                                            ui.separator();

                                            // Show notes in grid layout by row
                                            for row in 0..8 {
                                                ui.horizontal(|ui| {
                                                    for col in 0..8 {
                                                        let note = ((8 - row) * 10 + 1 + col) as u8;
                                                        // Check if already used by another scene
                                                        let is_used = if let Some(&owner) = used_ids.get(&(false, note)) {
                                                            owner != scene.id
                                                        } else {
                                                            false
                                                        };

                                                        let label = if is_used {
                                                            format!("{}✓", note)
                                                        } else {
                                                            format!("{}", note)
                                                        };

                                                        if ui.selectable_value(&mut new_note, note, label).clicked() {
                                                            if !is_used {
                                                                changed = true;
                                                            }
                                                        }
                                                    }
                                                });
                                            }
                                        });

                                    if changed && new_note != current_note {
                                        // Turn off old pad
                                        if let Some(old) = old_btn {
                                            let _ = sender.send(midi::MidiCommand::SetPadColor { note: old, color: 0 });
                                        }

                                        scene.launchpad_btn = if new_note == 0 { None } else { Some(new_note) };

                                        // Send new pad color
                                        if let (Some(note), Some(col)) = (scene.launchpad_btn, scene.launchpad_color) {
                                            let _ = sender.send(midi::MidiCommand::SetPadColor { note, color: col });
                                        }

                                        needs_save = true;
                                    }

                                    let mut col = scene.launchpad_color.unwrap_or(0);
                                    if launchpad_color_picker_ui(ui, &mut col) {
                                        scene.launchpad_color = Some(col);
                                        // Send to board immediately
                                        if let Some(note) = scene.launchpad_btn {
                                            let _ = sender.send(midi::MidiCommand::SetPadColor { note, color: col });
                                        }
                                        needs_save = true;
                                    }
                                });
                                if scene.kind == "Global" {
                                    ui.horizontal(|ui| {
                                        ui.label("Global Effects:");
                                        if ui.button("➕ Add Effect").clicked() {
                                             scene.global_effects.push(model::GlobalEffectConfig {
                                                 effect: model::GlobalEffect::default(),
                                                 targets: None
                                             });
                                        }
                                    });

                                    let mut delete_effect_idx = None;
                                    for (eff_idx, config) in scene.global_effects.iter_mut().enumerate() {
                                        ui.push_id(format!("ge_{}_{}", scene.id, eff_idx), |ui| {
                                            ui.group(|ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label(format!("#{}", eff_idx + 1));
                                                    
                                                    // TARGET SELECTOR
                                                    // TARGET SELECTOR
                                                    let label_text = match &config.targets {
                                                        None => "Targets: All Strips".to_string(),
                                                        Some(t) => {
                                                            if t.is_empty() { "Targets: None".to_string() }
                                                            else { format!("Targets: {} Strips", t.len()) }
                                                        }
                                                    };

                                                    egui::CollapsingHeader::new(label_text)
                                                        .id_source(format!("target_sel_{}_{}", scene.id, eff_idx))
                                                        .show(ui, |ui| {
                                                            if ui.selectable_label(config.targets.is_none(), "All Strips").clicked() {
                                                                config.targets = None;
                                                            }
                                                            ui.separator();
                                                            
                                                            for (sid, sidx) in &available_strips {
                                                                let mut is_selected = if let Some(t) = &config.targets {
                                                                    t.contains(sid)
                                                                } else {
                                                                    true 
                                                                };
                                                                
                                                                if ui.checkbox(&mut is_selected, format!("Strip #{}", sidx + 1)).clicked() {
                                                                    if config.targets.is_none() {
                                                                        // Was All.
                                                                        if !is_selected {
                                                                            // Unchecked one item. Switch to All-minus-one
                                                                            let mut new_t: Vec<u64> = available_strips.iter().map(|(id, _)| *id).collect();
                                                                            new_t.retain(|x| x != sid);
                                                                            config.targets = Some(new_t);
                                                                        }
                                                                    } else {
                                                                        // Was Selective
                                                                        let t = config.targets.as_mut().unwrap();
                                                                        if is_selected {
                                                                            if !t.contains(sid) { t.push(*sid); }
                                                                        } else {
                                                                            t.retain(|x| x != sid);
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        });
                                                        
                                                    // Type Selector
                                                    egui::ComboBox::from_id_source("kind_sel")
                                                        .selected_text(&config.effect.kind)
                                                        .show_ui(ui, |ui| {
                                                            ui.selectable_value(&mut config.effect.kind, "Rainbow".into(), "Rainbow");
                                                            ui.selectable_value(&mut config.effect.kind, "Solid".into(), "Solid");
                                                            ui.selectable_value(&mut config.effect.kind, "Flash".into(), "Flash");
                                                            ui.selectable_value(&mut config.effect.kind, "Sparkle".into(), "Sparkle");
                                                            ui.selectable_value(&mut config.effect.kind, "ColorWash".into(), "Color Wash");
                                                            ui.selectable_value(&mut config.effect.kind, "GlitchSparkle".into(), "Glitch Sparkle");
                                                            ui.selectable_value(&mut config.effect.kind, "PulseWave".into(), "Pulse Wave");
                                                            ui.selectable_value(&mut config.effect.kind, "ZoneAlternate".into(), "Zone Alternate");
                                                        });
                                                        
                                                    if ui.button("🗑").clicked() {
                                                        delete_effect_idx = Some(eff_idx);
                                                    }
                                                });
                                                
                                                // Target checkboxes (Custom UI instead of simple combobox for multi-select)
                                                // We need strip info. Since we can't access `self.state.strips` here easily,
                                                // we should have pre-calculated a list of (id, name/info).
                                                // See below for fix.
                                                
                                                // Render Effect Params
                                                let ge = &mut config.effect;
                                                // ... (Reusing existing UI logic, but refactored to check `ge`)
                                                // INLINED FOR NOW:
                                                if ge.kind == "Solid" {
                                                    let mut color = ge.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,255,255]);
                                                    if color_picker(ui, &mut color, format!("ge_sol_{}_{}", scene.id, eff_idx)) {
                                                        ge.params.insert("color".into(), serde_json::json!([color[0], color[1], color[2]]));
                                                    }
                                                } else if ge.kind == "Flash" {
                                                    ui.horizontal(|ui| {
                                                        ui.label("Color:");
                                                         let mut color = ge.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,255,255]);
                                                        if color_picker(ui, &mut color, format!("ge_fl_{}_{}", scene.id, eff_idx)) {
                                                            ge.params.insert("color".into(), serde_json::json!([color[0], color[1], color[2]]));
                                                        }
                                                    });
                                                    ui.horizontal(|ui| {
                                                        ui.label("Rate:");
                                                        let mut rate = ge.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1 Bar").to_string();
                                                        egui::ComboBox::from_id_source("rate")
                                                            .selected_text(rate.clone())
                                                            .show_ui(ui, |ui| {
                                                                ui.selectable_value(&mut rate, "4 Bar".into(), "4 Bar");
                                                                ui.selectable_value(&mut rate, "1 Bar".into(), "1 Bar");
                                                                ui.selectable_value(&mut rate, "1/2".into(), "1/2");
                                                                ui.selectable_value(&mut rate, "1/4".into(), "1/4");
                                                                ui.selectable_value(&mut rate, "1/8".into(), "1/8");
                                                            });
                                                        if rate != ge.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1 Bar") {
                                                            ge.params.insert("rate".into(), serde_json::json!(rate));
                                                        }
                                                    });
                                                    let mut decay = ge.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(5.0);
                                                    if ui.add(egui::Slider::new(&mut decay, 0.1..=20.0).text("Decay")).changed() {
                                                        ge.params.insert("decay".into(), decay.into());
                                                    }
                                                } else if ge.kind == "Sparkle" {
                                                    ui.horizontal(|ui| {
                                                        ui.label("Color:");
                                                        let mut color = ge.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,255,255]);
                                                        if color_picker(ui, &mut color, format!("ge_spk_{}_{}", scene.id, eff_idx)) {
                                                            ge.params.insert("color".into(), serde_json::json!([color[0], color[1], color[2]]));
                                                        }
                                                    });
                                                    let mut density = ge.params.get("density").and_then(|v| v.as_f64()).unwrap_or(0.05);
                                                    if ui.add(egui::Slider::new(&mut density, 0.001..=0.2).text("Density")).changed() {
                                                        ge.params.insert("density".into(), density.into());
                                                    }
                                                    let mut life = ge.params.get("life").and_then(|v| v.as_f64()).unwrap_or(0.2);
                                                    if ui.add(egui::Slider::new(&mut life, 0.05..=2.0).text("Life")).changed() {
                                                        ge.params.insert("life".into(), life.into());
                                                    }
                                                    let mut decay = ge.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(5.0);
                                                    if ui.add(egui::Slider::new(&mut decay, 0.1..=20.0).text("Decay")).changed() {
                                                        ge.params.insert("decay".into(), decay.into());
                                                    }
                                                } else if ge.kind == "ColorWash" {
                                                    ui.horizontal(|ui| {
                                                        ui.label("Color A:");
                                                        let mut color_a = ge.params.get("color_a").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,0,0]);
                                                        if color_picker(ui, &mut color_a, format!("ge_cw_a_{}_{}", scene.id, eff_idx)) {
                                                            ge.params.insert("color_a".into(), serde_json::json!([color_a[0], color_a[1], color_a[2]]));
                                                        }
                                                    });
                                                    ui.horizontal(|ui| {
                                                        ui.label("Color B:");
                                                        let mut color_b = ge.params.get("color_b").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([0u8,0,255]);
                                                        if color_picker(ui, &mut color_b, format!("ge_cw_b_{}_{}", scene.id, eff_idx)) {
                                                            ge.params.insert("color_b".into(), serde_json::json!([color_b[0], color_b[1], color_b[2]]));
                                                        }
                                                    });
                                                    let mut sync_to_beat = ge.params.get("sync_to_beat").and_then(|v| v.as_bool()).unwrap_or(false);
                                                    if ui.checkbox(&mut sync_to_beat, "Sync to Beat").changed() {
                                                        ge.params.insert("sync_to_beat".into(), sync_to_beat.into());
                                                    }
                                                    if sync_to_beat {
                                                        ui.horizontal(|ui| {
                                                            ui.label("Rate:");
                                                            let mut rate = ge.params.get("rate").and_then(|v| v.as_str().map(String::from)).unwrap_or("1 Bar".into());
                                                            egui::ComboBox::from_id_source(format!("cw_rate_{}_{}", scene.id, eff_idx))
                                                                .selected_text(&rate)
                                                                .show_ui(ui, |ui| {
                                                                    ui.selectable_value(&mut rate, "4 Bar".into(), "4 Bar");
                                                                    ui.selectable_value(&mut rate, "2 Bar".into(), "2 Bar");
                                                                    ui.selectable_value(&mut rate, "1 Bar".into(), "1 Bar");
                                                                    ui.selectable_value(&mut rate, "1/2".into(), "1/2");
                                                                    ui.selectable_value(&mut rate, "1/4".into(), "1/4");
                                                                    ui.selectable_value(&mut rate, "1/8".into(), "1/8");
                                                                });
                                                            ge.params.insert("rate".into(), serde_json::json!(rate));
                                                        });
                                                    } else {
                                                        let mut period = ge.params.get("period").and_then(|v| v.as_f64()).unwrap_or(4.0);
                                                        if ui.add(egui::Slider::new(&mut period, 0.5..=20.0).text("Period (s)")).changed() {
                                                            ge.params.insert("period".into(), period.into());
                                                        }
                                                    }
                                                } else if ge.kind == "GlitchSparkle" {
                                                    ui.horizontal(|ui| {
                                                        ui.label("Background:");
                                                        let mut bg_color = ge.params.get("background_color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([0u8,0,0]);
                                                        if color_picker(ui, &mut bg_color, format!("ge_gs_bg_{}_{}", scene.id, eff_idx)) {
                                                            ge.params.insert("background_color".into(), serde_json::json!([bg_color[0], bg_color[1], bg_color[2]]));
                                                        }
                                                    });
                                                    ui.horizontal(|ui| {
                                                        ui.label("Sparkle:");
                                                        let mut spk_color = ge.params.get("sparkle_color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,255,255]);
                                                        if color_picker(ui, &mut spk_color, format!("ge_gs_spk_{}_{}", scene.id, eff_idx)) {
                                                            ge.params.insert("sparkle_color".into(), serde_json::json!([spk_color[0], spk_color[1], spk_color[2]]));
                                                        }
                                                    });
                                                    let mut density = ge.params.get("density").and_then(|v| v.as_f64()).unwrap_or(0.05);
                                                    if ui.add(egui::Slider::new(&mut density, 0.001..=0.2).text("Density")).changed() {
                                                        ge.params.insert("density".into(), density.into());
                                                    }
                                                    let mut fade_time = ge.params.get("fade_time").and_then(|v| v.as_f64()).unwrap_or(0.3);
                                                    if ui.add(egui::Slider::new(&mut fade_time, 0.05..=2.0).text("Fade Time")).changed() {
                                                        ge.params.insert("fade_time".into(), fade_time.into());
                                                    }
                                                    let mut decay = ge.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(5.0);
                                                    if ui.add(egui::Slider::new(&mut decay, 0.1..=20.0).text("Decay")).changed() {
                                                        ge.params.insert("decay".into(), decay.into());
                                                    }
                                                } else if ge.kind == "PulseWave" {
                                                    ui.horizontal(|ui| {
                                                        ui.label("Color:");
                                                        let mut color = ge.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,255,255]);
                                                        if color_picker(ui, &mut color, format!("ge_pw_{}_{}", scene.id, eff_idx)) {
                                                            ge.params.insert("color".into(), serde_json::json!([color[0], color[1], color[2]]));
                                                        }
                                                    });
                                                    let mut sync = ge.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(true);
                                                    if ui.checkbox(&mut sync, "Sync to Beat").changed() {
                                                        ge.params.insert("sync".into(), sync.into());
                                                    }
                                                    if sync {
                                                        ui.horizontal(|ui| {
                                                            ui.label("Rate:");
                                                            let mut rate = ge.params.get("rate").and_then(|v| v.as_str().map(String::from)).unwrap_or("1/4".into());
                                                            egui::ComboBox::from_id_source(format!("pw_rate_{}_{}", scene.id, eff_idx))
                                                                .selected_text(&rate)
                                                                .show_ui(ui, |ui| {
                                                                    ui.selectable_value(&mut rate, "4 Bar".into(), "4 Bar");
                                                                    ui.selectable_value(&mut rate, "2 Bar".into(), "2 Bar");
                                                                    ui.selectable_value(&mut rate, "1 Bar".into(), "1 Bar");
                                                                    ui.selectable_value(&mut rate, "1/2".into(), "1/2");
                                                                    ui.selectable_value(&mut rate, "1/4".into(), "1/4");
                                                                    ui.selectable_value(&mut rate, "1/8".into(), "1/8");
                                                                });
                                                            ge.params.insert("rate".into(), serde_json::json!(rate));
                                                        });
                                                    } else {
                                                        let mut speed = ge.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(5.0);
                                                        if ui.add(egui::Slider::new(&mut speed, 0.1..=50.0).text("Speed (px/s)")).changed() {
                                                            ge.params.insert("speed".into(), speed.into());
                                                        }
                                                    }
                                                    let mut tail_length = ge.params.get("tail_length").and_then(|v| v.as_f64()).unwrap_or(10.0);
                                                    if ui.add(egui::Slider::new(&mut tail_length, 3.0..=50.0).text("Tail Length")).changed() {
                                                        ge.params.insert("tail_length".into(), tail_length.into());
                                                    }
                                                    let mut decay = ge.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(2.0);
                                                    if ui.add(egui::Slider::new(&mut decay, 0.1..=5.0).text("Decay")).changed() {
                                                        ge.params.insert("decay".into(), decay.into());
                                                    }
                                                    ui.horizontal(|ui| {
                                                        ui.label("Direction:");
                                                        let mut direction = ge.params.get("direction").and_then(|v| v.as_str().map(String::from)).unwrap_or("Forward".into());
                                                        egui::ComboBox::from_id_source(format!("pw_dir_{}_{}", scene.id, eff_idx))
                                                            .selected_text(&direction)
                                                            .show_ui(ui, |ui| {
                                                                ui.selectable_value(&mut direction, "Forward".into(), "Forward");
                                                                ui.selectable_value(&mut direction, "Reverse".into(), "Reverse");
                                                                ui.selectable_value(&mut direction, "Bounce".into(), "Bounce");
                                                            });
                                                        ge.params.insert("direction".into(), serde_json::json!(direction));
                                                    });
                                                } else if ge.kind == "ZoneAlternate" {
                                                    ui.horizontal(|ui| {
                                                        ui.label("Group A:");
                                                        let mut color_a = ge.params.get("group_a_color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,0,0]);
                                                        if color_picker(ui, &mut color_a, format!("ge_za_a_{}_{}", scene.id, eff_idx)) {
                                                            ge.params.insert("group_a_color".into(), serde_json::json!([color_a[0], color_a[1], color_a[2]]));
                                                        }
                                                    });
                                                    ui.horizontal(|ui| {
                                                        ui.label("Group B:");
                                                        let mut color_b = ge.params.get("group_b_color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([0u8,0,255]);
                                                        if color_picker(ui, &mut color_b, format!("ge_za_b_{}_{}", scene.id, eff_idx)) {
                                                            ge.params.insert("group_b_color".into(), serde_json::json!([color_b[0], color_b[1], color_b[2]]));
                                                        }
                                                    });
                                                    ui.horizontal(|ui| {
                                                        ui.label("Rate:");
                                                        let mut rate = ge.params.get("rate").and_then(|v| v.as_str().map(String::from)).unwrap_or("1/4".into());
                                                        egui::ComboBox::from_id_source(format!("za_rate_{}_{}", scene.id, eff_idx))
                                                            .selected_text(&rate)
                                                            .show_ui(ui, |ui| {
                                                                ui.selectable_value(&mut rate, "4 Bar".into(), "4 Bar");
                                                                ui.selectable_value(&mut rate, "2 Bar".into(), "2 Bar");
                                                                ui.selectable_value(&mut rate, "1 Bar".into(), "1 Bar");
                                                                ui.selectable_value(&mut rate, "1/2".into(), "1/2");
                                                                ui.selectable_value(&mut rate, "1/4".into(), "1/4");
                                                                ui.selectable_value(&mut rate, "1/8".into(), "1/8");
                                                            });
                                                        ge.params.insert("rate".into(), serde_json::json!(rate));
                                                    });
                                                    ui.horizontal(|ui| {
                                                        ui.label("Mode:");
                                                        let mut mode = ge.params.get("mode").and_then(|v| v.as_str().map(String::from)).unwrap_or("Swap".into());
                                                        egui::ComboBox::from_id_source(format!("za_mode_{}_{}", scene.id, eff_idx))
                                                            .selected_text(&mode)
                                                            .show_ui(ui, |ui| {
                                                                ui.selectable_value(&mut mode, "Swap".into(), "Swap");
                                                                ui.selectable_value(&mut mode, "Pulse".into(), "Pulse");
                                                            });
                                                        ge.params.insert("mode".into(), serde_json::json!(mode));
                                                    });
                                                    egui::CollapsingHeader::new("Assign Strip Groups")
                                                        .id_source(format!("za_groups_{}_{}", scene.id, eff_idx))
                                                        .show(ui, |ui| {
                                                            let mut group_a: Vec<u64> = ge.params.get("group_a_strips")
                                                                .and_then(|v| serde_json::from_value(v.clone()).ok())
                                                                .unwrap_or_default();
                                                            let mut group_b: Vec<u64> = ge.params.get("group_b_strips")
                                                                .and_then(|v| serde_json::from_value(v.clone()).ok())
                                                                .unwrap_or_default();

                                                            for (strip_idx, strip) in self.state.strips.iter().enumerate() {
                                                                ui.horizontal(|ui| {
                                                                    let mut in_a = group_a.contains(&strip.id);
                                                                    let mut in_b = group_b.contains(&strip.id);

                                                                    if ui.checkbox(&mut in_a, "").changed() {
                                                                        if in_a {
                                                                            if !group_a.contains(&strip.id) {
                                                                                group_a.push(strip.id);
                                                                            }
                                                                            group_b.retain(|&id| id != strip.id);
                                                                        } else {
                                                                            group_a.retain(|&id| id != strip.id);
                                                                        }
                                                                    }
                                                                    ui.label("Group A");

                                                                    if ui.checkbox(&mut in_b, "").changed() {
                                                                        if in_b {
                                                                            if !group_b.contains(&strip.id) {
                                                                                group_b.push(strip.id);
                                                                            }
                                                                            group_a.retain(|&id| id != strip.id);
                                                                        } else {
                                                                            group_b.retain(|&id| id != strip.id);
                                                                        }
                                                                    }
                                                                    ui.label("Group B");

                                                                    ui.label(format!("Strip #{}", strip_idx + 1));
                                                                });
                                                            }

                                                            ge.params.insert("group_a_strips".into(), serde_json::json!(group_a));
                                                            ge.params.insert("group_b_strips".into(), serde_json::json!(group_b));
                                                        });
                                                } else { // Rainbow / Default
                                                    let mut speed = ge.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(0.2);
                                                    if ui.add(egui::Slider::new(&mut speed, 0.05..=2.0).text("Speed")).changed() {
                                                        ge.params.insert("speed".into(), speed.into());
                                                    }
                                                    lfo_controls(ui, &mut ge.params, "speed", format!("spd_lfo"));
                                                }
                                            });
                                        });
                                    }
                                    if let Some(idx) = delete_effect_idx {
                                        scene.global_effects.remove(idx);
                                    }
                                } else {
                                    // Embedded Masks editor for this scene
                                    ui.horizontal(|ui| {
                                        ui.label("Masks:");
                                        egui::ComboBox::from_id_source(format!("add_mask_{}", scene.id))
                                            .selected_text("Add Mask...")
                                            .show_ui(ui, |ui| {
                                                if ui.selectable_label(false, "Scanner").clicked() {
                                                    let mut m = Mask { id: rand::random(), mask_type: "scanner".into(), x: 0.5, y: 0.5, params: std::collections::HashMap::new() };
                                                    m.params.insert("width".into(), 0.3.into());
                                                    m.params.insert("height".into(), 0.3.into());
                                                    m.params.insert("speed".into(), 1.0.into());
                                                    m.params.insert("color".into(), serde_json::json!([0, 255, 255]));
                                                    scene.masks.push(m);
                                                }
                                                if ui.selectable_label(false, "Radial").clicked() {
                                                    let mut m = Mask { id: rand::random(), mask_type: "radial".into(), x: 0.5, y: 0.5, params: std::collections::HashMap::new() };
                                                    m.params.insert("radius".into(), 0.2.into());
                                                    m.params.insert("color".into(), serde_json::json!([255, 0, 0]));
                                                    scene.masks.push(m);
                                                }
                                                if ui.selectable_label(false, "Burst").clicked() {
                                                    let mut m = Mask { id: rand::random(), mask_type: "burst".into(), x: 0.5, y: 0.5, params: std::collections::HashMap::new() };
                                                    m.params.insert("base_radius".into(), 0.1.into());
                                                    m.params.insert("max_radius".into(), 0.5.into());
                                                    m.params.insert("sensitivity".into(), 0.5.into());
                                                    m.params.insert("decay".into(), 0.05.into());
                                                    m.params.insert("color".into(), serde_json::json!([255, 100, 0]));
                                                    scene.masks.push(m);
                                                }
                                                if ui.selectable_label(false, "Orbit").clicked() {
                                                    let mut m = Mask { id: rand::random(), mask_type: "orbit".into(), x: 0.5, y: 0.5, params: std::collections::HashMap::new() };
                                                    m.params.insert("width".into(), 0.3.into());
                                                    m.params.insert("height".into(), 0.3.into());
                                                    m.params.insert("bar_width".into(), 0.1.into());
                                                    m.params.insert("speed".into(), 1.0.into());
                                                    m.params.insert("color".into(), serde_json::json!([255, 0, 255]));
                                                    scene.masks.push(m);
                                                }
                                            });
                                    });

                                    let mut delete_mask_idx = None;
                                    for (idx, m) in scene.masks.iter_mut().enumerate() {
                                        ui.push_id(m.id, |ui| {
                                            ui.collapsing(format!("{} Mask::{}", m.mask_type, m.id), |ui| {
                                                ui.horizontal(|ui| {
                                                    if ui.button("🗑 Delete").clicked() {
                                                        delete_mask_idx = Some(idx);
                                                    }
                                                });
                                    
                                    // DYNAMIC PARAMS
                                    if m.mask_type == "scanner" {
                                        // Hard Edge
                                        let mut hard_edge = m.params.get("hard_edge").and_then(|v| v.as_bool()).unwrap_or(false);
                                        if ui.checkbox(&mut hard_edge, "Hard Edge").changed() {
                                            m.params.insert("hard_edge".into(), hard_edge.into());
                                            needs_save = true;
                                        }
                                        
                                        // Speed
                                        let mut s = m.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                                        if ui.add(egui::Slider::new(&mut s, 0.1..=5.0).text("Speed")).changed() {
                                            m.params.insert("speed".into(), s.into());
                                            needs_save = true;
                                        }
                                        // Rotation
                                        let mut rotation = m.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                                        if ui.add(egui::Slider::new(&mut rotation, 0.0..=360.0).text("Rotation")).changed() {
                                            m.params.insert("rotation".into(), rotation.into());
                                            needs_save = true;
                                        }
                                    } else if m.mask_type == "radial" {
                                        let mut r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
                                        if ui.add(egui::Slider::new(&mut r, 0.0..=5.0).text("Radius")).changed() {
                                            m.params.insert("radius".into(), r.into());
                                            needs_save = true;
                                        }
                                        if lfo_controls(ui, &mut m.params, "radius", format!("radius_lfo_{}", m.id)) {
                                            needs_save = true;
                                        }
                                    } else if m.mask_type == "burst" {
                                        let mut base_r = m.params.get("base_radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                        if ui.add(egui::Slider::new(&mut base_r, 0.0..=2.0).text("Base Radius")).changed() {
                                            m.params.insert("base_radius".into(), base_r.into());
                                            needs_save = true;
                                        }

                                        let mut max_r = m.params.get("max_radius").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
                                        if ui.add(egui::Slider::new(&mut max_r, 0.0..=5.0).text("Max Radius")).changed() {
                                            m.params.insert("max_radius".into(), max_r.into());
                                            needs_save = true;
                                        }

                                        let mut sens = m.params.get("sensitivity").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
                                        if ui.add(egui::Slider::new(&mut sens, 0.0..=1.0).text("Sensitivity")).changed() {
                                            m.params.insert("sensitivity".into(), sens.into());
                                            needs_save = true;
                                        }

                                        let mut decay = m.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(0.05) as f32;
                                        if ui.add(egui::Slider::new(&mut decay, 0.001..=0.5).text("Decay Speed")).changed() {
                                            m.params.insert("decay".into(), decay.into());
                                            needs_save = true;
                                        }
                                    } else if m.mask_type == "orbit" {
                                        // Hard Edge
                                        let mut hard_edge = m.params.get("hard_edge").and_then(|v| v.as_bool()).unwrap_or(false);
                                        if ui.checkbox(&mut hard_edge, "Hard Edge").changed() {
                                            m.params.insert("hard_edge".into(), hard_edge.into());
                                            needs_save = true;
                                        }

                                        // Constant Speed
                                        let mut constant_speed = m.params.get("constant_speed").and_then(|v| v.as_bool()).unwrap_or(false);
                                        if ui.checkbox(&mut constant_speed, "Constant Speed").on_hover_text("When enabled, bar moves at the same speed on all sides. Shorter sides finish early and pause until the next beat.").changed() {
                                            m.params.insert("constant_speed".into(), constant_speed.into());
                                            needs_save = true;
                                        }

                                        // Bar Width
                                        let mut bw = m.params.get("bar_width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                        if ui.add(egui::Slider::new(&mut bw, 0.01..=4.0).text("Bar Width")).changed() {
                                            m.params.insert("bar_width".into(), bw.into());
                                            needs_save = true;
                                        }
                                    }

                                    // Color
                                    ui.horizontal(|ui| {
                                        ui.label("Color:");
                                        let mut rgb = m.params.get("color").and_then(|v| {
                                            serde_json::from_value::<Vec<u8>>(serde_json::json!(v)).ok()
                                        }).unwrap_or(vec![255, 0, 0]);
                                        let mut rgb_arr = [rgb[0], rgb[1], rgb[2]];
                                        if color_picker(ui, &mut rgb_arr, format!("msk_main_{}", m.id)) {
                                            m.params.insert("color".into(), serde_json::json!(rgb_arr));
                                            needs_save = true;
                                        }
                                    });
                                    
                                    // Color Mode
                                    ui.horizontal(|ui| {
                                        ui.label("Gradient:");
                                        let mut mode = m.params.get("color_mode").and_then(|v| v.as_str()).unwrap_or("static").to_string();
                                        // Auto-migrate "rainbow" or "pulse" to "gradient" or "static" if needed? 
                                        // For now just offer Static/Gradient.
                                        egui::ComboBox::from_id_source(m.id)
                                            .selected_text(if mode == "gradient" { "Gradient" } else { "Static" })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(&mut mode, "static".into(), "Static");
                                                ui.selectable_value(&mut mode, "gradient".into(), "Gradient");
                                            });
                                        
                                        if mode != m.params.get("color_mode").and_then(|v| v.as_str()).unwrap_or("static") {
                                            m.params.insert("color_mode".into(), serde_json::json!(mode));
                                            needs_save = true;
                                        }
                                    });

                                    // Multi-Color Gradient Colors
                                    let mode_ref = m.params.get("color_mode").and_then(|v| v.as_str()).unwrap_or("static");
                                    if mode_ref == "gradient" {
                                        ui.label("Gradient Colors:");
                                        
                                        // Load colors or init defaults
                                        let mut colors: Vec<[u8; 3]> = m.params.get("gradient_colors").and_then(|v| {
                                            serde_json::from_value(v.clone()).ok()
                                        }).unwrap_or_else(|| {
                                            // Fallback to [color, color2] if exists, else defaults
                                            let c1 = m.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([0, 255, 255]);
                                            let c2 = m.params.get("color2").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255, 0, 255]);
                                            vec![c1, c2]
                                        });

                                        let mut changed = false;
                                        ui.horizontal_wrapped(|ui| {
                                            for (_i, rgb) in colors.iter_mut().enumerate() {
                                                // use array directly
                                                if color_picker(ui, rgb, format!("msk_grad_{}_{}", m.id, _i)) {
                                                    changed = true;
                                                }
                                                // Remove button (small x)
                                                if ui.small_button("x").clicked() {
                                                    // Mark for deletion? tricky in iterator. 
                                                    // Re-render limitation here.
                                                    // Let's do a separate loop or indexed loop.
                                                    // Handled by below logic: "remove at index i"
                                                    // Actually, immediate mode means we can't mutate vector while iterating easily if removing.
                                                    // We'll trust the user to not click too fast or handle it next frame?
                                                    // Better: Collect indices to remove.
                                                }
                                            }
                                            if ui.button("+").clicked() {
                                                colors.push([255, 255, 255]);
                                                changed = true;
                                            }
                                        });
                                        
                                        // Since we can't remove easily inside the iter_mut loop above due to borrow rules,
                                        // let's do a robust simple list:
                                        
                                        let mut remove_idx = None;
                                        ui.horizontal(|ui| {
                                           for i in 0..colors.len() {
                                               ui.push_id(format!("gcol_{}_{}", m.id, i), |ui| {
                                                    if color_picker(ui, &mut colors[i], "picker") {
                                                        changed = true;
                                                    }
                                                    if colors.len() > 1 && ui.small_button("-").clicked() {
                                                        remove_idx = Some(i);
                                                    }
                                               });
                                           } 
                                        });
                                        
                                        if let Some(idx) = remove_idx {
                                            colors.remove(idx);
                                            changed = true;
                                        }

                                        if changed {
                                            m.params.insert("gradient_colors".into(), serde_json::json!(colors));
                                            // Also update main "color" param to be the first one for compatibility/thumbnails?
                                            if let Some(first) = colors.first() {
                                                 m.params.insert("color".into(), serde_json::json!(first));
                                            }
                                            needs_save = true;
                                        }
                                    }
                                    
                                    // Speed / Sync
                                    ui.horizontal(|ui| {
                                        if m.mask_type == "scanner" {
                                            ui.vertical(|ui| {
                                                let mut is_sync = m.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(false);
                                                ui.horizontal(|ui| {
                                                    if ui.checkbox(&mut is_sync, "Syn").changed() {
                                                        m.params.insert("sync".into(), is_sync.into());
                                                        needs_save = true;
                                                    }
                                                    let mut uni = m.params.get("unidirectional").and_then(|v| v.as_bool()).unwrap_or(false);
                                                    if ui.checkbox(&mut uni, "Uni").changed() {
                                                        m.params.insert("unidirectional".into(), uni.into());
                                                        needs_save = true;
                                                    }
                                                    
                                                    // Motion Easing
                                                    let mut motion = m.params.get("motion").and_then(|v| v.as_str()).unwrap_or("Smooth").to_string();
                                                    egui::ComboBox::from_id_source(format!("mot_{}", m.id))
                                                        .selected_text(motion.clone())
                                                        .show_ui(ui, |ui| {
                                                            ui.selectable_value(&mut motion, "Smooth".into(), "Smooth");
                                                            ui.selectable_value(&mut motion, "Linear".into(), "Linear");
                                                        });
                                                    if motion != m.params.get("motion").and_then(|v| v.as_str()).unwrap_or("Smooth") {
                                                        m.params.insert("motion".into(), serde_json::json!(motion));
                                                        needs_save = true;
                                                    }
                                                });

                                                if is_sync {
                                                    ui.horizontal(|ui| {
                                                        ui.label("Rate:");
                                                        let mut rate = m.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4").to_string();
                                                        egui::ComboBox::from_id_source(format!("rate_{}", m.id))
                                                            .selected_text(rate.clone())
                                                            .show_ui(ui, |ui| {
                                                                ui.selectable_value(&mut rate, "4 Bar".into(), "4 Bar");
                                                                ui.selectable_value(&mut rate, "1 Bar".into(), "1 Bar");
                                                                ui.selectable_value(&mut rate, "1/2".into(), "1/2");
                                                                ui.selectable_value(&mut rate, "1/4".into(), "1/4");
                                                                ui.selectable_value(&mut rate, "1/8".into(), "1/8");
                                                            });
                                                        if rate != m.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4") {
                                                            m.params.insert("rate".into(), serde_json::json!(rate));
                                                            needs_save = true;
                                                        }
                                                    });
                                                    
                                                    ui.horizontal(|ui| {
                                                        ui.label("Start Pos:");
                                                        let mut start_pos = m.params.get("start_pos").and_then(|v| v.as_str()).unwrap_or("Center").to_string();
                                                        egui::ComboBox::from_id_source(format!("start_{}", m.id))
                                                            .selected_text(start_pos.clone())
                                                            .show_ui(ui, |ui| {
                                                                ui.selectable_value(&mut start_pos, "Center".into(), "Center");
                                                                ui.selectable_value(&mut start_pos, "Left".into(), "Left");
                                                                ui.selectable_value(&mut start_pos, "Right".into(), "Right");
                                                            });
                                                        if start_pos != m.params.get("start_pos").and_then(|v| v.as_str()).unwrap_or("Center") {
                                                            m.params.insert("start_pos".into(), serde_json::json!(start_pos));
                                                            needs_save = true;
                                                        }
                                                    });
                                                    
                                                        ui.horizontal(|ui| {
                                                            ui.label("Bar Width:");
                                                            let mut width = m.params.get("bar_width").and_then(|v| v.as_f64()).unwrap_or(0.1);
                                                            if ui.add(egui::Slider::new(&mut width, 0.01..=4.0).text("Width")).changed() {
                                                                m.params.insert("bar_width".into(), width.into());
                                                                needs_save = true;
                                                            }
                                                        });
                                                        if lfo_controls(ui, &mut m.params, "bar_width", format!("barwidth_lfo_{}", m.id)) {
                                                            needs_save = true;
                                                        }
                                                } else {
                                                    let mut speed = m.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0);
                                                    if ui.add(egui::Slider::new(&mut speed, 0.1..=5.0).text("Speed")).changed() {
                                                        m.params.insert("speed".into(), speed.into());
                                                        needs_save = true;
                                                    }
                                                }
                                            });
                                        } else if m.mask_type == "orbit" {
                                            ui.vertical(|ui| {
                                                let mut is_sync = m.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(false);
                                                if ui.checkbox(&mut is_sync, "Beat Sync").changed() {
                                                    m.params.insert("sync".into(), is_sync.into());
                                                    needs_save = true;
                                                }

                                                if is_sync {
                                                    ui.horizontal(|ui| {
                                                        ui.label("Rate:");
                                                        let mut rate = m.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4").to_string();
                                                        egui::ComboBox::from_id_source(format!("orbit_rate_{}", m.id))
                                                            .selected_text(rate.clone())
                                                            .show_ui(ui, |ui| {
                                                                ui.selectable_value(&mut rate, "4 Bar".into(), "4 Bar");
                                                                ui.selectable_value(&mut rate, "2 Bar".into(), "2 Bar");
                                                                ui.selectable_value(&mut rate, "1 Bar".into(), "1 Bar");
                                                                ui.selectable_value(&mut rate, "1/2".into(), "1/2");
                                                                ui.selectable_value(&mut rate, "1/4".into(), "1/4");
                                                                ui.selectable_value(&mut rate, "1/8".into(), "1/8");
                                                            });
                                                        if rate != m.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4") {
                                                            m.params.insert("rate".into(), serde_json::json!(rate));
                                                            needs_save = true;
                                                        }
                                                    });
                                                } else {
                                                    let mut speed = m.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0);
                                                    if ui.add(egui::Slider::new(&mut speed, 0.1..=5.0).text("Speed")).changed() {
                                                        m.params.insert("speed".into(), speed.into());
                                                        needs_save = true;
                                                    }
                                                }
                                            });
                                        }
                                        });
                                    // Close collapsing and push_id blocks, then the for-loop
                                    });
                                });
                            }
                            if let Some(idx) = delete_mask_idx {
                                scene.masks.remove(idx);
                                needs_save = true;
                            }
                        }
                        } // End of !is_being_dragged
                        } // End of push_id
                            });
                        }
                        if let Some(i) = duplicate_scene_idx {
                            let mut new_s = self.state.scenes[i].clone();
                            new_s.id = rand::random();
                            new_s.name = format!("{} Copy", new_s.name);
                            new_s.launchpad_btn = None;
                            self.state.scenes.push(new_s);
                            self.mark_state_changed();
                        }
                        
                        if let Some((from, to)) = swap_request {
                            self.state.scenes.swap(from, to);
                            self.mark_state_changed();
                        }

                        if let Some(i) = delete_scene_idx {
                            // Clear MIDI button before deleting scene
                            if let Some(scene) = self.state.scenes.get(i) {
                                if let Some(btn) = scene.launchpad_btn {
                                    if btn > 0 {
                                        let cmd = if scene.launchpad_is_cc {
                                            midi::MidiCommand::SetButtonColor { cc: btn, color: 0 }
                                        } else {
                                            midi::MidiCommand::SetPadColor { note: btn, color: 0 }
                                        };
                                        let _ = self.midi_sender.send(cmd);
                                    }
                                }
                            }
                            self.state.scenes.remove(i);
                            self.mark_state_changed();
                        }

                        if needs_save {
                            self.mark_state_changed();
                        }
                        
                        // Render Floating Scene
                        if let Some(scene) = floating_scene {
                             if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                                 egui::Area::new("dragged_scene")
                                     .fixed_pos(pointer_pos + egui::vec2(10.0, 10.0)) // Offset slightly
                                     .order(egui::Order::Tooltip)
                                     .show(ui.ctx(), |ui| {
                                         egui::Frame::popup(ui.style()).show(ui, |ui| {
                                             ui.label(egui::RichText::new(format!(":: {}", scene.name)).strong());
                                         });
                                     });
                             }
                        }
                        
                        // Clear drag state if mouse released
                        if ui.input(|i| i.pointer.any_released()) {
                            self.dragged_scene_id = None;
                        }

                    });
                });

                // RIGHT PANEL: CANVAS
                let canvas_ui = &mut columns[1];
                
                canvas_ui.horizontal(|ui| {
                    ui.checkbox(&mut self.state.layout_locked, "🔒 Lock Layout");
                });

                let (response, painter) = canvas_ui.allocate_painter(
                    canvas_ui.available_size(), 
                    egui::Sense::click_and_drag()
                );
                
                let rect = response.rect;
                
                // AUTO-FIT ON LOAD
                if self.is_first_frame {
                    self.is_first_frame = false;
                    
                    let mut min_x: f32 = 1.0;
                    let mut min_y: f32 = 1.0;
                    let mut max_x: f32 = 0.0;
                    let mut max_y: f32 = 0.0;
                    let mut found = false;
                    
                    for s in &self.state.strips {
                        // Start point
                        min_x = min_x.min(s.x);
                        min_y = min_y.min(s.y);
                        max_x = max_x.max(s.x);
                        max_y = max_y.max(s.y);

                        // End point
                        if s.pixel_count > 1 {
                            let len = (s.pixel_count - 1) as f32 * s.spacing;
                            // Strip always extends to Right
                            let tail_x = s.x + len;
                            let tail_y = s.y;
                            min_x = min_x.min(tail_x);
                            min_y = min_y.min(tail_y);
                            max_x = max_x.max(tail_x);
                            max_y = max_y.max(tail_y);
                        }
                        found = true;
                    }


                    
                    if found {
                        // Pad slightly
                        min_x -= 0.1;
                        min_y -= 0.1;
                        max_x += 0.1;
                        max_y += 0.1;
                        
                        let w = max_x - min_x;
                        let h = max_y - min_y;
                        
                        // Fit w/h into 1.0/1.0 (since normalized coords 0..1 are standard)
                        // Scale = Pixels / NormUnit
                        // Available: rect.width(), rect.height()
                        
                        let scale_x = 1.0 / w;
                        let scale_y = 1.0 / h;
                        let fit_scale = scale_x.min(scale_y) * 0.9; 
                        
                        self.view.scale = fit_scale.clamp(0.1, 100.0);
                        
                        // Center Logic
                        let cx = (min_x + max_x) / 2.0;
                        let cy = (min_y + max_y) / 2.0;

                        let w_px = rect.width();
                        let h_px = rect.height();
                        
                        self.view.offset.x = -(cx - 0.5) * w_px * self.view.scale;
                        self.view.offset.y = -(cy - 0.5) * h_px * self.view.scale;
                    }
                }
                
                // HELPER CLOSURES (Moved up for scope visibility)
                let to_screen = |x: f32, y: f32, view: &ViewState| -> egui::Pos2 {
                    egui::pos2(
                        rect.center().x + (x - 0.5) * rect.width() * view.scale + view.offset.x,
                        rect.center().y + (y - 0.5) * rect.height() * view.scale + view.offset.y
                    )
                };
                
                let from_screen = |pos: egui::Pos2, view: &ViewState| -> (f32, f32) {
                     let dx = pos.x - (rect.center().x + view.offset.x);
                     let dy = pos.y - (rect.center().y + view.offset.y);
                     let x = (dx / (rect.width() * view.scale)) + 0.5;
                     let y = (dy / (rect.height() * view.scale)) + 0.5;
                     (x, y)
                };

                // INPUT TRANSFORMS (Keep existing input logic)
                let input = ctx.input(|i| i.clone());
                // Determine which masks are active for viewing/editing on canvas
                let active_masks: Vec<model::Mask> = if let Some(sel) = self.state.selected_scene_id {
                    if let Some(scene) = self.state.scenes.iter().find(|s| s.id == sel) {
                        if scene.kind == "Masks" { scene.masks.clone() } else { self.state.masks.clone() }
                    } else { self.state.masks.clone() }
                } else { self.state.masks.clone() };
                
                if response.hovered() {
                    let mut zoom_factor = 1.0;
                    let pinch_delta = input.zoom_delta();
                    if pinch_delta != 1.0 { zoom_factor *= pinch_delta; }
                    let scroll_y = input.scroll_delta.y;
                    if scroll_y != 0.0 { zoom_factor *= (scroll_y * 0.002).exp(); }
                    
                    if zoom_factor != 1.0 {
                        if let Some(mouse_pos) = response.hover_pos() {
                            let (wx, wy) = from_screen(mouse_pos, &self.view);
                            let new_scale = (self.view.scale * zoom_factor).clamp(0.01, 100.0);
                            self.view.scale = new_scale;
                             self.view.offset.x = mouse_pos.x - rect.center().x - (wx - 0.5) * rect.width() * self.view.scale;
                             self.view.offset.y = mouse_pos.y - rect.center().y - (wy - 0.5) * rect.height() * self.view.scale;
                        }
                    }

                    // HOVER CURSOR LOGIC
                    if let Some(pos) = response.hover_pos() {
                       // Use Screen Pixels directly!
                       for m in &active_masks {
                           let handle_size = 15.0; // Pixels
                           
                           match m.mask_type.as_str() {
                               "scanner" => {
                                   let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                   let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                   let rot_deg = m.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                                   let rot = rot_deg.to_radians();
                                   let cos_r = rot.cos();
                                   let sin_r = rot.sin();
                                   
                                   // Center in Screen Matrix
                                   let center_scr = to_screen(m.x, m.y, &self.view);
                                   let dx_scr = pos.x - center_scr.x;
                                   let dy_scr = pos.y - center_scr.y;
                                   
                                   // Rotate into Local Space (Screen Pixels)
                                   let lx_scr = dx_scr * cos_r + dy_scr * sin_r;
                                   let ly_scr = -dx_scr * sin_r + dy_scr * cos_r;
                                   
                                   // Dimensions in Screen Pixels
                                   let w_scr = w * rect.width() * self.view.scale;
                                   let h_scr = h * rect.height() * self.view.scale;
                                   let hw_scr = w_scr / 2.0;
                                   let hh_scr = h_scr / 2.0;

                                   let in_y = ly_scr >= -hh_scr - handle_size && ly_scr <= hh_scr + handle_size;
                                   let in_x = lx_scr >= -hw_scr - handle_size && lx_scr <= hw_scr + handle_size;

                                   let set_icon = |normal_ang: f32| {
                                        let mut a = normal_ang.rem_euclid(std::f32::consts::PI);
                                        if a > std::f32::consts::PI { a -= std::f32::consts::PI; }
                                        let icon = if (a - 0.0).abs() < 0.3 || (a - std::f32::consts::PI).abs() < 0.3 {
                                             egui::CursorIcon::ResizeHorizontal
                                        } else if (a - std::f32::consts::PI/2.0).abs() < 0.3 {
                                             egui::CursorIcon::ResizeVertical
                                        } else if (a - std::f32::consts::PI/4.0).abs() < 0.3 {
                                             egui::CursorIcon::ResizeNeSw
                                        } else {
                                             egui::CursorIcon::ResizeNwSe
                                        };
                                        canvas_ui.output_mut(|o| o.cursor_icon = icon);
                                   };

                                   if in_x && (ly_scr - (-hh_scr)).abs() < handle_size {
                                       set_icon(rot - std::f32::consts::FRAC_PI_2);
                                       break;
                                   }
                                   if in_y && (lx_scr - hw_scr).abs() < handle_size {
                                       set_icon(rot);
                                       break;
                                   }
                                   if in_x && (ly_scr - hh_scr).abs() < handle_size {
                                       set_icon(rot + std::f32::consts::FRAC_PI_2);
                                       break;
                                   }
                                   if in_y && (lx_scr - (-hw_scr)).abs() < handle_size {
                                       set_icon(rot + std::f32::consts::PI);
                                       break;
                                   }
                               },
                               "radial" => {
                                   let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                   let center_scr = to_screen(m.x, m.y, &self.view);
                                   let dx_scr = pos.x - center_scr.x;
                                   let dy_scr = pos.y - center_scr.y;
                                   // Note: Radius param is normalized to Width?
                                   // Logic in draw: let radius_screen = r * rect.width() * self.view.scale;
                                   let radius_scr = r * rect.width() * self.view.scale;

                                   let dist_scr = (dx_scr.powi(2) + dy_scr.powi(2)).sqrt();

                                   if (dist_scr - radius_scr).abs() < handle_size {
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeNwSe);
                                       break;
                                   }
                               },
                               "orbit" => {
                                   let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                   let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;

                                   let center_scr = to_screen(m.x, m.y, &self.view);
                                   let dx_scr = pos.x - center_scr.x;
                                   let dy_scr = pos.y - center_scr.y;

                                   let w_scr = w * rect.width() * self.view.scale;
                                   let h_scr = h * rect.height() * self.view.scale;
                                   let hw_scr = w_scr / 2.0;
                                   let hh_scr = h_scr / 2.0;

                                   let in_y = dy_scr >= -hh_scr - handle_size && dy_scr <= hh_scr + handle_size;
                                   let in_x = dx_scr >= -hw_scr - handle_size && dx_scr <= hw_scr + handle_size;

                                   // Show cursor hints for resize handles
                                   if in_x && (dy_scr - (-hh_scr)).abs() < handle_size {
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeVertical);
                                       break;
                                   }
                                   if in_y && (dx_scr - hw_scr).abs() < handle_size {
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
                                       break;
                                   }
                                   if in_x && (dy_scr - hh_scr).abs() < handle_size {
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeVertical);
                                       break;
                                   }
                                   if in_y && (dx_scr - (-hw_scr)).abs() < handle_size {
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
                                       break;
                                   }
                               },
                               _ => {}
                           }
                       }
                    }
                }

                if response.clicked() || response.drag_started() {
                   if let Some(pos) = response.interact_pointer_pos() {
                       let (wx, wy) = from_screen(pos, &self.view);
                       let mut hit = false;
                       
                       // 1. HIT TEST RESIZE HANDLES (Priority over Move)
                       // Only check masks for resizing for now
                       for m in &active_masks {
                           let handle_size = 15.0; // Pixels
                           
                           match m.mask_type.as_str() {
                               "scanner" => {
                                   let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                   let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                   let rot_deg = m.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                                   let rot = rot_deg.to_radians();
                                   let cos_r = rot.cos();
                                   let sin_r = rot.sin();
                                   
                                   let center_scr = to_screen(m.x, m.y, &self.view);
                                   let dx_scr = pos.x - center_scr.x;
                                   let dy_scr = pos.y - center_scr.y;
                                   
                                   let lx_scr = dx_scr * cos_r + dy_scr * sin_r;
                                   let ly_scr = -dx_scr * sin_r + dy_scr * cos_r;
                                   
                                   let w_scr = w * rect.width() * self.view.scale;
                                   let h_scr = h * rect.height() * self.view.scale;
                                   let hw_scr = w_scr / 2.0;
                                   let hh_scr = h_scr / 2.0;
                                   
                                   let in_y = ly_scr >= -hh_scr - handle_size && ly_scr <= hh_scr + handle_size;
                                   let in_x = lx_scr >= -hw_scr - handle_size && lx_scr <= hw_scr + handle_size;
                                   
                                   
                                   let mut set_cursor = |edge: usize, normal_ang: f32| {
                                        self.view.drag_id = Some(m.id);
                                        self.view.drag_type = DragType::ResizeMask(edge);
                                        hit = true;
                                        
                                        // Pick Cursor based on Normal Angle (screen space)
                                        let mut a = normal_ang.rem_euclid(std::f32::consts::PI);
                                        if a > std::f32::consts::PI { a -= std::f32::consts::PI; }
                                        let icon = if (a - 0.0).abs() < 0.3 || (a - std::f32::consts::PI).abs() < 0.3 {
                                             egui::CursorIcon::ResizeHorizontal
                                        } else if (a - std::f32::consts::PI/2.0).abs() < 0.3 {
                                             egui::CursorIcon::ResizeVertical
                                        } else if (a - std::f32::consts::PI/4.0).abs() < 0.3 {
                                             egui::CursorIcon::ResizeNeSw
                                        } else {
                                             egui::CursorIcon::ResizeNwSe
                                        };
                                        canvas_ui.output_mut(|o| o.cursor_icon = icon);
                                   };
 
                                   if in_x && (ly_scr - (-hh_scr)).abs() < handle_size {
                                       set_cursor(0, rot - std::f32::consts::FRAC_PI_2);
                                       break;
                                   }
                                   if in_y && (lx_scr - hw_scr).abs() < handle_size {
                                       set_cursor(1, rot);
                                       break;
                                   }
                                   if in_x && (ly_scr - hh_scr).abs() < handle_size {
                                       set_cursor(2, rot + std::f32::consts::FRAC_PI_2);
                                       break;
                                   }
                                   if in_y && (lx_scr - (-hw_scr)).abs() < handle_size {
                                       set_cursor(3, rot + std::f32::consts::PI);
                                       break;
                                   }
                               },
                               "radial" => {
                                   let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                   let center_scr = to_screen(m.x, m.y, &self.view);
                                   let dx_scr = pos.x - center_scr.x;
                                   let dy_scr = pos.y - center_scr.y;
                                   let radius_scr = r * rect.width() * self.view.scale;

                                   let dist_scr = (dx_scr.powi(2) + dy_scr.powi(2)).sqrt();

                                   if (dist_scr - radius_scr).abs() < handle_size {
                                       self.view.drag_id = Some(m.id);
                                       self.view.drag_type = DragType::ResizeMask(1); // Treat as "Right" for logic
                                       hit = true;
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeNwSe);
                                       break;
                                   }
                               },
                               "orbit" => {
                                   let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                   let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;

                                   let center_scr = to_screen(m.x, m.y, &self.view);
                                   let dx_scr = pos.x - center_scr.x;
                                   let dy_scr = pos.y - center_scr.y;

                                   let w_scr = w * rect.width() * self.view.scale;
                                   let h_scr = h * rect.height() * self.view.scale;
                                   let hw_scr = w_scr / 2.0;
                                   let hh_scr = h_scr / 2.0;

                                   let in_y = dy_scr >= -hh_scr - handle_size && dy_scr <= hh_scr + handle_size;
                                   let in_x = dx_scr >= -hw_scr - handle_size && dx_scr <= hw_scr + handle_size;

                                   // Check edges for resize handles
                                   if in_x && (dy_scr - (-hh_scr)).abs() < handle_size {
                                       self.view.drag_id = Some(m.id);
                                       self.view.drag_type = DragType::ResizeMask(0); // Top
                                       hit = true;
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeVertical);
                                       break;
                                   }
                                   if in_y && (dx_scr - hw_scr).abs() < handle_size {
                                       self.view.drag_id = Some(m.id);
                                       self.view.drag_type = DragType::ResizeMask(1); // Right
                                       hit = true;
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
                                       break;
                                   }
                                   if in_x && (dy_scr - hh_scr).abs() < handle_size {
                                       self.view.drag_id = Some(m.id);
                                       self.view.drag_type = DragType::ResizeMask(2); // Bottom
                                       hit = true;
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeVertical);
                                       break;
                                   }
                                   if in_y && (dx_scr - (-hw_scr)).abs() < handle_size {
                                       self.view.drag_id = Some(m.id);
                                       self.view.drag_type = DragType::ResizeMask(3); // Left
                                       hit = true;
                                       canvas_ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
                                       break;
                                   }
                               },
                               _ => {}
                           }
                       }


                       // 2. HIT TEST MOVE (Masks) - With proper rotation support
                       if !hit {
                           for m in &active_masks {
                               match m.mask_type.as_str() {
                                   "scanner" => {
                                       let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                       let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                       let rot_deg = m.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                                       let rot = rot_deg.to_radians();
                                       let cos_r = rot.cos();
                                       let sin_r = rot.sin();

                                       // Transform click point to mask local space (same as scanner collision)
                                       let dx = wx - m.x;
                                       let dy = wy - m.y;
                                       let local_x = dx * cos_r + dy * sin_r;
                                       let local_y = -dx * sin_r + dy * cos_r;

                                       let half_w = w / 2.0;
                                       let half_h = h / 2.0;

                                       // Check if click is inside rotated rectangle
                                       if local_x >= -half_w && local_x <= half_w &&
                                          local_y >= -half_h && local_y <= half_h {
                                           self.view.drag_id = Some(m.id);
                                           self.view.drag_type = DragType::Mask;
                                           hit = true;
                                           break;
                                       }
                                   },
                                   "radial" => {
                                       let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                       let dist = ((wx - m.x).powi(2) + (wy - m.y).powi(2)).sqrt();
                                       if dist < r {
                                           self.view.drag_id = Some(m.id);
                                           self.view.drag_type = DragType::Mask;
                                           hit = true;
                                           break;
                                       }
                                   },
                                   "orbit" => {
                                       let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                       let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;

                                       let dx = wx - m.x;
                                       let dy = wy - m.y;

                                       let half_w = w / 2.0;
                                       let half_h = h / 2.0;

                                       if dx >= -half_w && dx <= half_w && dy >= -half_h && dy <= half_h {
                                           self.view.drag_id = Some(m.id);
                                           self.view.drag_type = DragType::Mask;
                                           hit = true;
                                           break;
                                       }
                                   },
                                   _ => {}
                               }
                           }
                       }

                       // 3. HIT TEST STRIPS
                       if !hit && !self.state.layout_locked {
                           for s in &self.state.strips {
                               let dist = ((wx - s.x).powi(2) + (wy - s.y).powi(2)).sqrt();
                               let pixel_size_x = 15.0 / (rect.width() * self.view.scale);
                               if dist < pixel_size_x {
                                   self.view.drag_id = Some(s.id);
                                   self.view.drag_type = DragType::Strip;
                                   hit = true;
                                   break;
                               }
                           }
                       }
                       
                       if !hit {
                           self.view.drag_id = None; 
                           self.view.drag_type = DragType::None;
                       }
                   }
                }
                
                // Calculate bounds and grid cell sizes for snapping
                let mut snap_b_min_x: f32 = if self.state.strips.is_empty() { 0.0 } else { f32::MAX };
                let mut snap_b_min_y: f32 = if self.state.strips.is_empty() { 0.0 } else { f32::MAX };
                let mut snap_b_max_x: f32 = if self.state.strips.is_empty() { 1.0 } else { f32::MIN };
                let mut snap_b_max_y: f32 = if self.state.strips.is_empty() { 1.0 } else { f32::MIN };
                for s in &self.state.strips {
                    snap_b_min_x = snap_b_min_x.min(s.x);
                    snap_b_min_y = snap_b_min_y.min(s.y);
                    snap_b_max_x = snap_b_max_x.max(s.x);
                    snap_b_max_y = snap_b_max_y.max(s.y);
                    if s.pixel_count > 1 {
                        let tail_x = s.x + (s.pixel_count - 1) as f32 * s.spacing;
                        snap_b_min_x = snap_b_min_x.min(tail_x);
                        snap_b_max_x = snap_b_max_x.max(tail_x);
                    }
                }
                let snap_bounds_width = snap_b_max_x - snap_b_min_x;
                let snap_bounds_height = snap_b_max_y - snap_b_min_y;

                // Calculate grid cell sizes (same logic as rendering)
                let (cell_size_x, cell_size_y) = if !self.state.strips.is_empty() && snap_bounds_width > 0.0 {
                    let target_min_pixels = 25.0;
                    let target_max_pixels = 80.0;

                    let grid_unit_x = snap_bounds_width;
                    let unit_pixels_x = grid_unit_x * rect.width() * self.view.scale;
                    let mut subdivisions_x = 1;
                    while unit_pixels_x / (subdivisions_x as f32) > target_max_pixels && subdivisions_x < 128 {
                        subdivisions_x *= 2;
                    }
                    while unit_pixels_x / (subdivisions_x as f32) < target_min_pixels && subdivisions_x > 1 {
                        subdivisions_x /= 2;
                    }

                    let grid_unit_y = if snap_bounds_height > 0.001 { snap_bounds_height } else { snap_bounds_width };
                    let unit_pixels_y = grid_unit_y * rect.height() * self.view.scale;
                    let mut subdivisions_y = 1;
                    while unit_pixels_y / (subdivisions_y as f32) > target_max_pixels && subdivisions_y < 128 {
                        subdivisions_y *= 2;
                    }
                    while unit_pixels_y / (subdivisions_y as f32) < target_min_pixels && subdivisions_y > 1 {
                        subdivisions_y /= 2;
                    }

                    (grid_unit_x / subdivisions_x as f32, grid_unit_y / subdivisions_y as f32)
                } else {
                    (0.1, 0.1) // Default if no strips
                };

                // Snap helper functions
                let snap_to_grid_x = |x: f32| -> f32 {
                    let rel = x - snap_b_min_x;
                    let snapped_rel = (rel / cell_size_x).round() * cell_size_x;
                    snap_b_min_x + snapped_rel
                };
                let snap_to_grid_y = |y: f32| -> f32 {
                    let rel = y - snap_b_min_y;
                    let snapped_rel = (rel / cell_size_y).round() * cell_size_y;
                    snap_b_min_y + snapped_rel
                };

                // Check if shift is held (disables snapping)
                let shift_held = input.modifiers.shift;

                if response.dragged() {
                    let delta = response.drag_delta(); // screen pixels

                    if self.view.drag_id.is_some() {
                         if self.view.drag_type == DragType::Strip {
                             // Keep Strip logic simple (normalized)
                             // Convert delta to normalized
                             let dx = delta.x / (rect.width() * self.view.scale);
                             let dy = delta.y / (rect.height() * self.view.scale);
                             if let Some(s) = self.state.strips.iter_mut().find(|s| Some(s.id) == self.view.drag_id) {
                                  s.x += dx;
                                  s.y += dy;
                             }
                         } else if self.view.drag_type == DragType::Mask {
                             // Move mask (snapping happens on release)
                             let dx = delta.x / (rect.width() * self.view.scale);
                             let dy = delta.y / (rect.height() * self.view.scale);
                             if let Some(sel) = self.state.selected_scene_id {
                                 if let Some(scene_index) = self.state.scenes.iter().position(|s| s.id == sel && s.kind == "Masks") {
                                     if let Some(m) = self.state.scenes[scene_index].masks.iter_mut().find(|m| Some(m.id) == self.view.drag_id) {
                                         m.x += dx; m.y += dy;
                                     }
                                 } else if let Some(m) = self.state.masks.iter_mut().find(|m| Some(m.id) == self.view.drag_id) {
                                     m.x += dx; m.y += dy;
                                 }
                             } else if let Some(m) = self.state.masks.iter_mut().find(|m| Some(m.id) == self.view.drag_id) {
                                 m.x += dx; m.y += dy;
                             }
                         } else if let DragType::ResizeMask(edge_idx) = self.view.drag_type {
                              // Fetch target mask mutably depending on scene selection
                              // We'll duplicate the resize logic for whichever collection contains the mask
                              // Scene masks first
                              if let Some(sel) = self.state.selected_scene_id {
                                  if let Some(scene_index) = self.state.scenes.iter().position(|s| s.id == sel && s.kind == "Masks") {
                                      if let Some(m) = self.state.scenes[scene_index].masks.iter_mut().find(|m| Some(m.id) == self.view.drag_id) {
                                          match m.mask_type.as_str() {
                                              "scanner" => {
                                                  let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                                  let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                                  let rot_deg = m.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                                                  let rot = rot_deg.to_radians();
                                                  let cos_r = rot.cos();
                                                  let sin_r = rot.sin();
                                                  let ldx_scr = delta.x * cos_r + delta.y * sin_r;
                                                  let ldy_scr = -delta.x * sin_r + delta.y * cos_r;
                                                  let w_scr = w * rect.width() * self.view.scale;
                                                  let h_scr = h * rect.height() * self.view.scale;
                                                  let mut new_w_scr = w_scr;
                                                  let mut new_h_scr = h_scr;
                                                  let mut shift_lx_scr = 0.0;
                                                  let mut shift_ly_scr = 0.0;
                                                  match edge_idx { 0 => { new_h_scr = (h_scr - ldy_scr).max(1.0); shift_ly_scr = ldy_scr / 2.0; },
                                                                   1 => { new_w_scr = (w_scr + ldx_scr).max(1.0); shift_lx_scr = ldx_scr / 2.0; },
                                                                   2 => { new_h_scr = (h_scr + ldy_scr).max(1.0); shift_ly_scr = ldy_scr / 2.0; },
                                                                   3 => { new_w_scr = (w_scr - ldx_scr).max(1.0); shift_lx_scr = ldx_scr / 2.0; },
                                                                   _ => {} }
                                                  let new_w = new_w_scr / (rect.width() * self.view.scale);
                                                  let new_h = new_h_scr / (rect.height() * self.view.scale);
                                                  let wx_shift_scr = shift_lx_scr * cos_r - shift_ly_scr * sin_r;
                                                  let wy_shift_scr = shift_lx_scr * sin_r + shift_ly_scr * cos_r;
                                                  let wx_shift_norm = wx_shift_scr / (rect.width() * self.view.scale);
                                                  let wy_shift_norm = wy_shift_scr / (rect.height() * self.view.scale);
                                                  m.x += wx_shift_norm; m.y += wy_shift_norm;
                                                  m.params.insert("width".to_string(), new_w.max(0.01).into());
                                                  m.params.insert("height".to_string(), new_h.max(0.01).into());
                                              },
                                              "radial" => {
                                                  let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                                  let dr_scr = delta.x;
                                                  let dr_norm = dr_scr / (rect.width() * self.view.scale);
                                                  m.params.insert("radius".to_string(), (r + dr_norm).max(0.01).into());
                                              },
                                              "orbit" => {
                                                  // Orbit has no rotation, simpler resize logic
                                                  let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                                  let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                                  let w_scr = w * rect.width() * self.view.scale;
                                                  let h_scr = h * rect.height() * self.view.scale;
                                                  let mut new_w_scr = w_scr;
                                                  let mut new_h_scr = h_scr;
                                                  let mut shift_x_scr = 0.0f32;
                                                  let mut shift_y_scr = 0.0f32;
                                                  match edge_idx {
                                                      0 => { new_h_scr = (h_scr - delta.y).max(1.0); shift_y_scr = delta.y / 2.0; },
                                                      1 => { new_w_scr = (w_scr + delta.x).max(1.0); shift_x_scr = delta.x / 2.0; },
                                                      2 => { new_h_scr = (h_scr + delta.y).max(1.0); shift_y_scr = delta.y / 2.0; },
                                                      3 => { new_w_scr = (w_scr - delta.x).max(1.0); shift_x_scr = delta.x / 2.0; },
                                                      _ => {}
                                                  }
                                                  let new_w = new_w_scr / (rect.width() * self.view.scale);
                                                  let new_h = new_h_scr / (rect.height() * self.view.scale);
                                                  m.x += shift_x_scr / (rect.width() * self.view.scale);
                                                  m.y += shift_y_scr / (rect.height() * self.view.scale);
                                                  m.params.insert("width".to_string(), new_w.max(0.01).into());
                                                  m.params.insert("height".to_string(), new_h.max(0.01).into());
                                              },
                                              _ => {}
                                          }
                                          // End scene mask branch
                                      } else if let Some(m) = self.state.masks.iter_mut().find(|m| Some(m.id) == self.view.drag_id) {
                                          // Fall back to global masks if not found
                                          match m.mask_type.as_str() {
                                              "scanner" => {
                                                  let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                                  let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                                  let rot_deg = m.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                                                  let rot = rot_deg.to_radians();
                                                  let cos_r = rot.cos();
                                                  let sin_r = rot.sin();
                                                  let ldx_scr = delta.x * cos_r + delta.y * sin_r;
                                                  let ldy_scr = -delta.x * sin_r + delta.y * cos_r;
                                                  let w_scr = w * rect.width() * self.view.scale;
                                                  let h_scr = h * rect.height() * self.view.scale;
                                                  let mut new_w_scr = w_scr;
                                                  let mut new_h_scr = h_scr;
                                                  let mut shift_lx_scr = 0.0;
                                                  let mut shift_ly_scr = 0.0;
                                                  match edge_idx {
                                                      0 => { new_h_scr = (h_scr - ldy_scr).max(1.0); shift_ly_scr = -(new_h_scr - h_scr) / 2.0; },
                                                      1 => { new_w_scr = (w_scr + ldx_scr).max(1.0); shift_lx_scr = (new_w_scr - w_scr) / 2.0; },
                                                      2 => { new_h_scr = (h_scr + ldy_scr).max(1.0); shift_ly_scr = (new_h_scr - h_scr) / 2.0; },
                                                      3 => { new_w_scr = (w_scr - ldx_scr).max(1.0); shift_lx_scr = -(new_w_scr - w_scr) / 2.0; },
                                                      _ => {}
                                                  }
                                                  let new_w = new_w_scr / (rect.width() * self.view.scale);
                                                  let new_h = new_h_scr / (rect.height() * self.view.scale);
                                                  let wx_shift_scr = shift_lx_scr * cos_r - shift_ly_scr * sin_r;
                                                  let wy_shift_scr = shift_lx_scr * sin_r + shift_ly_scr * cos_r;
                                                  let wx_shift_norm = wx_shift_scr / (rect.width() * self.view.scale);
                                                  let wy_shift_norm = wy_shift_scr / (rect.height() * self.view.scale);
                                                  m.x += wx_shift_norm; m.y += wy_shift_norm;
                                                  m.params.insert("width".to_string(), new_w.max(0.01).into());
                                                  m.params.insert("height".to_string(), new_h.max(0.01).into());
                                              },
                                              "radial" => {
                                                  let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                                  let dr_scr = delta.x;
                                                  let dr_norm = dr_scr / (rect.width() * self.view.scale);
                                                  m.params.insert("radius".to_string(), (r + dr_norm).max(0.01).into());
                                              },
                                              "orbit" => {
                                                  let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                                  let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                                  let w_scr = w * rect.width() * self.view.scale;
                                                  let h_scr = h * rect.height() * self.view.scale;
                                                  let mut new_w_scr = w_scr;
                                                  let mut new_h_scr = h_scr;
                                                  let mut shift_x_scr = 0.0f32;
                                                  let mut shift_y_scr = 0.0f32;
                                                  match edge_idx {
                                                      0 => { new_h_scr = (h_scr - delta.y).max(1.0); shift_y_scr = delta.y / 2.0; },
                                                      1 => { new_w_scr = (w_scr + delta.x).max(1.0); shift_x_scr = delta.x / 2.0; },
                                                      2 => { new_h_scr = (h_scr + delta.y).max(1.0); shift_y_scr = delta.y / 2.0; },
                                                      3 => { new_w_scr = (w_scr - delta.x).max(1.0); shift_x_scr = delta.x / 2.0; },
                                                      _ => {}
                                                  }
                                                  let new_w = new_w_scr / (rect.width() * self.view.scale);
                                                  let new_h = new_h_scr / (rect.height() * self.view.scale);
                                                  m.x += shift_x_scr / (rect.width() * self.view.scale);
                                                  m.y += shift_y_scr / (rect.height() * self.view.scale);
                                                  m.params.insert("width".to_string(), new_w.max(0.01).into());
                                                  m.params.insert("height".to_string(), new_h.max(0.01).into());
                                              },
                                              _ => {}
                                          }
                                      }
                                  } else if let Some(m) = self.state.masks.iter_mut().find(|m| Some(m.id) == self.view.drag_id) {
                                      // No scene selected; operate on global masks
                                      match m.mask_type.as_str() {
                                          "scanner" => {
                                              let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                              let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                              let rot_deg = m.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                                              let rot = rot_deg.to_radians();
                                              let cos_r = rot.cos();
                                              let sin_r = rot.sin();
                                              let ldx_scr = delta.x * cos_r + delta.y * sin_r;
                                              let ldy_scr = -delta.x * sin_r + delta.y * cos_r;
                                              let w_scr = w * rect.width() * self.view.scale;
                                              let h_scr = h * rect.height() * self.view.scale;
                                              let mut new_w_scr = w_scr;
                                              let mut new_h_scr = h_scr;
                                              let mut shift_lx_scr = 0.0;
                                              let mut shift_ly_scr = 0.0;
                                              match edge_idx {
                                                  0 => { new_h_scr = (h_scr - ldy_scr).max(1.0); shift_ly_scr = -(new_h_scr - h_scr) / 2.0; },
                                                  1 => { new_w_scr = (w_scr + ldx_scr).max(1.0); shift_lx_scr = (new_w_scr - w_scr) / 2.0; },
                                                  2 => { new_h_scr = (h_scr + ldy_scr).max(1.0); shift_ly_scr = (new_h_scr - h_scr) / 2.0; },
                                                  3 => { new_w_scr = (w_scr - ldx_scr).max(1.0); shift_lx_scr = -(new_w_scr - w_scr) / 2.0; },
                                                  _ => {}
                                              }
                                              let new_w = new_w_scr / (rect.width() * self.view.scale);
                                              let new_h = new_h_scr / (rect.height() * self.view.scale);
                                              let wx_shift_scr = shift_lx_scr * cos_r - shift_ly_scr * sin_r;
                                              let wy_shift_scr = shift_lx_scr * sin_r + shift_ly_scr * cos_r;
                                              let wx_shift_norm = wx_shift_scr / (rect.width() * self.view.scale);
                                              let wy_shift_norm = wy_shift_scr / (rect.height() * self.view.scale);
                                              m.x += wx_shift_norm; m.y += wy_shift_norm;
                                              m.params.insert("width".to_string(), new_w.max(0.01).into());
                                              m.params.insert("height".to_string(), new_h.max(0.01).into());
                                          },
                                          "radial" => {
                                              let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                              let dr_scr = delta.x;
                                              let dr_norm = dr_scr / (rect.width() * self.view.scale);
                                              m.params.insert("radius".to_string(), (r + dr_norm).max(0.01).into());
                                          },
                                          "orbit" => {
                                              let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                              let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                              let w_scr = w * rect.width() * self.view.scale;
                                              let h_scr = h * rect.height() * self.view.scale;
                                              let mut new_w_scr = w_scr;
                                              let mut new_h_scr = h_scr;
                                              let mut shift_x_scr = 0.0f32;
                                              let mut shift_y_scr = 0.0f32;
                                              match edge_idx {
                                                  0 => { new_h_scr = (h_scr - delta.y).max(1.0); shift_y_scr = delta.y / 2.0; },
                                                  1 => { new_w_scr = (w_scr + delta.x).max(1.0); shift_x_scr = delta.x / 2.0; },
                                                  2 => { new_h_scr = (h_scr + delta.y).max(1.0); shift_y_scr = delta.y / 2.0; },
                                                  3 => { new_w_scr = (w_scr - delta.x).max(1.0); shift_x_scr = delta.x / 2.0; },
                                                  _ => {}
                                              }
                                              let new_w = new_w_scr / (rect.width() * self.view.scale);
                                              let new_h = new_h_scr / (rect.height() * self.view.scale);
                                              m.x += shift_x_scr / (rect.width() * self.view.scale);
                                              m.y += shift_y_scr / (rect.height() * self.view.scale);
                                              m.params.insert("width".to_string(), new_w.max(0.01).into());
                                              m.params.insert("height".to_string(), new_h.max(0.01).into());
                                          },
                                          _ => {}
                                      }
                                  }
                              }
                         }
                    } else {
                        // Pan View - offset is in Pixels
                        self.view.offset.x += delta.x;
                        self.view.offset.y += delta.y;
                    }
                }
                
                if response.drag_released() {
                    // Apply snapping on release (if shift not held)
                    if !shift_held && self.view.drag_id.is_some() {
                        let drag_id = self.view.drag_id;
                        let drag_type = self.view.drag_type;

                        // Helper to snap mask position (for move)
                        let snap_mask_position = |m: &mut crate::model::Mask| {
                            // Snap center to nearest grid intersection
                            m.x = snap_to_grid_x(m.x);
                            m.y = snap_to_grid_y(m.y);
                        };

                        // Helper to snap mask edge (for resize)
                        let snap_mask_edge = |m: &mut crate::model::Mask, edge_idx: usize| {
                            let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                            let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;

                            match edge_idx {
                                0 => { // Top edge - snap top Y to grid
                                    let top_y = m.y - h / 2.0;
                                    let snapped_top = snap_to_grid_y(top_y);
                                    let new_h = h + (top_y - snapped_top);
                                    m.y = snapped_top + new_h / 2.0;
                                    m.params.insert("height".to_string(), new_h.max(0.01).into());
                                },
                                1 => { // Right edge - snap right X to grid
                                    let right_x = m.x + w / 2.0;
                                    let snapped_right = snap_to_grid_x(right_x);
                                    let new_w = w + (snapped_right - right_x);
                                    m.x = snapped_right - new_w / 2.0;
                                    m.params.insert("width".to_string(), new_w.max(0.01).into());
                                },
                                2 => { // Bottom edge - snap bottom Y to grid
                                    let bottom_y = m.y + h / 2.0;
                                    let snapped_bottom = snap_to_grid_y(bottom_y);
                                    let new_h = h + (snapped_bottom - bottom_y);
                                    m.y = snapped_bottom - new_h / 2.0;
                                    m.params.insert("height".to_string(), new_h.max(0.01).into());
                                },
                                3 => { // Left edge - snap left X to grid
                                    let left_x = m.x - w / 2.0;
                                    let snapped_left = snap_to_grid_x(left_x);
                                    let new_w = w + (left_x - snapped_left);
                                    m.x = snapped_left + new_w / 2.0;
                                    m.params.insert("width".to_string(), new_w.max(0.01).into());
                                },
                                _ => {}
                            }
                        };

                        // Find the mask and apply appropriate snapping
                        let apply_snap = |m: &mut crate::model::Mask| {
                            match drag_type {
                                DragType::Mask => snap_mask_position(m),
                                DragType::ResizeMask(edge_idx) => snap_mask_edge(m, edge_idx),
                                _ => {}
                            }
                        };

                        if let Some(sel) = self.state.selected_scene_id {
                            if let Some(scene_index) = self.state.scenes.iter().position(|s| s.id == sel && s.kind == "Masks") {
                                if let Some(m) = self.state.scenes[scene_index].masks.iter_mut().find(|m| Some(m.id) == drag_id) {
                                    apply_snap(m);
                                }
                            } else if let Some(m) = self.state.masks.iter_mut().find(|m| Some(m.id) == drag_id) {
                                apply_snap(m);
                            }
                        } else if let Some(m) = self.state.masks.iter_mut().find(|m| Some(m.id) == drag_id) {
                            apply_snap(m);
                        }
                    }

                    self.view.drag_id = None;
                    self.view.drag_type = DragType::None;
                    self.mark_state_changed();
                }

                // RENDERING
                // Background
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(15, 15, 18));
                
                
                // Draw bounds (Fit to strips)
                let mut b_min_x: f32 = if self.state.strips.is_empty() { 0.0 } else { f32::MAX };
                let mut b_min_y: f32 = if self.state.strips.is_empty() { 0.0 } else { f32::MAX };
                let mut b_max_x: f32 = if self.state.strips.is_empty() { 1.0 } else { f32::MIN };
                let mut b_max_y: f32 = if self.state.strips.is_empty() { 1.0 } else { f32::MIN };

                for s in &self.state.strips {
                    b_min_x = b_min_x.min(s.x);
                    b_min_y = b_min_y.min(s.y);
                    b_max_x = b_max_x.max(s.x);
                    b_max_y = b_max_y.max(s.y);
                    
                     if s.pixel_count > 1 {
                        let len = (s.pixel_count - 1) as f32 * s.spacing;
                        // Strip always extends Right
                        let tail_x = s.x + len;
                        let tail_y = s.y;
                        b_min_x = b_min_x.min(tail_x);
                        b_min_y = b_min_y.min(tail_y);
                        b_max_x = b_max_x.max(tail_x);
                        b_max_y = b_max_y.max(tail_y);
                    }
                }
                
                // Calculate bounds dimensions for grid
                let bounds_width = b_max_x - b_min_x;
                let bounds_height = b_max_y - b_min_y;

                // Only draw grid if we have strips
                if !self.state.strips.is_empty() && bounds_width > 0.0 {
                    // Get visible world coordinates (canvas corners)
                    let (visible_min_x, visible_min_y) = from_screen(rect.left_top(), &self.view);
                    let (visible_max_x, visible_max_y) = from_screen(rect.right_bottom(), &self.view);

                    // Grid colors - visible but subtle
                    let grid_color_major = egui::Color32::from_rgba_unmultiplied(70, 70, 70, 180);    // Full units
                    let grid_color_half = egui::Color32::from_rgba_unmultiplied(55, 55, 55, 140);     // Half units
                    let grid_color_quarter = egui::Color32::from_rgba_unmultiplied(45, 45, 45, 110);  // Quarter units
                    let grid_color_minor = egui::Color32::from_rgba_unmultiplied(38, 38, 38, 80);     // Smaller

                    let target_min_pixels = 25.0;
                    let target_max_pixels = 80.0;

                    // X GRID (vertical lines) - based on LED bounds width
                    let grid_unit_x = bounds_width;
                    let unit_pixels_x = grid_unit_x * rect.width() * self.view.scale;

                    let mut subdivisions_x = 1;
                    while unit_pixels_x / (subdivisions_x as f32) > target_max_pixels && subdivisions_x < 128 {
                        subdivisions_x *= 2;
                    }
                    while unit_pixels_x / (subdivisions_x as f32) < target_min_pixels && subdivisions_x > 1 {
                        subdivisions_x /= 2;
                    }
                    let cell_size_x = grid_unit_x / subdivisions_x as f32;

                    let start_x = b_min_x + ((visible_min_x - b_min_x) / cell_size_x).floor() * cell_size_x;
                    let end_x = b_min_x + ((visible_max_x - b_min_x) / cell_size_x).ceil() * cell_size_x;

                    // Draw vertical grid lines
                    let mut x = start_x;
                    while x <= end_x {
                        let top = to_screen(x, visible_min_y, &self.view);
                        let bottom = to_screen(x, visible_max_y, &self.view);

                        let rel_x = x - b_min_x;
                        let units_from_origin = rel_x / grid_unit_x;

                        let is_full_unit = (units_from_origin - units_from_origin.round()).abs() < 0.0001;
                        let is_half_unit = ((units_from_origin * 2.0) - (units_from_origin * 2.0).round()).abs() < 0.0001;
                        let is_quarter_unit = ((units_from_origin * 4.0) - (units_from_origin * 4.0).round()).abs() < 0.0001;

                        let (stroke_width, color) = if is_full_unit {
                            (1.0, grid_color_major)
                        } else if is_half_unit {
                            (0.75, grid_color_half)
                        } else if is_quarter_unit {
                            (0.5, grid_color_quarter)
                        } else {
                            (0.5, grid_color_minor)
                        };

                        painter.line_segment([top, bottom], egui::Stroke::new(stroke_width, color));

                        // Draw labels at significant positions
                        let label_pixels = cell_size_x * rect.width() * self.view.scale;
                        if label_pixels > 60.0 && is_quarter_unit && top.y > rect.top() + 5.0 {
                            let frac = units_from_origin;
                            let label = if (frac - frac.round()).abs() < 0.001 {
                                if frac.round() as i32 == 0 { None } else { Some(format!("{}", frac.round() as i32)) }
                            } else if ((frac * 2.0) - (frac * 2.0).round()).abs() < 0.001 {
                                let n = (frac * 2.0).round() as i32;
                                if n % 2 == 0 { None } else { Some(format!("{}/2", n)) }
                            } else if ((frac * 4.0) - (frac * 4.0).round()).abs() < 0.001 {
                                let n = (frac * 4.0).round() as i32;
                                if n % 2 == 0 { None } else { Some(format!("{}/4", n)) }
                            } else {
                                None
                            };

                            if let Some(text) = label {
                                painter.text(
                                    egui::pos2(top.x + 3.0, rect.top() + 5.0),
                                    egui::Align2::LEFT_TOP,
                                    text,
                                    egui::FontId::proportional(10.0),
                                    egui::Color32::from_rgba_unmultiplied(140, 140, 140, 180),
                                );
                            }
                        }

                        x += cell_size_x;
                    }

                    // Y GRID (horizontal lines) - based on LED bounds height
                    // Use bounds_height if meaningful, otherwise fall back to bounds_width
                    let grid_unit_y = if bounds_height > 0.001 { bounds_height } else { bounds_width };
                    let unit_pixels_y = grid_unit_y * rect.height() * self.view.scale;

                    let mut subdivisions_y = 1;
                    while unit_pixels_y / (subdivisions_y as f32) > target_max_pixels && subdivisions_y < 128 {
                        subdivisions_y *= 2;
                    }
                    while unit_pixels_y / (subdivisions_y as f32) < target_min_pixels && subdivisions_y > 1 {
                        subdivisions_y /= 2;
                    }
                    let cell_size_y = grid_unit_y / subdivisions_y as f32;

                    let start_y = b_min_y + ((visible_min_y - b_min_y) / cell_size_y).floor() * cell_size_y;
                    let end_y = b_min_y + ((visible_max_y - b_min_y) / cell_size_y).ceil() * cell_size_y;

                    // Draw horizontal grid lines
                    let mut y = start_y;
                    while y <= end_y {
                        let left = to_screen(visible_min_x, y, &self.view);
                        let right = to_screen(visible_max_x, y, &self.view);

                        let rel_y = y - b_min_y;
                        let units_from_origin = rel_y / grid_unit_y;

                        let is_full_unit = (units_from_origin - units_from_origin.round()).abs() < 0.0001;
                        let is_half_unit = ((units_from_origin * 2.0) - (units_from_origin * 2.0).round()).abs() < 0.0001;
                        let is_quarter_unit = ((units_from_origin * 4.0) - (units_from_origin * 4.0).round()).abs() < 0.0001;

                        let (stroke_width, color) = if is_full_unit {
                            (1.0, grid_color_major)
                        } else if is_half_unit {
                            (0.75, grid_color_half)
                        } else if is_quarter_unit {
                            (0.5, grid_color_quarter)
                        } else {
                            (0.5, grid_color_minor)
                        };

                        painter.line_segment([left, right], egui::Stroke::new(stroke_width, color));

                        y += cell_size_y;
                    }
                }

                // Strips
                for s in &self.state.strips {
                    let pos = to_screen(s.x, s.y, &self.view);
                    
                    // Draw Head (Start)
                    painter.rect_filled(
                        egui::Rect::from_center_size(pos, egui::vec2(8.0, 8.0)), 
                        1.0, 
                        egui::Color32::from_rgb(0, 255, 255) // Cyan
                    );
                    painter.rect_stroke(
                         egui::Rect::from_center_size(pos, egui::vec2(8.0, 8.0)),
                         1.0,
                         egui::Stroke::new(1.0, egui::Color32::BLACK)
                    );
                    
                    // Draw Label "U:C"
                    painter.text(
                        pos + egui::vec2(8.0, -8.0),
                        egui::Align2::LEFT_BOTTOM,
                        format!("{}:{}", s.universe, s.start_channel),
                        egui::FontId::proportional(12.0),
                        egui::Color32::WHITE,
                    );

                    // Draw Line of Pixels representation
                    if s.pixel_count > 0 {
                        let _spacing = s.spacing;
                        // let angle = s.rotation.to_radians(); -> Removed
                        // let _dir = egui::vec2(angle.cos(), angle.sin());
                        
                        // We actually draw the pixels in the Engine loop usually, 
                        // but here we can draw a "ghost" line or the pixels themselves if we have data.
                        // The previous code drew pixels. Let's keep that logic but assume it's below.
                    }
                    
                    // Draw pixels based on simulation data...
                    for i in 0..s.pixel_count {
                        // Calculate world pos of pixel i
                        // Calculate world pos of pixel i
                        // Reverse in place
                        let effective_offset = if s.flipped {
                             ((s.pixel_count - 1).saturating_sub(i)) as f32 * s.spacing
                        } else {
                             i as f32 * s.spacing
                        };
                        let px_world = s.x + effective_offset;
                        let py_world = s.y;

                        let px_screen = to_screen(px_world, py_world, &self.view);

                        // Color from data (rgb_data is Vec<[u8; 3]>, so length is pixel count)
                        let rgb_data = &s.data;
                        let color = if i < rgb_data.len() {
                            let p = rgb_data[i];
                            egui::Color32::from_rgb(p[0], p[1], p[2])
                        } else {
                            egui::Color32::GRAY
                        };
                        
                        painter.rect_filled(
                            egui::Rect::from_center_size(px_screen, egui::vec2(4.0, 4.0)),
                            1.0, 
                            color
                        );
                    }
                }
                
                // Masks
                for m in &active_masks {
                    let pos = to_screen(m.x, m.y, &self.view);
                    
                    let mut rgb = m.params.get("color").and_then(|v| {
                        serde_json::from_value::<Vec<u8>>(serde_json::json!(v)).ok() // Hacky conversion
                    }).unwrap_or(vec![255, 0, 0]);
                    if rgb.len() < 3 { rgb = vec![255, 0, 0]; }
                    
                    let mode = m.params.get("color_mode").and_then(|v| v.as_str()).unwrap_or("static");
                    
                    // TRANSPARENCY FIX: Use less alpha (30)
                    let base_color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                    let color = egui::Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], 30); 
                    // Define stroke_color for Radial use
                    let stroke_color = base_color;

                    match m.mask_type.as_str() {
                         "scanner" => {
                             let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                             let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                             let speed_param = m.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                             let rotation_deg = m.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                             let rot = rotation_deg.to_radians();
                             let cos_r = rot.cos();
                             let sin_r = rot.sin();


                             
                             // Helper: rotate a normalized local offset (lx, ly) by rot and convert to screen
                             // We rotate in WORLD/normalized space first, then map to screen.
                             let rotate_norm_to_screen = |lx: f32, ly: f32| -> egui::Pos2 {
                                 let rx_n = lx * cos_r - ly * sin_r;
                                 let ry_n = lx * sin_r + ly * cos_r;
                                 to_screen(m.x + rx_n, m.y + ry_n, &self.view)
                             };

                             // 1. Draw Rotated Box (consistent with engine math)
                             let half_w_n = w / 2.0;
                             let half_h_n = h / 2.0;
                             let corners = vec![
                                 rotate_norm_to_screen(-half_w_n, -half_h_n),
                                 rotate_norm_to_screen( half_w_n, -half_h_n),
                                 rotate_norm_to_screen( half_w_n,  half_h_n),
                                 rotate_norm_to_screen(-half_w_n,  half_h_n),
                             ];
                             
                             painter.add(egui::Shape::convex_polygon(
                                 corners.clone(),
                                 color,
                                 egui::Stroke::new(2.0, base_color)
                             ));
                             
                             // VISUALIZE SCANNER BAR
                             let t = self.engine.get_time();
                             
                             let is_sync = m.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(false);
                             let phase = if is_sync {
                                 let beat = self.engine.get_beat();
                                 let rate_str = m.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4");
                                 let divisor = match rate_str {
                                     "4 Bar" => 16.0,
                                     "2 Bar" => 8.0,
                                     "1 Bar" => 4.0,
                                     "1/2" => 2.0,
                                     "1/4" => 1.0, 
                                     "1/8" => 0.5,
                                     _ => 1.0,
                                 };
                                 let start_pos = m.params.get("start_pos").and_then(|v| v.as_str()).unwrap_or("Center");
                                 let offset = match start_pos {
                                    "Right" => 0.25,
                                    "Left" => 0.75,
                                    _ => 0.0,
                                 };
                                 (beat / divisor + offset) * std::f64::consts::PI * 2.0
                             } else {
                                 (t * speed_param * self.engine.speed) as f64
                             };
                             
                             // Motion Easing
                             // Motion Easing
                             let motion = m.params.get("motion").and_then(|v| v.as_str()).unwrap_or("Smooth");
                             let unidirectional = m.params.get("unidirectional").and_then(|v| v.as_bool()).unwrap_or(false);

                             let osc_val = if unidirectional {
                                  let norm_phase = (phase / (std::f64::consts::PI * 2.0)).fract();
                                  let p = if norm_phase < 0.0 { norm_phase + 1.0 } else { norm_phase };
                                  p * 2.0 - 1.0
                             } else if motion == "Linear" {
                                 (2.0 / std::f64::consts::PI) * (phase.sin().asin())
                             } else {
                                 phase.sin()
                             };

                             // Offset of bar center in NORMALIZED units
                             let offset_x_n = (w / 2.0) * osc_val as f32;
                             
                             let bar_color = if mode == "gradient" {
                                  // Visualize Multi-Color Gradient
                                  let colors: Vec<[u8; 3]> = m.params.get("gradient_colors").and_then(|v| {
                                      serde_json::from_value(v.clone()).ok()
                                  }).unwrap_or_else(|| {
                                      // Fallback
                                      let c1 = m.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255, 255, 255]);
                                      let c2 = m.params.get("color2").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([0, 0, 0]);
                                      vec![c1, c2]
                                  });
                                  
                                  if colors.is_empty() {
                                      egui::Color32::WHITE
                                  } else {
                                      // Calc progress
                                       let progress = if is_sync {
                                             let beat = self.engine.get_beat();
                                             let rate_str = m.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4");
                                             let divisor = match rate_str {
                                                 "4 Bar" => 16.0, "2 Bar" => 8.0, "1 Bar" => 4.0, "1/2" => 2.0, "1/4" => 1.0, "1/8" => 0.5, _ => 1.0,
                                             };
                                             let start_pos = m.params.get("start_pos").and_then(|v| v.as_str()).unwrap_or("Center");
                                             let offset = match start_pos {
                                                "Right" => 0.25,
                                                "Left" => 0.75,
                                                _ => 0.0,
                                             };
                                             (beat / divisor + offset).fract()
                                       } else {
                                             (t * speed_param).fract() as f64
                                       };
                                       
                                       let n = colors.len();
                                      let scaled = progress * n as f64;
                                      let idx = scaled.floor() as usize;
                                      let sub_t = scaled.fract() as f32;
                                      
                                      let c_start = colors[idx % n];
                                      let c_end = colors[(idx + 1) % n];
                                      
                                      let r = (c_start[0] as f32 * (1.0 - sub_t) + c_end[0] as f32 * sub_t) as u8;
                                      let g = (c_start[1] as f32 * (1.0 - sub_t) + c_end[1] as f32 * sub_t) as u8;
                                      let b = (c_start[2] as f32 * (1.0 - sub_t) + c_end[2] as f32 * sub_t) as u8;
                                      
                                      egui::Color32::from_rgb(r, g, b)
                                  }
                             } else {
                                   let c: [u8; 3] = m.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255, 255, 255]);
                                   egui::Color32::from_rgb(c[0], c[1], c[2])
                             };

                             let bar_width_param = m.params.get("bar_width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                             // Visualization uses normalized units to match engine math
                             let half_bw_n = bar_width_param; // threshold radius
                             let half_h_n = h / 2.0;

                             // Bar is a vertical strip inside the box (Rotated)
                             // Local coords in NORMALIZED space
                             // Center X = offset_x_n, Y = -half_h_n .. half_h_n
                             let p1 = rotate_norm_to_screen(offset_x_n - half_bw_n, -half_h_n);
                             let p2 = rotate_norm_to_screen(offset_x_n + half_bw_n, -half_h_n);
                             let p3 = rotate_norm_to_screen(offset_x_n + half_bw_n,  half_h_n);
                             let p4 = rotate_norm_to_screen(offset_x_n - half_bw_n,  half_h_n);
                             
                             let _hard_edge = m.params.get("hard_edge").and_then(|v| v.as_bool()).unwrap_or(false);
                             
                             // If hard edge, solid fill. If soft, maybe use alpha?
                             // Egui simple painter doesn't do gradient fills easily.
                             // Let's rely on Color to show the center, and fading alpha?
                             // Actually, user wants Hard Edge to be visible.
                             let b_color = egui::Color32::from_rgba_unmultiplied(bar_color.r(), bar_color.g(), bar_color.b(), 80);

                             painter.add(egui::Shape::convex_polygon(
                                 vec![p1, p2, p3, p4],
                                 b_color,
                                 egui::Stroke::NONE
                             ));
                             
                          },
                         "radial" => {
                             let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                             let radius_screen = r * rect.width() * self.view.scale; // Width as basis

                             painter.circle(pos, radius_screen, color, egui::Stroke::new(2.0, stroke_color));
                         },
                         "burst" => {
                             let base_r = m.params.get("base_radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                             let max_r = m.params.get("max_radius").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;

                             // Draw base radius
                             let radius_screen = base_r * rect.width() * self.view.scale;
                             painter.circle(pos, radius_screen, color, egui::Stroke::new(2.0, stroke_color));

                             // Draw max radius (dotted)
                             let max_radius_screen = max_r * rect.width() * self.view.scale;
                             painter.circle(pos, max_radius_screen, egui::Color32::TRANSPARENT,
                                 egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(
                                     stroke_color.r(), stroke_color.g(), stroke_color.b(), 100)));
                         },
                         "orbit" => {
                             let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                             let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                             let bar_width_param = m.params.get("bar_width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                             let speed_param = m.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;

                             let half_w = w / 2.0;
                             let half_h = h / 2.0;

                             // Draw rectangle outline
                             let corners = vec![
                                 to_screen(m.x - half_w, m.y - half_h, &self.view),
                                 to_screen(m.x + half_w, m.y - half_h, &self.view),
                                 to_screen(m.x + half_w, m.y + half_h, &self.view),
                                 to_screen(m.x - half_w, m.y + half_h, &self.view),
                             ];

                             painter.add(egui::Shape::convex_polygon(
                                 corners.clone(),
                                 color,
                                 egui::Stroke::new(2.0, base_color)
                             ));

                             // Calculate phase for orbit animation
                             let t = self.engine.get_time();
                             let is_sync = m.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(false);
                             let constant_speed = m.params.get("constant_speed").and_then(|v| v.as_bool()).unwrap_or(false);
                             let raw_phase = if is_sync {
                                 let beat = self.engine.get_beat();
                                 let rate_str = m.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4");
                                 let divisor = match rate_str {
                                     "4 Bar" => 16.0, "2 Bar" => 8.0, "1 Bar" => 4.0,
                                     "1/2" => 2.0, "1/4" => 1.0, "1/8" => 0.5, _ => 1.0,
                                 };
                                 beat / divisor
                             } else {
                                 (t * speed_param * self.engine.speed / 4.0) as f64
                             };

                             // Calculate side and progress based on constant_speed setting
                             let (side, side_progress): (u32, f32) = if constant_speed {
                                 // Constant speed: bar moves at same speed on all sides
                                 // Each side still starts on the beat, but shorter sides finish early and pause
                                 let phase = (raw_phase * 4.0).rem_euclid(4.0);
                                 let current_side = phase.floor() as u32;
                                 let beat_progress = phase.fract() as f32;

                                 let max_side = w.max(h);
                                 let current_side_length = match current_side {
                                     0 | 2 => w,
                                     _ => h,
                                 };

                                 let side_duration_ratio = current_side_length / max_side;
                                 let progress = if beat_progress >= side_duration_ratio {
                                     -1.0 // Hide bar until next beat
                                 } else {
                                     beat_progress / side_duration_ratio
                                 };

                                 (current_side, progress)
                             } else {
                                 let phase = (raw_phase * 4.0).rem_euclid(4.0);
                                 (phase.floor() as u32, phase.fract() as f32)
                             };

                             // Only draw bar if not hidden (side_progress >= 0)
                             if side_progress >= 0.0 {
                                 // Calculate bar position based on side
                                 let (bar_center_x, bar_center_y, is_horizontal) = match side {
                                     0 => (-half_w + side_progress * w, -half_h, false),
                                     1 => (half_w, -half_h + side_progress * h, true),
                                     2 => (half_w - side_progress * w, half_h, false),
                                     _ => (-half_w, half_h - side_progress * h, true),
                                 };

                                 // Draw the bar
                                 let bar_color = egui::Color32::from_rgba_unmultiplied(base_color.r(), base_color.g(), base_color.b(), 120);
                                 let bar_points = if is_horizontal {
                                     // Horizontal bar (on left/right edges)
                                     vec![
                                         to_screen(m.x - half_w, m.y + bar_center_y - bar_width_param, &self.view),
                                         to_screen(m.x + half_w, m.y + bar_center_y - bar_width_param, &self.view),
                                         to_screen(m.x + half_w, m.y + bar_center_y + bar_width_param, &self.view),
                                         to_screen(m.x - half_w, m.y + bar_center_y + bar_width_param, &self.view),
                                     ]
                                 } else {
                                     // Vertical bar (on top/bottom edges)
                                     vec![
                                         to_screen(m.x + bar_center_x - bar_width_param, m.y - half_h, &self.view),
                                         to_screen(m.x + bar_center_x + bar_width_param, m.y - half_h, &self.view),
                                         to_screen(m.x + bar_center_x + bar_width_param, m.y + half_h, &self.view),
                                         to_screen(m.x + bar_center_x - bar_width_param, m.y + half_h, &self.view),
                                     ]
                                 };

                                 painter.add(egui::Shape::convex_polygon(
                                     bar_points,
                                     bar_color,
                                     egui::Stroke::NONE
                                 ));
                             }
                         },
                         _ => {}
                    }
                }
            });
        });
        
        // Debounced auto-save (saves 5 seconds after last change)
        if let Some(last_change) = self.last_change_time {
            if last_change.elapsed() >= self.save_debounce {
                self.save_state();
            }
        }

        ctx.request_repaint();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Save state when app is closing
        self.save_state();
    }
}
// Simple RGB color picker helper with Hex Input
fn color_picker(ui: &mut egui::Ui, rgb: &mut [u8; 3], id_source: impl std::hash::Hash) -> bool {
    let mut changed = false;
    let mut arr = [rgb[0], rgb[1], rgb[2]];

    // Get persistent state ID
    let id = ui.make_persistent_id(id_source);
    
    // Get temp hex string from memory or init
    let mut hex_str = ui.data_mut(|d| {
        d.get_temp::<String>(id).unwrap_or_else(|| format!("#{:02X}{:02X}{:02X}", arr[0], arr[1], arr[2]))
    });

    ui.horizontal(|ui| {
        let resp = ui.color_edit_button_srgb(&mut arr);
        if resp.changed() {
            *rgb = arr;
            hex_str = format!("#{:02X}{:02X}{:02X}", arr[0], arr[1], arr[2]);
            changed = true;
        }

        if ui.add(egui::TextEdit::singleline(&mut hex_str).desired_width(60.0)).changed() {
             // Parse hex
             let clean_hex = hex_str.trim().trim_start_matches('#');
             if clean_hex.len() == 6 {
                 if let Ok(val) = u32::from_str_radix(clean_hex, 16) {
                     let r = ((val >> 16) & 0xFF) as u8;
                     let g = ((val >> 8) & 0xFF) as u8;
                     let b = (val & 0xFF) as u8;
                     arr = [r, g, b];
                     *rgb = arr;
                     changed = true;
                 }
             }
        }
    });

    // Write back to memory
    ui.data_mut(|d| d.insert_temp(id, hex_str));
    changed
}

/// Renders LFO controls for a given parameter
/// Returns true if any value changed
fn lfo_controls(
    ui: &mut egui::Ui,
    params: &mut std::collections::HashMap<String, serde_json::Value>,
    param_name: &str,
    id_source: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let lfo_key = |suffix: &str| format!("{}_lfo_{}", param_name, suffix);
    let mut changed = false;

    let mut enabled = params.get(&lfo_key("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    ui.horizontal(|ui| {
        if ui.checkbox(&mut enabled, "LFO").changed() {
            params.insert(lfo_key("enabled"), enabled.into());
            changed = true;
        }

        if !enabled {
            return;
        }

        let mut depth = params.get(&lfo_key("depth"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);
        if ui.add(egui::Slider::new(&mut depth, 0.0..=1.0).text("±%")).changed() {
            params.insert(lfo_key("depth"), depth.into());
            changed = true;
        }

        let mut waveform = params.get(&lfo_key("waveform"))
            .and_then(|v| v.as_str())
            .unwrap_or("sine")
            .to_string();

        egui::ComboBox::from_id_source(format!("{:?}_wave", id_source))
            .selected_text(&waveform)
            .show_ui(ui, |ui| {
                if ui.selectable_label(waveform == "sine", "Sine").clicked() {
                    waveform = "sine".into();
                    changed = true;
                }
                if ui.selectable_label(waveform == "triangle", "Triangle").clicked() {
                    waveform = "triangle".into();
                    changed = true;
                }
                if ui.selectable_label(waveform == "sawtooth", "Sawtooth").clicked() {
                    waveform = "sawtooth".into();
                    changed = true;
                }
            });

        if changed {
            params.insert(lfo_key("waveform"), serde_json::json!(waveform));
        }
    });

    if enabled {
        ui.horizontal(|ui| {
            let mut is_sync = params.get(&lfo_key("sync"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if ui.checkbox(&mut is_sync, "Sync").changed() {
                params.insert(lfo_key("sync"), is_sync.into());
                changed = true;
            }

            if is_sync {
                let mut rate = params.get(&lfo_key("rate"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("1/4")
                    .to_string();

                egui::ComboBox::from_id_source(format!("{:?}_rate", id_source))
                    .selected_text(&rate)
                    .show_ui(ui, |ui| {
                        for r in ["4 Bar", "2 Bar", "1 Bar", "1/2", "1/4", "1/8"] {
                            if ui.selectable_label(rate == r, r).clicked() {
                                rate = r.into();
                                changed = true;
                            }
                        }
                    });

                if changed {
                    params.insert(lfo_key("rate"), serde_json::json!(rate));
                }
            } else {
                let mut hz = params.get(&lfo_key("hz"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0);
                if ui.add(egui::Slider::new(&mut hz, 0.1..=10.0).text("Hz")).changed() {
                    params.insert(lfo_key("hz"), hz.into());
                    changed = true;
                }
            }
        });
    }

    changed
}

// Helper for Launchpad Color Picker
fn launchpad_color_picker_ui(ui: &mut egui::Ui, current_color: &mut u8) -> bool {
    let mut changed = false;

    // Get current RGB values
    let (cr, cg, cb) = launchpad_color_to_rgb(*current_color);

    ui.horizontal(|ui| {
        // Store temp RGB in egui's temporary storage (as f32 0.0-1.0 for color_edit)
        let id = ui.id().with("rgb_picker");
        let mut temp_rgb = ui.ctx().data_mut(|d| {
            d.get_temp::<[f32; 3]>(id).unwrap_or([cr as f32 / 255.0, cg as f32 / 255.0, cb as f32 / 255.0])
        });

        // Visual RGB color picker - let it be any color
        let picker_response = ui.color_edit_button_rgb(&mut temp_rgb);

        // Always store the temp value when it changes
        if picker_response.changed() {
            ui.ctx().data_mut(|d| {
                d.insert_temp(id, temp_rgb);
            });

            // Update to nearest color when user changes the picker
            let r = (temp_rgb[0] * 255.0) as u8;
            let g = (temp_rgb[1] * 255.0) as u8;
            let b = (temp_rgb[2] * 255.0) as u8;
            let nearest = find_nearest_launchpad_color(r, g, b);

            if nearest != *current_color {
                *current_color = nearest;
                changed = true;
            }
        }

        // Calculate and show which Launchpad color will be used
        let r = (temp_rgb[0] * 255.0) as u8;
        let g = (temp_rgb[1] * 255.0) as u8;
        let b = (temp_rgb[2] * 255.0) as u8;
        let nearest = find_nearest_launchpad_color(r, g, b);

        // Show the Launchpad color index
        ui.label("→");
        ui.label(format!("LP #{}", nearest));
    });

    changed
}

// Full 128-color Launchpad palette (official hardware colors)
// Format: (R, G, B) where values are 0-63, converted to 0-255 by multiplying by 4
const LAUNCHPAD_PALETTE: [(u8, u8, u8); 128] = [
    (0, 0, 0), (64, 64, 64), (128, 128, 128), (252, 252, 252), (252, 60, 60), (252, 0, 0), (128, 0, 0), (64, 0, 0),
    (252, 184, 104), (252, 60, 0), (128, 32, 0), (64, 16, 0), (252, 172, 44), (252, 252, 0), (128, 128, 0), (64, 64, 0),
    (132, 252, 48), (80, 252, 0), (40, 128, 0), (20, 64, 0), (72, 252, 72), (0, 252, 0), (0, 128, 0), (0, 64, 0),
    (72, 252, 92), (0, 252, 24), (0, 128, 12), (0, 64, 4), (72, 252, 88), (0, 252, 84), (0, 128, 44), (0, 64, 24),
    (72, 252, 180), (0, 252, 148), (0, 128, 72), (0, 64, 36), (72, 192, 252), (0, 164, 252), (0, 84, 128), (0, 44, 64),
    (72, 132, 252), (0, 84, 252), (0, 44, 128), (0, 24, 64), (44, 36, 252), (0, 0, 252), (0, 0, 128), (0, 0, 64),
    (104, 52, 248), (44, 0, 252), (24, 0, 128), (12, 0, 64), (252, 60, 252), (252, 0, 252), (128, 0, 128), (64, 0, 64),
    (252, 64, 108), (252, 0, 80), (128, 0, 40), (64, 0, 20), (252, 12, 0), (148, 52, 0), (116, 80, 0), (32, 52, 4),
    (0, 56, 0), (0, 72, 24), (0, 20, 108), (0, 0, 252), (0, 68, 76), (16, 0, 200), (124, 124, 124), (28, 28, 28),
    (252, 0, 0), (184, 252, 44), (172, 232, 4), (96, 252, 8), (12, 136, 0), (0, 252, 92), (0, 164, 252), (0, 40, 252),
    (24, 0, 252), (88, 0, 252), (172, 24, 120), (40, 16, 0), (252, 48, 0), (132, 220, 4), (112, 252, 20), (0, 252, 0),
    (56, 252, 36), (84, 252, 108), (52, 252, 200), (88, 136, 252), (48, 80, 192), (104, 80, 228), (208, 28, 252), (252, 0, 88),
    (252, 68, 0), (180, 164, 0), (140, 252, 0), (128, 88, 4), (56, 40, 0), (0, 72, 12), (12, 76, 32), (20, 20, 40),
    (20, 28, 88), (100, 56, 24), (128, 0, 0), (216, 64, 40), (212, 72, 16), (252, 188, 36), (156, 220, 44), (100, 176, 12),
    (20, 20, 44), (216, 208, 104), (124, 232, 136), (152, 148, 252), (140, 100, 252), (60, 60, 60), (112, 112, 112), (220, 252, 252),
    (156, 0, 0), (52, 0, 0), (24, 204, 0), (4, 64, 0), (180, 172, 0), (60, 48, 0), (176, 80, 0), (72, 20, 0),
];

fn launchpad_color_to_egui(code: u8) -> egui::Color32 {
    let idx = code as usize;
    if idx < LAUNCHPAD_PALETTE.len() {
        let (r, g, b) = LAUNCHPAD_PALETTE[idx];
        egui::Color32::from_rgb(r, g, b)
    } else {
        egui::Color32::LIGHT_GRAY
    }
}

fn launchpad_color_to_rgb(code: u8) -> (u8, u8, u8) {
    let idx = code as usize;
    if idx < LAUNCHPAD_PALETTE.len() {
        LAUNCHPAD_PALETTE[idx]
    } else {
        (200, 200, 200)
    }
}

// Find nearest Launchpad color to given RGB using perceptually accurate color distance
fn find_nearest_launchpad_color(r: u8, g: u8, b: u8) -> u8 {
    let mut min_distance = f32::MAX;
    let mut best_idx = 0u8;

    let r1 = r as f32;
    let g1 = g as f32;
    let b1 = b as f32;

    for (idx, &(pr, pg, pb)) in LAUNCHPAD_PALETTE.iter().enumerate() {
        let r2 = pr as f32;
        let g2 = pg as f32;
        let b2 = pb as f32;

        // Use redmean formula for perceptually accurate color distance
        // This accounts for human eye's different sensitivity to R, G, B
        let rmean = (r1 + r2) / 2.0;
        let dr = r1 - r2;
        let dg = g1 - g2;
        let db = b1 - b2;

        let distance = ((2.0 + rmean / 256.0) * dr * dr)
            + (4.0 * dg * dg)
            + ((2.0 + (255.0 - rmean) / 256.0) * db * db);

        if distance < min_distance {
            min_distance = distance;
            best_idx = idx as u8;
        }
    }

    best_idx
}
