

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
            db,
            last_change_time: None,
            save_debounce: Duration::from_secs(5),
            import_dialog_open: false,
            import_merge_mode: false,
            import_file_path: None,
            midi_sender: tx_cmd,
            midi_receiver: rx_event,
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
                            }
                            if !self.state.scenes.is_empty() {
                                if ui.button("Select None").clicked() { self.state.selected_scene_id = None; }
                            }
                        });
                        if self.new_scene_open {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Name:");
                                    ui.text_edit_singleline(&mut self.new_scene_name);
                                });
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut self.new_scene_kind, "Masks".into(), "Masks");
                                    ui.selectable_value(&mut self.new_scene_kind, "Global".into(), "Global effect");
                                });
                                ui.horizontal(|ui| {
                                    if ui.button("Create").clicked() {
                                        let id = rand::random();
                                        let scene = if self.new_scene_kind == "Masks" {
                                            model::Scene { id, name: self.new_scene_name.clone(), kind: "Masks".into(), masks: vec![], global: None, launchpad_btn: None, launchpad_color: None, launchpad_is_cc: false }
                                        } else {
                                            let mut ge = model::GlobalEffect::default();
                                            ge.params.insert("speed".into(), 0.2.into());
                                            model::Scene { id, name: self.new_scene_name.clone(), kind: "Global".into(), masks: vec![], global: Some(ge), launchpad_btn: None, launchpad_color: None, launchpad_is_cc: false }
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
                        let mut needs_save = false;
                        let sender = self.midi_sender.clone();
                        
                        // Collect used IDs to prevent duplicates
                        let mut used_ids = std::collections::HashMap::new();
                        for s in &self.state.scenes {
                            if let Some(btn) = s.launchpad_btn {
                                if btn != 0 {
                                    used_ids.insert((s.launchpad_is_cc, btn), s.id);
                                }
                            }
                        }

                        for (si, scene) in self.state.scenes.iter_mut().enumerate() {
                            ui.push_id(scene.id, |ui| {
                                ui.separator();
                                let selected = self.state.selected_scene_id == Some(scene.id);
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(selected, &scene.name).clicked() {
                                        self.state.selected_scene_id = Some(scene.id);
                                    }
                                    ui.text_edit_singleline(&mut scene.name);
                                    if ui.button("X").clicked() { delete_scene_idx = Some(si); }
                                });
                                
                                // Launchpad Config
                                ui.horizontal(|ui| {
                                    ui.label("Launchpad:");
                                    let is_cc = scene.launchpad_is_cc;
                                    let old_btn = scene.launchpad_btn;
                                    let send_off = |info: Option<u8>, cc: bool, sender: &Sender<midi::MidiCommand>| {
                                        if let Some(old) = info {
                                             let cmd = if cc { midi::MidiCommand::SetButtonColor { cc: old, color: 0 } }
                                                       else { midi::MidiCommand::SetPadColor { note: old, color: 0 } };
                                             let _ = sender.send(cmd);
                                        }
                                    };

                                    if ui.selectable_label(!is_cc, "Note").clicked() {
                                        if is_cc { // Changed from CC to Note
                                            send_off(old_btn, true, &sender);
                                            scene.launchpad_is_cc = false;
                                            // Re-send current if exists
                                            if let (Some(b), Some(c)) = (scene.launchpad_btn, scene.launchpad_color) {
                                                let _ = sender.send(midi::MidiCommand::SetPadColor { note: b, color: c });
                                            }
                                            needs_save = true;
                                        }
                                    }
                                    if ui.selectable_label(is_cc, "CC").clicked() {
                                        if !is_cc { // Changed from Note to CC
                                            send_off(old_btn, false, &sender);
                                            scene.launchpad_is_cc = true;
                                            // Re-send
                                            if let (Some(b), Some(c)) = (scene.launchpad_btn, scene.launchpad_color) {
                                                let _ = sender.send(midi::MidiCommand::SetButtonColor { cc: b, color: c });
                                            }
                                            needs_save = true;
                                        }
                                    }
                                    
                                    let mut val = scene.launchpad_btn.unwrap_or(0);
                                    if ui.add(egui::DragValue::new(&mut val).prefix("ID: ")).changed() {
                                        // Validate Duplicate
                                        let is_dup = if val != 0 {
                                            if let Some(&owner) = used_ids.get(&(scene.launchpad_is_cc, val)) {
                                                owner != scene.id
                                            } else { false }
                                        } else { false };
                                        
                                        if !is_dup {
                                            // Turn off old
                                            send_off(old_btn, scene.launchpad_is_cc, &sender);
                                            
                                            scene.launchpad_btn = Some(val);
                                            // Send new
                                            if let Some(col) = scene.launchpad_color {
                                                let cmd = if scene.launchpad_is_cc { midi::MidiCommand::SetButtonColor { cc: val, color: col } }
                                                          else { midi::MidiCommand::SetPadColor { note: val, color: col } };
                                                let _ = sender.send(cmd);
                                            }
                                            needs_save = true;
                                        }
                                    }
                                    
                                    let mut col = scene.launchpad_color.unwrap_or(0);
                                    if launchpad_color_picker_ui(ui, &mut col) {
                                        scene.launchpad_color = Some(col);
                                        // Send to board immediately
                                        let cmd = if scene.launchpad_is_cc {
                                             midi::MidiCommand::SetButtonColor { cc: val, color: col }
                                        } else {
                                             midi::MidiCommand::SetPadColor { note: val, color: col }
                                        };
                                        let _ = sender.send(cmd);
                                        needs_save = true;
                                    }
                                });
                                if scene.kind == "Global" {
                                    if scene.global.is_none() { scene.global = Some(model::GlobalEffect::default()); }
                                    if let Some(ge) = scene.global.as_mut() {
                                        ui.horizontal(|ui| {
                                            ui.label("Effect:");
                                            egui::ComboBox::from_id_source(format!("ge_kind_{}", scene.id))
                                                .selected_text(ge.kind.clone())
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(&mut ge.kind, "Rainbow".into(), "Rainbow");
                                                    ui.selectable_value(&mut ge.kind, "Solid".into(), "Solid");
                                                    ui.selectable_value(&mut ge.kind, "Flash".into(), "Flash");
                                                    ui.selectable_value(&mut ge.kind, "Sparkle".into(), "Sparkle");
                                                });
                                        });
                                        if ge.kind == "Solid" {
                                            let mut color = ge.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,255,255]);
                                            if color_picker(ui, &mut color) {
                                                ge.params.insert("color".into(), serde_json::json!([color[0], color[1], color[2]]));
                                            }
                                        } else if ge.kind == "Flash" {
                                            // Flash UI
                                            // Color
                                            ui.horizontal(|ui| {
                                                ui.label("Color:");
                                                 let mut color = ge.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,255,255]);
                                                if color_picker(ui, &mut color) {
                                                    ge.params.insert("color".into(), serde_json::json!([color[0], color[1], color[2]]));
                                                }
                                            });
                                            // Rate
                                            ui.horizontal(|ui| {
                                                ui.label("Rate:");
                                                let mut rate = ge.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1 Bar").to_string();
                                                egui::ComboBox::from_id_source(format!("flash_rate_{}", scene.id))
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
                                            // Decay
                                            let mut decay = ge.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(5.0);
                                            if ui.add(egui::Slider::new(&mut decay, 1.0..=20.0).text("Decay (Sharpness)")).changed() {
                                                ge.params.insert("decay".into(), decay.into());
                                            }
                                        } else if ge.kind == "Sparkle" {
                                            // Sparkle UI
                                            // Color
                                            ui.horizontal(|ui| {
                                                ui.label("Color:");
                                                let mut color = ge.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255u8,255,255]);
                                                if color_picker(ui, &mut color) {
                                                    ge.params.insert("color".into(), serde_json::json!([color[0], color[1], color[2]]));
                                                }
                                            });

                                            let mut density = ge.params.get("density").and_then(|v| v.as_f64()).unwrap_or(0.05);
                                            if ui.add(egui::Slider::new(&mut density, 0.001..=0.2).text("Density")).changed() {
                                                ge.params.insert("density".into(), density.into());
                                            }

                                            let mut life = ge.params.get("life").and_then(|v| v.as_f64()).unwrap_or(0.2);
                                            if ui.add(egui::Slider::new(&mut life, 0.05..=2.0).text("Life (seconds)")).changed() {
                                                ge.params.insert("life".into(), life.into());
                                            }

                                            let mut decay = ge.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(5.0);
                                            if ui.add(egui::Slider::new(&mut decay, 1.0..=20.0).text("Decay")).changed() {
                                                ge.params.insert("decay".into(), decay.into());
                                            }
                                        } else {
                                            let mut speed = ge.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(0.2);
                                            if ui.add(egui::Slider::new(&mut speed, 0.05..=2.0).text("Speed")).changed() {
                                                ge.params.insert("speed".into(), speed.into());
                                            }
                                            lfo_controls(ui, &mut ge.params, "speed", format!("speed_lfo_{}", scene.id));
                                        }
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
                                            });
                                    });

                                    let mut delete_mask_idx = None;
                                    let mut needs_save = false;
                                    for (idx, m) in scene.masks.iter_mut().enumerate() {
                                        ui.push_id(m.id, |ui| {
                                            ui.collapsing(format!("{} Mask::{}", m.mask_type, m.id), |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label("Pos:");
                                                    ui.add(egui::Slider::new(&mut m.x, 0.0..=1.0).text("X"));
                                                    ui.add(egui::Slider::new(&mut m.y, 0.0..=1.0).text("Y"));
                                                    if ui.button("🗑").clicked() {
                                                        delete_mask_idx = Some(idx);
                                                    }
                                                });
                                    
                                    // DYNAMIC PARAMS
                                    if m.mask_type == "scanner" {
                                        // Width
                                        let mut w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                        if ui.add(egui::Slider::new(&mut w, 0.0..=50.0).text("Width")).changed() {
                                            m.params.insert("width".into(), w.into());
                                            needs_save = true;
                                        }
                                        if lfo_controls(ui, &mut m.params, "width", format!("width_lfo_{}", m.id)) {
                                            needs_save = true;
                                        }
                                        // Height
                                        let mut h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                        if ui.add(egui::Slider::new(&mut h, 0.0..=50.0).text("Height")).changed() {
                                            m.params.insert("height".into(), h.into());
                                            needs_save = true;
                                        }
                                        if lfo_controls(ui, &mut m.params, "height", format!("height_lfo_{}", m.id)) {
                                            needs_save = true;
                                        }
                                        
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
                                        if ui.add(egui::Slider::new(&mut decay, 0.01..=0.5).text("Decay Speed")).changed() {
                                            m.params.insert("decay".into(), decay.into());
                                            needs_save = true;
                                        }
                                    }
                                    
                                    // Color
                                    ui.horizontal(|ui| {
                                        ui.label("Color:");
                                        let mut rgb = m.params.get("color").and_then(|v| {
                                            serde_json::from_value::<Vec<u8>>(serde_json::json!(v)).ok()
                                        }).unwrap_or(vec![255, 0, 0]);
                                        if rgb.len() < 3 { rgb = vec![255, 0, 0]; }
                                        let mut color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                        
                                        if ui.color_edit_button_srgba(&mut color).changed() {
                                            m.params.insert("color".into(), serde_json::json!([color.r(), color.g(), color.b()]));
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
                                                let mut c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                                if ui.color_edit_button_srgba(&mut c).changed() {
                                                    *rgb = [c.r(), c.g(), c.b()];
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
                                               let rgb = colors[i];
                                               ui.push_id(format!("gcol_{}_{}", m.id, i), |ui| {
                                                    let mut c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                                    if ui.color_edit_button_srgba(&mut c).changed() {
                                                        colors[i] = [c.r(), c.g(), c.b()];
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
                                                            if ui.add(egui::Slider::new(&mut width, 0.01..=1.0).text("Width")).changed() {
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
                                        }
                                        });
                                    // Close collapsing and push_id blocks, then the for-loop
                                    });
                                });
                            }
                            if let Some(idx) = delete_mask_idx { scene.masks.remove(idx); }
                            if needs_save { /* autosave will handle */ }
                        }
                            });
                        }
                        if let Some(i) = delete_scene_idx {
                            self.state.scenes.remove(i);
                            self.mark_state_changed();
                        }

                        if needs_save {
                            self.mark_state_changed();
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
                             // Keep Mask parameter move simple (normalized)
                             let dx = delta.x / (rect.width() * self.view.scale);
                             let dy = delta.y / (rect.height() * self.view.scale);
                             // Move mask in selected scene if active
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
                                                  m.params.insert("width".to_string(), new_w.into());
                                                  m.params.insert("height".to_string(), new_h.into());
                                                  let wx_shift_scr = shift_lx_scr * cos_r - shift_ly_scr * sin_r;
                                                  let wy_shift_scr = shift_lx_scr * sin_r + shift_ly_scr * cos_r;
                                                  let wx_shift_norm = wx_shift_scr / (rect.width() * self.view.scale);
                                                  let wy_shift_norm = wy_shift_scr / (rect.height() * self.view.scale);
                                                  m.x += wx_shift_norm; m.y += wy_shift_norm;
                                              },
                                              "radial" => {
                                                  let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                                  let dr_scr = delta.x;
                                                  let dr_norm = dr_scr / (rect.width() * self.view.scale);
                                                  m.params.insert("radius".to_string(), (r + dr_norm).max(0.01).into());
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
                                                  m.params.insert("width".to_string(), new_w.into());
                                                  m.params.insert("height".to_string(), new_h.into());
                                                  let wx_shift_scr = shift_lx_scr * cos_r - shift_ly_scr * sin_r;
                                                  let wy_shift_scr = shift_lx_scr * sin_r + shift_ly_scr * cos_r;
                                                  let wx_shift_norm = wx_shift_scr / (rect.width() * self.view.scale);
                                                  let wy_shift_norm = wy_shift_scr / (rect.height() * self.view.scale);
                                                  m.x += wx_shift_norm; m.y += wy_shift_norm;
                                              },
                                              "radial" => {
                                                  let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                                  let dr_scr = delta.x; 
                                                  let dr_norm = dr_scr / (rect.width() * self.view.scale);
                                                  m.params.insert("radius".to_string(), (r + dr_norm).max(0.01).into());
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
                                              m.params.insert("width".to_string(), new_w.into());
                                              m.params.insert("height".to_string(), new_h.into());
                                              let wx_shift_scr = shift_lx_scr * cos_r - shift_ly_scr * sin_r;
                                              let wy_shift_scr = shift_lx_scr * sin_r + shift_ly_scr * cos_r;
                                              let wx_shift_norm = wx_shift_scr / (rect.width() * self.view.scale);
                                              let wy_shift_norm = wy_shift_scr / (rect.height() * self.view.scale);
                                              m.x += wx_shift_norm; m.y += wy_shift_norm;
                                          },
                                          "radial" => {
                                              let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                              let dr_scr = delta.x; 
                                              let dr_norm = dr_scr / (rect.width() * self.view.scale);
                                              m.params.insert("radius".to_string(), (r + dr_norm).max(0.01).into());
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
                    self.view.drag_id = None;
                    self.view.drag_type = DragType::None;
                    self.mark_state_changed();
                }

                // RENDERING
                // Background
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(15, 15, 18));
                
                // Grid (infinite)
                let grid_spacing = 0.1 * rect.width() * self.view.scale;
                if grid_spacing > 5.0 { 
                     // Only draw if dense enough
                }
                
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
                
                let tl = to_screen(b_min_x, b_min_y, &self.view);
                let br = to_screen(b_max_x, b_max_y, &self.view);
                painter.rect_stroke(egui::Rect::from_min_max(tl, br), 0.0, egui::Stroke::new(1.0, egui::Color32::from_gray(60)));

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
                             let motion = m.params.get("motion").and_then(|v| v.as_str()).unwrap_or("Smooth");
                             let osc_val = if motion == "Linear" {
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
}
// Simple RGB color picker helper
fn color_picker(ui: &mut egui::Ui, rgb: &mut [u8; 3]) -> bool {
    let mut arr = [rgb[0], rgb[1], rgb[2]];
    let resp = ui.color_edit_button_srgb(&mut arr);
    if resp.changed() {
        *rgb = arr;
        true
    } else { false }
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
    
    ui.horizontal(|ui| {
        // Preview
        let _ = ui.add(egui::Button::new("   ").fill(launchpad_color_to_egui(*current_color)));
        
        ui.menu_button(format!("Color: {}", current_color), |ui| {
            ui.set_width(320.0);
            
            let colors = [
                (5, "Red"), (9, "Amber"), (13, "Yellow"), (21, "Green"), (29, "Mint"), (37, "Azure"), (45, "Blue"), (49, "Purple"),
                (53, "Magenta"), (57, "Pink"), (6, "Dk Red"), (14, "Dk Yellow"), (22, "Dk Green"), (46, "Dk Blue"), (1, "Low White"), (3, "White"),
            ];
            
            egui::Grid::new("launchpad_palette").show(ui, |ui| {
                for (i, (code, name)) in colors.iter().enumerate() {
                    let btn = egui::Button::new("   ")
                        .fill(launchpad_color_to_egui(*code));
                    
                    if ui.add(btn).on_hover_text(*name).clicked() {
                        *current_color = *code;
                        changed = true;
                        ui.close_menu();
                    }
                    
                    if (i + 1) % 8 == 0 {
                        ui.end_row();
                    }
                }
            });
            
            // Manual override
            ui.separator();
            if ui.add(egui::DragValue::new(current_color).prefix("Code: ")).changed() {
                changed = true;
            }
        });
    });

    changed
}

fn launchpad_color_to_egui(code: u8) -> egui::Color32 {
    // Approximate colors
    match code {
        0 => egui::Color32::BLACK,
        1..=3 => egui::Color32::GRAY,
        5 => egui::Color32::RED,
        9 => egui::Color32::from_rgb(255, 100, 0), // Amber
        13 => egui::Color32::YELLOW,
        21 => egui::Color32::GREEN,
        29 => egui::Color32::from_rgb(0, 255, 128), // Mint
        37 => egui::Color32::from_rgb(0, 200, 255), // Azure
        45 => egui::Color32::BLUE,
        49 => egui::Color32::from_rgb(128, 0, 255), // Purple
        53 => egui::Color32::from_rgb(255, 0, 255), // Magenta
        57 => egui::Color32::from_rgb(255, 100, 150), // Pink
        6 => egui::Color32::from_rgb(150, 0, 0),
        14 => egui::Color32::from_rgb(150, 150, 0),
        22 => egui::Color32::from_rgb(0, 150, 0),
        46 => egui::Color32::from_rgb(0, 0, 150),
        72 => egui::Color32::RED, // Bright Red
        _ => egui::Color32::LIGHT_GRAY,
    }
}
