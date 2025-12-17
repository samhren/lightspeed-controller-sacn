mod model;
mod engine;
mod audio;

use eframe::egui;
use model::{AppState, PixelStrip, Mask};
use engine::LightingEngine;
use std::fs;

// View Transform State
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
    ResizeMask,
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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    
    eframe::run_native(
        "Lightspeed Controller",
        options,
        Box::new(|_cc| Box::new(MyApp::default())),
    )
}

struct MyApp {
    state: AppState,
    engine: LightingEngine,
    view: ViewState,
    status: String,
}

impl Default for MyApp {
    fn default() -> Self {
        let mut state = AppState::default();
        
        if let Ok(content) = fs::read_to_string("lighting_config.json") {
            if let Ok(loaded) = serde_json::from_str::<AppState>(&content) {
                state = loaded;
            }
        } else {
             state.strips.push(PixelStrip::default());
             state.masks.push(model::Mask {
                id: 1,
                mask_type: "scanner".into(),
                x: 0.5,
                y: 0.5,
                params: std::collections::HashMap::new(), 
            });
        }

        Self {
            state,
            engine: LightingEngine::new(),
            view: ViewState::default(),
            status: "Ready".to_owned(),
        }
    }
}

impl MyApp {
    fn save_state(&self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.state) {
            let _ = fs::write("lighting_config.json", json);
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        
        // Update Loop (Physics/Networking)
        self.engine.update(&mut self.state);

        egui::CentralPanel::default().show(ctx, |ui| {
            // HEADER AND STATUS
            ui.horizontal(|ui| {
                ui.heading("Lightspeed");
                ui.separator();
                ui.label(egui::RichText::new(format!("Beat: {}", self.engine.current_beat)).color(egui::Color32::LIGHT_GRAY)); // Placeholder
                
                // Beat Indicator
                let bpm = self.engine.get_bpm();
                let beat = self.engine.get_beat();
                let beat_in_bar = ((beat % 4.0).floor() as i32) + 1;
                ui.label(egui::RichText::new(format!("BPM: {:.1} | Beat: {}", bpm, beat_in_bar)).size(18.0).color(egui::Color32::GREEN));

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
                                 ui.add(egui::Slider::new(&mut self.engine.latency_ms, -200.0..=500.0));
                            });
                            ui.horizontal(|ui| {
                                 ui.checkbox(&mut self.engine.use_flywheel, "Beat Smoothing (Flywheel)");
                            });
                            ui.separator();
                            ui.label("Hybrid Sync (Audio)");
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut self.engine.hybrid_sync, "Enable Audio Snap");
                                if self.engine.hybrid_sync {
                                     ui.add(egui::Slider::new(&mut self.engine.audio_sensitivity, 0.0..=1.0).text("Sens"));
                                }
                            });
                        });
                        
                        ui.collapsing("Network Output", |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Universe");
                                ui.add(egui::DragValue::new(&mut self.state.network.universe).speed(1));
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

                        // STRIPS
                        ui.horizontal(|ui| {
                            ui.heading("Strips");
                            if ui.button("➕ Add Strip").clicked() {
                                let mut s = PixelStrip::default();
                                s.id = rand::random();
                                self.state.strips.push(s);
                                self.save_state();
                            }
                        });
                        
                        let mut delete_strip_idx = None;
                        for (idx, s) in self.state.strips.iter_mut().enumerate() {
                            ui.push_id(s.id, |ui| {
                                ui.collapsing(format!("Strip::{}", s.id), |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label("Position:");
                                        ui.add(egui::Slider::new(&mut s.x, 0.0..=1.0).text("X"));
                                        ui.add(egui::Slider::new(&mut s.y, 0.0..=1.0).text("Y"));
                                    });
                                    ui.horizontal(|ui| {
                                         ui.label("Rotation:");
                                         ui.add(egui::Slider::new(&mut s.rotation, 0.0..=6.28));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Config:");
                                        ui.add(egui::DragValue::new(&mut s.universe).prefix("Uni: "));
                                        ui.add(egui::DragValue::new(&mut s.start_channel).prefix("Ch: "));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Layout:");
                                        ui.add(egui::DragValue::new(&mut s.pixel_count).prefix("Count: "));
                                        ui.add(egui::Slider::new(&mut s.spacing, 0.001..=0.05).text("Spacing"));
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

                        // MASKS
                        ui.horizontal(|ui| {
                            ui.heading("Masks");
                            egui::ComboBox::from_id_source("add_mask")
                                .selected_text("Add Mask...")
                                .show_ui(ui, |ui| {
                                    if ui.selectable_label(false, "Scanner").clicked() {
                                        let mut m = Mask {
                                            id: rand::random(),
                                            mask_type: "scanner".into(),
                                            x: 0.5, y: 0.5,
                                            params: std::collections::HashMap::new(),
                                        };
                                        // Default params
                                        m.params.insert("width".into(), 0.3.into());
                                        m.params.insert("height".into(), 0.3.into());
                                        m.params.insert("speed".into(), 1.0.into());
                                        m.params.insert("color".into(), serde_json::json!([0, 255, 255]));
                                        
                                        self.state.masks.push(m);
                                        self.save_state();
                                    }
                                    if ui.selectable_label(false, "Radial").clicked() {
                                        let mut m = Mask {
                                            id: rand::random(),
                                            mask_type: "radial".into(),
                                            x: 0.5, y: 0.5,
                                            params: std::collections::HashMap::new(),
                                        };
                                        m.params.insert("radius".into(), 0.2.into());
                                        m.params.insert("color".into(), serde_json::json!([255, 0, 0]));
                                        self.state.masks.push(m);
                                        self.save_state();
                                    }
                                });
                        });

                        let mut delete_mask_idx = None;
                        let mut needs_save = false;
                        for (idx, m) in self.state.masks.iter_mut().enumerate() {
                            ui.push_id(m.id, |ui| {
                                ui.collapsing(format!("{} Mask::{}", m.mask_type, m.id), |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label("Pos:");
                                        ui.add(egui::Slider::new(&mut m.x, 0.0..=1.0).text("X"));
                                        ui.add(egui::Slider::new(&mut m.y, 0.0..=1.0).text("Y"));
                                    });
                                    
                                    // DYNAMIC PARAMS
                                    if m.mask_type == "scanner" {
                                        // Width
                                        let mut w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                        if ui.add(egui::Slider::new(&mut w, 0.0..=1.0).text("Width")).changed() {
                                            m.params.insert("width".into(), w.into());
                                            needs_save = true;
                                        }
                                        // Height
                                        let mut h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                                        if ui.add(egui::Slider::new(&mut h, 0.0..=1.0).text("Height")).changed() {
                                            m.params.insert("height".into(), h.into());
                                            needs_save = true;
                                        }
                                        // Speed
                                        let mut s = m.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                                        if ui.add(egui::Slider::new(&mut s, 0.1..=5.0).text("Speed")).changed() {
                                            m.params.insert("speed".into(), s.into());
                                            needs_save = true;
                                        }
                                    } else if m.mask_type == "radial" {
                                        let mut r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
                                        if ui.add(egui::Slider::new(&mut r, 0.0..=1.0).text("Radius")).changed() {
                                            m.params.insert("radius".into(), r.into());
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
                                            for (i, rgb) in colors.iter_mut().enumerate() {
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
                                                        if ui.add(egui::Slider::new(&mut width, 0.01..=0.5).text("Width")).changed() {
                                                            m.params.insert("bar_width".into(), width.into());
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
                                        } else {
                                            // Radial just has Speed
                                            let mut speed = m.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0);
                                            if ui.add(egui::Slider::new(&mut speed, 0.1..=5.0).text("Speed")).changed() {
                                                m.params.insert("speed".into(), speed.into());
                                                needs_save = true;
                                            }
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                         if ui.button("🗑 Delete").clicked() {
                                             delete_mask_idx = Some(idx);
                                         }
                                    });
                                });
                            });
                        }
                        if let Some(idx) = delete_mask_idx {
                            self.state.masks.remove(idx);
                            self.save_state();
                        } else if needs_save {
                            self.save_state();
                        }
                    });
                });

                // RIGHT PANEL: CANVAS
                let canvas_ui = &mut columns[1];
                let (response, painter) = canvas_ui.allocate_painter(
                    canvas_ui.available_size(), 
                    egui::Sense::click_and_drag()
                );
                
                let rect = response.rect;
                
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
                let mut input = ctx.input_mut(|i| i.clone());
                
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
                }

                // DRAG LOGIC
                if response.clicked() || response.drag_started() {
                   if let Some(pos) = response.interact_pointer_pos() {
                       let (wx, wy) = from_screen(pos, &self.view);
                       let mut hit = false;
                       
                       // 1. HIT TEST RESIZE HANDLES (Priority over Move)
                       // Only check masks for resizing for now
                       for m in &self.state.masks {
                           // Logic depends on mask type
                           let handle_size = 8.0 / (rect.width() * self.view.scale); // visual size
                           match m.mask_type.as_str() {
                               "scanner" => {
                                   let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                   let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                   // Bottom-Right Corner for resize
                                   let handle_x = m.x + w/2.0;
                                   let handle_y = m.y + h/2.0;
                                   let dist = ((wx - handle_x).powi(2) + (wy - handle_y).powi(2)).sqrt();
                                   if dist < handle_size {
                                       self.view.drag_id = Some(m.id);
                                       self.view.drag_type = DragType::ResizeMask; 
                                       hit = true;
                                       break;
                                   }
                               },
                               "radial" => {
                                   let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                   // Right edge for radius resize
                                   let handle_x = m.x + r;
                                   let handle_y = m.y;
                                   let dist = ((wx - handle_x).powi(2) + (wy - handle_y).powi(2)).sqrt();
                                    if dist < handle_size {
                                       self.view.drag_id = Some(m.id);
                                       self.view.drag_type = DragType::ResizeMask; 
                                       hit = true;
                                       break;
                                   }
                               },
                               _ => {}
                           }
                       }

                       // 2. HIT TEST MOVE (Masks) - Improved to click anywhere
                       if !hit {
                           for m in &self.state.masks {
                               match m.mask_type.as_str() {
                                   "scanner" => {
                                       let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                       let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                       
                                       // Check if point is inside rotated rect? No rotation on masks yet.
                                       // AABB check
                                       let half_w = w / 2.0;
                                       let half_h = h / 2.0; // In normalized 'world' coords
                                       
                                       // Simple Screen Space check
                                       // But wait, our 'hit' check uses 'wx, wy' which are normalized world coords.
                                       if wx >= m.x - half_w && wx <= m.x + half_w && 
                                          wy >= m.y - half_h && wy <= m.y + half_h {
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
                       if !hit {
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
                    let dx = delta.x / (rect.width() * self.view.scale);
                    let dy = delta.y / (rect.height() * self.view.scale);

                    if self.view.drag_id.is_some() {
                         if self.view.drag_type == DragType::Strip {
                             if let Some(s) = self.state.strips.iter_mut().find(|s| Some(s.id) == self.view.drag_id) {
                                 s.x += dx;
                                 s.y += dy;
                             }
                         } else if self.view.drag_type == DragType::Mask {
                             if let Some(m) = self.state.masks.iter_mut().find(|m| Some(m.id) == self.view.drag_id) {
                                 m.x += dx;
                                 m.y += dy;
                             }
                         } else if self.view.drag_type == DragType::ResizeMask {
                              if let Some(m) = self.state.masks.iter_mut().find(|m| Some(m.id) == self.view.drag_id) {
                                  match m.mask_type.as_str() {
                                      "scanner" => {
                                          let w = m.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                          let h = m.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                          // Simple resize: add dx/dy to width/height
                                          // Limit min size
                                          m.params.insert("width".to_string(), (w + dx * 2.0).max(0.01).into()); // *2 because centered
                                          m.params.insert("height".to_string(), (h + dy * 2.0).max(0.01).into());
                                      },
                                      "radial" => {
                                          let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                                          m.params.insert("radius".to_string(), (r + dx).max(0.01).into());
                                      },
                                      _ => {}
                                  }
                              }
                         }
                    } else {
                        self.view.offset += delta;
                    }
                }
                
                if response.drag_released() {
                    self.view.drag_id = None;
                    self.view.drag_type = DragType::None;
                    self.save_state();
                }

                // RENDERING
                // Background
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(15, 15, 18));
                
                // Grid (infinite)
                let grid_spacing = 0.1 * rect.width() * self.view.scale;
                if grid_spacing > 5.0 { 
                     // Only draw if dense enough
                }
                
                // Draw bounds (0..1)
                let tl = to_screen(0.0, 0.0, &self.view);
                let br = to_screen(1.0, 1.0, &self.view);
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
                        let angle = s.rotation.to_radians();
                        let _dir = egui::vec2(angle.cos(), angle.sin());
                        
                        // We actually draw the pixels in the Engine loop usually, 
                        // but here we can draw a "ghost" line or the pixels themselves if we have data.
                        // The previous code drew pixels. Let's keep that logic but assume it's below.
                    }
                    
                    // Draw pixels based on simulation data...
                    for i in 0..s.pixel_count {
                        // Calculate world pos of pixel i
                        let angle = s.rotation.to_radians(); 
                        // Note: In engine we use glam, here we use simple math or just replicate
                        let off_x = (i as f32 * s.spacing) * angle.cos();
                        let off_y = (i as f32 * s.spacing) * angle.sin();
                        let px_world = s.x + off_x;
                        let py_world = s.y + off_y;
                        
                        let px_screen = to_screen(px_world, py_world, &self.view);
                        
                        // Color from data
                        let rgb_data = &s.data;
                        let color = if i*3+2 < rgb_data.len() * 3 { 
                             // Wait, rgb_data is Vec<[u8; 3]>. So length is pixel count.
                             // rgb_data[i] gives [u8; 3].
                             if i < rgb_data.len() {
                                 let p = rgb_data[i];
                                 egui::Color32::from_rgb(p[0], p[1], p[2])
                             } else { egui::Color32::GRAY }
                        } else { egui::Color32::GRAY };
                        
                        painter.rect_filled(
                            egui::Rect::from_center_size(px_screen, egui::vec2(4.0, 4.0)),
                            1.0, 
                            color
                        );
                    }
                }
                
                // Masks
                for m in &self.state.masks {
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
                             
                             let rect_size = egui::vec2(
                                 w * rect.width() * self.view.scale,
                                 h * rect.height() * self.view.scale
                             );
                             
                             // Draw Box
                             painter.rect(
                                 egui::Rect::from_center_size(pos, rect_size),
                                 2.0,
                                 color,
                                 egui::Stroke::new(2.0, base_color)
                             );
                             
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

                             let offset_x = (w / 2.0) * osc_val as f32;
                             let bar_pos_world = m.x + offset_x;
                             let bar_screen = to_screen(bar_pos_world, m.y, &self.view);
                             
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
                                  egui::Color32::WHITE
                             };

                             // Bar is a vertical line inside the box
                             painter.line_segment(
                                 [
                                     bar_screen - egui::vec2(0.0, rect_size.y/2.0),
                                     bar_screen + egui::vec2(0.0, rect_size.y/2.0)
                                 ],
                                 egui::Stroke::new(3.0, bar_color)
                             );
                             
                             // Draw Resize Handle (Bottom Right)
                             let handle_pos = to_screen(m.x + w/2.0, m.y + h/2.0, &self.view);
                             painter.circle_filled(handle_pos, 4.0, egui::Color32::WHITE);
                             painter.circle_stroke(handle_pos, 4.0, egui::Stroke::new(1.0, egui::Color32::BLACK));
                         },
                         "radial" => {
                             let r = m.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                             let radius_screen = r * rect.width() * self.view.scale; // Width as basis
                             
                             painter.circle(pos, radius_screen, color, egui::Stroke::new(2.0, stroke_color));
                             
                             // Draw Resize Handle (Right Edge)
                             let handle_pos = to_screen(m.x + r, m.y, &self.view);
                             painter.circle_filled(handle_pos, 4.0, egui::Color32::WHITE);
                             painter.circle_stroke(handle_pos, 4.0, egui::Stroke::new(1.0, egui::Color32::BLACK));
                         },
                         _ => {}
                    }
                }
            });
        });
        
        ctx.request_repaint(); 
    }
}
