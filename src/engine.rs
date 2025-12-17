use crate::model::{AppState, Mask, PixelStrip, NetworkConfig};
use crate::audio::AudioListener;
use sacn::source::SacnSource; 
use std::time::Instant;

use rusty_link::{AblLink, SessionState};

pub struct LightingEngine {
    sender: SacnSource,
    link: AblLink,
    registered_universes: std::collections::HashSet<u16>,
    bind_ip: Option<String>,
    pub speed: f32,
    pub latency_ms: f32,
    pub use_flywheel: bool,
    pub hybrid_sync: bool, 
    pub audio_sensitivity: f32,
    audio_listener: Option<AudioListener>,
    was_peaking: bool, // For edge detection
    pub current_beat: u8, // 1, 2, 3, 4
    start_time: Instant,
    last_network: NetworkConfig,
    flywheel_beat: f64,
    last_update: std::time::Instant,
    sync_error_timer: f32, // How long we've been out of sync
    sync_mode: bool, // true if locked, false if drifting/error
}

impl LightingEngine {
    pub fn new() -> Self {
        let local_addr = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
        let sender = SacnSource::with_ip("Lightspeed", local_addr).unwrap();
        // Start ensuring multicast send works? 
        // sacn crate defaults fine usually.
        
        // sender.set_unicast_destinations(...) if needed
        let link = AblLink::new(120.0);
        link.enable(true);
        
        Self {
            sender,
            link,
            registered_universes: std::collections::HashSet::new(),
            bind_ip: None,
            speed: 1.0,
            latency_ms: 0.0,
            use_flywheel: true,
            hybrid_sync: false,
            audio_sensitivity: 0.5,
            audio_listener: AudioListener::new(), // Try to init
            was_peaking: false,
            current_beat: 1,
            start_time: Instant::now(),
            last_network: NetworkConfig::default(),
            flywheel_beat: 0.0,
            last_update: Instant::now(),
            sync_error_timer: 0.0,
            sync_mode: true,
        }
    }

    pub fn update(&mut self, state: &mut AppState) {


        let now = Instant::now();
        let dt = now.duration_since(self.last_update).as_secs_f64();
        self.last_update = now;
        let t = self.start_time.elapsed().as_secs_f32();
        
        // Capture Link Beat
        let mut session_state = SessionState::new();
        self.link.capture_app_session_state(&mut session_state);
        let link_micros = self.link.clock_micros();
        // Adjust for latency: Visuals are ahead, so we need to query 'earlier' time? 
        // No, if visuals are ahead (Beat 2.0 displayed when Audio is 1.9), 
        // we want to display Beat 1.9.
        // Beat increases with time. 
        // If we subtract delay from 'now', we get a smaller time -> smaller beat. 
        // Correct.
        let adjusted_micros = (link_micros as i64 - (self.latency_ms * 1000.0) as i64).max(0) as u64;
        
        let link_beat = session_state.beat_at_time(adjusted_micros as i64, 1.0);
        let phase = session_state.phase_at_time(adjusted_micros as i64, 4.0); // Quantum 4 for bars
        self.current_beat = (phase.floor() as u8 % 4) + 1;
        
        let tempo = session_state.tempo();
        
        // Hybrid Sync / Audio logic
        let mut force_snap = false;
        if let Some(audio) = &self.audio_listener {
             // Read Volume
             let vol = *audio.current_volume.lock().unwrap();
             
             // Detect Peak using Sensitivity
             // Sensitivity 0.0 = Need HUGE volume (Threshold 1.0)
             // Sensitivity 1.0 = React to silence (Threshold 0.0)
             // Let's map Sensitivity 0..1 to Threshold 0.5 .. 0.01 
             let threshold = 0.5 - (self.audio_sensitivity * 0.45);
             
             let is_peaking = vol > threshold;
             
             // Rising Edge Detection
             if is_peaking && !self.was_peaking {
                 // AUDIO HIT!
                 if self.hybrid_sync {
                     // Check if we are close to a beat?
                     // If we are at 1.9, and hit comes, snap to 2.0.
                     // If we are at 2.1, snap to 2.0.
                     let current_phase = self.flywheel_beat.fract(); 
                     // fract() is 0.0 to 0.999.
                     // Near beat means near 0.0 or near 1.0.
                     
                     let dist_to_next = 1.0 - current_phase;
                     let dist_to_prev = current_phase;
                     
                     let snap_tolerance = 0.35; // Broad window (hit must be roughly near beat)
                     
                     if dist_to_prev < snap_tolerance {
                         // We are just past the beat (e.g. 2.1). Snap back to 2.0
                         let target = self.flywheel_beat.floor();
                         self.flywheel_beat = target;
                         force_snap = true;
                     } else if dist_to_next < snap_tolerance {
                         // We are nearing the beat (e.g. 1.9). Snap fwd to 2.0
                         let target = self.flywheel_beat.ceil();
                         self.flywheel_beat = target;
                         force_snap = true;
                     }
                 }
             }
             self.was_peaking = is_peaking;
        }

        // Flywheel Logic (only run if we didn't just hard-snap)
        if !self.use_flywheel && !force_snap {
            self.flywheel_beat = link_beat;
            self.sync_mode = true;
        } else if !force_snap {
            // Predict next beat based on current flywheel + tempo
            // beat = beats/min * min/sec * sec
            // beat_delta = (tempo / 60.0) * dt
            let predicted_beat = self.flywheel_beat + (tempo / 60.0) * dt;
            
            // Check difference
            let diff = (link_beat - predicted_beat).abs();
            
            // Configurable Thresholds
            let error_threshold = 0.5; // If off by more than half a beat, consider it an error (jump)
            let recovery_time = 1.0; // Seconds to wait before snapping (approx 2 beats at 120bpm)
            
            if diff > error_threshold {
                // Significant deviation
                self.sync_error_timer += dt as f32;
                self.sync_mode = false;
                
                if self.sync_error_timer > recovery_time {
                    // Snap to link beat
                    self.flywheel_beat = link_beat;
                    self.sync_error_timer = 0.0;
                    self.sync_mode = true;
                } else {
                    // Continue drifting/predicting but invalid sync
                    self.flywheel_beat = predicted_beat;
                }
            } else {
                // Small deviation - Nudge towards link beat
                self.sync_error_timer = 0.0;
                self.sync_mode = true;
                let lerp_factor = 0.1; // Smooth correction
                self.flywheel_beat = predicted_beat + (link_beat - predicted_beat) * lerp_factor;
            }
        }

        // Use flywheel_beat for animations
        let beat = self.flywheel_beat;

        // 1. Clear all strips
        for strip in &mut state.strips {
            strip.data = vec![[0, 0, 0]; strip.pixel_count];
        }

        // 2. Apply Masks
        for mask in &state.masks {
            self.apply_mask_to_strips(mask, &mut state.strips, t, beat);
        }

        // 3. Send to sACN
        // Coalesce data by universe
        let mut universe_data: std::collections::HashMap<u16, Vec<u8>> = std::collections::HashMap::new();
        
        let global_universe_offset = state.network.universe.saturating_sub(1);

        for strip in &state.strips {
             // specific strip universe + global offset
             let u = strip.universe.saturating_add(global_universe_offset);
             
             // sACN allows multiple strips in one universe if channels don't overlap
             let start = (strip.start_channel as usize).saturating_sub(1);
             
             // Ensure we have a buffer (512 bytes for DMX)
             let entry = universe_data.entry(u).or_insert_with(|| vec![0; 512]);
             
             for (i, pixel) in strip.data.iter().enumerate() {
                 let idx = start + i * 3;
                 if idx + 2 < entry.len() {
                     match strip.color_order.as_str() {
                         "GRB" => {
                             entry[idx] = pixel[1];   // G
                             entry[idx+1] = pixel[0]; // R
                             entry[idx+2] = pixel[2]; // B
                         },
                         "BGR" => {
                             entry[idx] = pixel[2];   // B
                             entry[idx+1] = pixel[1]; // G
                             entry[idx+2] = pixel[0]; // R
                         },
                         _ => { // RGB
                             entry[idx] = pixel[0];   // R
                             entry[idx+1] = pixel[1]; // G
                             entry[idx+2] = pixel[2]; // B
                         }
                     }
                 }
             }
        }
        
        // Send Coalesced Universes
        if beat.fract() < 0.05 { // Rough throttle
             // println!("Sending sACN... Universes: {}", universe_data.len());
        }
        
        for (u, data) in universe_data {
            if !self.registered_universes.contains(&u) {
                match self.sender.register_universe(u) {
                    Ok(_) => {
                        self.registered_universes.insert(u);
                        println!("Registered sACN Universe {}", u);
                    },
                    Err(e) => {
                        println!("Failed to register sACN Universe {}: {:?}", u, e);
                    }
                }
            }

            let priority = 100; // Default priority
            let dst_ip: Option<std::net::SocketAddr> = if state.network.use_multicast {
                None
            } else {
                if let Ok(ip) = state.network.unicast_ip.parse::<std::net::IpAddr>() {
                    Some(std::net::SocketAddr::new(ip, 5568))
                } else {
                    None // Fallback
                }
            };
            
            // Only send if we have a valid config (if Unicast was selected but invalid IP, we might SKIP or fall back)
            // User code implies we should try to send. 
            // If !multicast and invalid IP -> dst_ip is None -> Sends Multicast? 
            // Let's explicitly check:
            if !state.network.use_multicast && dst_ip.is_none() {
                // Invalid Unicast IP, skip or log
                continue;
            }

            // DEBUG: Print once per second if needed, or just print errors
            // let _ = self.sender.send(&[u], &data, Some(priority), dst_ip, None);
            match self.sender.send(&[u], &data, Some(priority), dst_ip, None) {
                Ok(_) => {
                    // Success, verbose logging might flood
                }
                Err(e) => {
                    println!("sACN Error sending to U{} (Dest: {:?}): {:?}", u, dst_ip, e);
                }
            }
        }
    }

    fn apply_mask_to_strips(&self, mask: &Mask, strips: &mut [PixelStrip], t: f32, beat: f64) {
        let mx = mask.x;
        let my = mask.y;
        
        let mode = mask.params.get("color_mode").and_then(|v| v.as_str()).unwrap_or("static");
        let speed = mask.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;

        // Helper to get color based on mode
        let get_color = |base_color: [u8; 3]| -> [u8; 3] {
            if mode == "rainbow" {
                let hue = (t * speed * 0.5) % 1.0; // 0.0 to 1.0
                hsv_to_rgb(hue, 1.0, 1.0)
            } else if mode == "gradient" {
                let colors: Vec<[u8; 3]> = mask.params.get("gradient_colors").and_then(|v| {
                    serde_json::from_value(v.clone()).ok()
                }).unwrap_or_else(|| {
                    // Fallback
                    let c1 = mask.params.get("color").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([0, 255, 255]);
                    let c2 = mask.params.get("color2").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or([255, 0, 255]);
                    vec![c1, c2]
                });
                
                if colors.is_empty() { return base_color; }
                if colors.len() == 1 { return colors[0]; }

                // Determine progress (0.0 to 1.0)
                // Use the same phase logic as position? Or separate? 
                // Position phase is calculated below based on sync/speed.
                // We should probably share that phase calculation if possible, or recalculate it.
                // Re-calculating here for simplicity as we don't have 'phase' variable yet.
                // WAIT: 'phase' is calculated inside scanner block. But 'get_color' helper is defined before it.
                // Let's defer color calculation until after phase is known? 
                // BUT 'apply_mask_to_strips' structure defines 'get_color' then uses it.
                // Let's use 't' and 'beat' here to calc independent color phase if needed, 
                // OR ideally, move 'phase' calc up.
                
                // Let's move phase calc up? Width/Height are specific to Scanner, but phase could be general (Radial uses it too for pulse?).
                // For now, let's duplicate the Sync check phase logic here for color cycle.
                
                let is_sync = mask.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(false);
                let progress = if is_sync {
                     let rate_str = mask.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4");
                     let divisor = match rate_str {
                         "4 Bar" => 16.0, "2 Bar" => 8.0, "1 Bar" => 4.0, "1/2" => 2.0, "1/4" => 1.0, "1/8" => 0.5, _ => 1.0,
                     };
                     // Phase 0..1
                     (beat / divisor).fract()
                } else {
                     // User said "take same amount of time per color".
                     // If speed=1, cycle 1hz.
                     (t * speed).fract() as f64
                };

                // Cycle logic: c1->c2->c3->c1
                let n = colors.len();
                let scaled = progress * n as f64;
                let idx = scaled.floor() as usize;
                let sub_t = scaled.fract() as f32;
                
                let c_start = colors[idx % n];
                let c_end = colors[(idx + 1) % n];
                
                [
                    (c_start[0] as f32 * (1.0 - sub_t) + c_end[0] as f32 * sub_t) as u8,
                    (c_start[1] as f32 * (1.0 - sub_t) + c_end[1] as f32 * sub_t) as u8,
                    (c_start[2] as f32 * (1.0 - sub_t) + c_end[2] as f32 * sub_t) as u8,
                ]
            } else {
                base_color
            }
        };

        if mask.mask_type == "scanner" {
            // Scanner Logic
            let width = mask.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let height = mask.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let _thickness = mask.params.get("thickness").and_then(|v| v.as_f64()).unwrap_or(0.05) as f32;
            let m_color = mask.params.get("color").and_then(|v| {
                let arr = v.as_array()?;
                Some([
                    arr.get(0)?.as_u64()? as u8,
                    arr.get(1)?.as_u64()? as u8,
                    arr.get(2)?.as_u64()? as u8
                ])
            }).unwrap_or([0, 255, 255]);
            
            let final_color = get_color(m_color);

            // Animation Logic (Sync vs Free)
            let is_sync = mask.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(false);
            
            let phase = if is_sync {
                let rate_str = mask.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4");
                let divisor = match rate_str {
                    "4 Bar" => 16.0,
                    "2 Bar" => 8.0,
                    "1 Bar" => 4.0,
                    "1/2" => 2.0,
                    "1/4" => 1.0, 
                    "1/8" => 0.5,
                    _ => 1.0,
                };
                let start_pos = mask.params.get("start_pos").and_then(|v| v.as_str()).unwrap_or("Center");
                let offset = match start_pos {
                    "Right" => 0.25,
                    "Left" => 0.75,
                    _ => 0.0, // Center
                };
                (beat / divisor + offset) * std::f64::consts::PI * 2.0
            } else {
                (t * speed * self.speed) as f64
            };

            // Motion Easing
            let motion = mask.params.get("motion").and_then(|v| v.as_str()).unwrap_or("Smooth");
            let osc_val = if motion == "Linear" {
                // Triangle wave: 2/PI * asin(sin(phase))
                // Result is -1.0 to 1.0
                (2.0 / std::f64::consts::PI) * (phase.sin().asin())
            } else {
                // Smooth (Sine)
                phase.sin()
            };

            let offset_x = (width / 2.0) * osc_val as f32;
            let _bar_center_x = mx + offset_x;
            
            let bar_width = mask.params.get("bar_width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
            
            // Mask Rotation
            let rotation_deg = mask.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            let rot_rad = rotation_deg.to_radians();
            let cos_rot = rot_rad.cos();
            let sin_rot = rot_rad.sin();

            for i in 0..strips.len() {
                let strip = &mut strips[i];
                let cos_r = strip.rotation.cos();
                let sin_r = strip.rotation.sin();

                for p in 0..strip.pixel_count {
                    // Calculate pixel position
                    let local_x = p as f32 * strip.spacing;
                    let local_y = 0.0;
                    
                    let rx = local_x * cos_r - local_y * sin_r;
                    let ry = local_x * sin_r + local_y * cos_r;
                    
                    let px = strip.x + rx;
                    let py = strip.y + ry;
                    
                    // Transform pixel into Mask's Local Space (Centered at 0, aligned with Mask Rotation)
                    // Translate to origin
                    let dx_world = px - mx;
                    let dy_world = py - my;
                    
                    // Rotate by -rot (Inverse of Mask Rotation)
                    // x' = x cos(-t) - y sin(-t) = x cos t + y sin t
                    // y' = x sin(-t) + y cos(-t) = -x sin t + y cos t
                    let mask_local_x = dx_world * cos_rot + dy_world * sin_rot;
                    let mask_local_y = -dx_world * sin_rot + dy_world * cos_rot;
                    
                    // Now check bounds in Local Space
                    // Width applies to Local X (Scan Axis), Height applies to Local Y
                    if mask_local_x >= -width/2.0 && mask_local_x <= width/2.0 &&
                       mask_local_y >= -height/2.0 && mask_local_y <= height/2.0 {
                           
                        // Bar movement is along local X
                        // Bar center relative to Mask Center (mx)
                        // offset_x is already relative to center
                        let bar_center_local_x = offset_x;
                        
                        let dist_to_bar = (mask_local_x - bar_center_local_x).abs();
                        let max_dist = bar_width; 
                        let mut intensity = (1.0 - dist_to_bar / max_dist).max(0.0);
                        
                        let hard_edge = mask.params.get("hard_edge").and_then(|v| v.as_bool()).unwrap_or(false);
                        if hard_edge {
                             intensity = if intensity > 0.0 { 1.0 } else { 0.0 };
                        }
                        
                        if intensity > 0.0 {
                             let r = (final_color[0] as f32 * intensity) as u8;
                             let g = (final_color[1] as f32 * intensity) as u8;
                             let b = (final_color[2] as f32 * intensity) as u8;
                             
                             let curr = strip.data[p];
                             strip.data[p] = [
                                 curr[0].saturating_add(r),
                                 curr[1].saturating_add(g),
                                 curr[2].saturating_add(b)
                             ];
                        }
                    }
                }
            }
        } else if mask.mask_type == "radial" {
             let radius = mask.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
             let m_color = mask.params.get("color").and_then(|v| {
                let arr = v.as_array()?;
                Some([
                    arr.get(0)?.as_u64()? as u8,
                    arr.get(1)?.as_u64()? as u8,
                    arr.get(2)?.as_u64()? as u8
                ])
            }).unwrap_or([255, 0, 0]);
            
            let final_color = get_color(m_color);

             for strip in strips.iter_mut() {
                // ALIGNMENT FIX: Start at 0
                let start_idx_x = 0.0; 
                
                let cos_r = strip.rotation.cos();
                let sin_r = strip.rotation.sin();

                for i in 0..strip.pixel_count {
                    let local_x = start_idx_x + (i as f32 * strip.spacing);
                    let local_y = 0.0;
                    let px = strip.x + (local_x * cos_r - local_y * sin_r);
                    let py = strip.y + (local_x * sin_r + local_y * cos_r);

                    let dist = ((px - mx).powi(2) + (py - my).powi(2)).sqrt();
                    if dist < radius {
                         let intensity = 1.0 - (dist / radius);
                         let intensity = intensity.clamp(0.0, 1.0);
                         
                         let [r, g, b] = strip.data[i];
                         strip.data[i] = [
                              r.saturating_add((final_color[0] as f32 * intensity) as u8),
                              g.saturating_add((final_color[1] as f32 * intensity) as u8),
                              b.saturating_add((final_color[2] as f32 * intensity) as u8),
                         ];
                    }
                 }
              }
        }
    }

    pub fn get_bpm(&self) -> f64 {
        let mut session_state = SessionState::new();
        self.link.capture_app_session_state(&mut session_state);
        session_state.tempo()
    }

    pub fn get_beat(&self) -> f64 {
        if self.use_flywheel {
            self.flywheel_beat
        } else {
            // Need to capture fresh or store last raw beat?
            // self.flywheel logic already captures raw beat in update.
            // But update is called once per frame.
            // Let's store raw_beat in struct or just assume flywheel_beat is kept in sync if disabled?
            // Better: update() logic should set flywheel_beat = link_beat if disabled.
            self.flywheel_beat 
        }
    }
    
    pub fn get_time(&self) -> f32 {
        self.start_time.elapsed().as_secs_f32()
    }
}

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h_i = (h * 6.0) as i32;
    let f = h * 6.0 - h_i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    
    let (r, g, b) = match h_i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}
