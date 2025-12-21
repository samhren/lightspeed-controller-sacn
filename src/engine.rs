use crate::model::{AppState, Mask, PixelStrip, NetworkConfig, GlobalEffect};
use crate::audio::AudioListener;
use sacn::source::SacnSource; 
use std::time::Instant;

use rusty_link::{AblLink, SessionState};

struct SparklePixel {
    strip_id: u64,
    pixel_index: usize,
    birth_time: f32,
    color: [u8; 3],
}

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

    // Audio BPM
    last_tap_time: Option<Instant>,
    tap_intervals: Vec<f64>,
    pub audio_bpm: f64,

    // Audio Snap Phase Tracking
    last_audio_beat_time: Option<Instant>,
    phase_error: f64, // How far off we are from audio beats (in beats)
    phase_correction_rate: f64, // How fast we correct (beats per second)

    // Sparkle effect state tracking
    sparkle_states: Vec<SparklePixel>,
    // Burst effect radius smoothing per-mask
    burst_radius_states: std::collections::HashMap<u64, f32>,
}

impl LightingEngine {
    pub fn new() -> Self {
        let local_addr = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
        let sender = SacnSource::with_ip("Lightspeed", local_addr)
            .unwrap_or_else(|e| {
                log::error!("Failed to create sACN sender: {:?}", e);
                log::warn!("Attempting fallback configuration...");
                // Try with explicit IPv4 any address as fallback
                SacnSource::with_ip("Lightspeed", "0.0.0.0:0".parse().unwrap())
                    .expect("Critical: Cannot initialize network stack")
            });
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
            last_tap_time: None,
            tap_intervals: Vec::new(),
            audio_bpm: 0.0,
            last_audio_beat_time: None,
            phase_error: 0.0,
            phase_correction_rate: 0.5, // Correct half a beat per second when out of sync
            sparkle_states: Vec::new(),
            burst_radius_states: std::collections::HashMap::new(),
        }
    }

    pub fn update(&mut self, state: &mut AppState) {


        // Sync Audio Params from State
        self.latency_ms = state.audio.latency_ms;
        self.use_flywheel = state.audio.use_flywheel;
        self.hybrid_sync = state.audio.hybrid_sync;
        self.audio_sensitivity = state.audio.sensitivity;

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
        let link_peers = self.link.num_peers();

        // Hybrid Sync / Audio logic
        let mut force_snap = false;
        if let Some(audio) = &self.audio_listener {
             // Read Volume (handle poisoned mutex gracefully)
             let vol = audio.current_volume.lock()
                 .map(|v| *v)
                 .unwrap_or_else(|poisoned| {
                     log::warn!("Audio mutex poisoned, recovering");
                     *poisoned.into_inner()
                 });

             // Detect Peak using Sensitivity
             // Sensitivity 0.0 = Need HUGE volume (Threshold 1.0)
             // Sensitivity 1.0 = React to silence (Threshold 0.0)
             // Let's map Sensitivity 0..1 to Threshold 0.5 .. 0.01
             let threshold = 0.5 - (self.audio_sensitivity * 0.45);

             let is_peaking = vol > threshold;

             // Rising Edge Detection
             if is_peaking && !self.was_peaking {
                 // AUDIO HIT!

                 let now_t = Instant::now();

                 // 1. Audio BPM Detection (Tap Tempo)
                 if let Some(last) = self.last_tap_time {
                     let delta = now_t.duration_since(last).as_secs_f64();
                     // Filter reasonable range: 30 BPM (2.0s) to 200 BPM (0.3s)
                     if delta > 0.3 && delta < 2.0 {
                         self.tap_intervals.push(delta);
                         if self.tap_intervals.len() > 8 {
                             self.tap_intervals.remove(0);
                         }

                         // Average with outlier filtering
                         let mut sorted = self.tap_intervals.clone();
                         sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

                         // Use median or trimmed mean for better accuracy
                         let mid = sorted.len() / 2;
                         let avg_interval = if sorted.len() > 3 {
                             // Use middle 50% of values
                             let start = sorted.len() / 4;
                             let end = 3 * sorted.len() / 4;
                             sorted[start..end].iter().sum::<f64>() / (end - start) as f64
                         } else {
                             sorted[mid]
                         };

                         self.audio_bpm = 60.0 / avg_interval;
                     } else if delta > 2.0 {
                         // Reset if too long silence
                         self.tap_intervals.clear();
                     }
                 }
                 self.last_tap_time = Some(now_t);

                 if self.hybrid_sync {
                     // NEW APPROACH: Track expected beat time and calculate phase error

                     // Get current effective BPM
                     let current_bpm = if self.link.num_peers() > 0 {
                         tempo
                     } else if self.audio_bpm > 30.0 {
                         self.audio_bpm
                     } else {
                         120.0 * self.speed as f64
                     };

                     if current_bpm > 30.0 {
                         // Calculate where we SHOULD be based on last confirmed audio beat
                         if let Some(last_audio_time) = self.last_audio_beat_time {
                             let time_since_last = now_t.duration_since(last_audio_time).as_secs_f64();
                             let expected_beats_elapsed = (current_bpm / 60.0) * time_since_last;

                             // Check if this hit is close to a beat boundary
                             let beats_to_nearest = expected_beats_elapsed.fract();
                             let dist_to_next = 1.0 - beats_to_nearest;
                             let dist_to_prev = beats_to_nearest;

                             // Tighter tolerance - only accept hits near actual beats
                             let beat_window = 0.25; // 25% of a beat on either side

                             if dist_to_prev < beat_window || dist_to_next < beat_window {
                                 // This is likely a real beat!

                                 // Calculate phase error: difference between expected and actual
                                 let current_phase = self.flywheel_beat.fract();

                                 // Determine which beat boundary we're snapping to
                                 let target_phase = if dist_to_prev < dist_to_next {
                                     0.0 // Snap to previous beat (just happened)
                                 } else {
                                     1.0 // Snap to next beat (about to happen)
                                 };

                                 // Calculate error (how far off we are)
                                 let error = if target_phase == 0.0 {
                                     -current_phase // We're past 0, need to pull back
                                 } else {
                                     1.0 - current_phase // We're before 1, need to push forward
                                 };

                                 // Store the error for gradual correction
                                 self.phase_error = error;

                                 // Update last confirmed audio beat time
                                 self.last_audio_beat_time = Some(now_t);
                             }
                         } else {
                             // First audio beat detected - initialize
                             self.last_audio_beat_time = Some(now_t);
                             // Snap immediately to nearest beat
                             let current_phase = self.flywheel_beat.fract();
                             if current_phase < 0.5 {
                                 self.flywheel_beat = self.flywheel_beat.floor();
                             } else {
                                 self.flywheel_beat = self.flywheel_beat.ceil();
                             }
                             force_snap = true;
                         }
                     }
                 }
             }
             self.was_peaking = is_peaking;
        }

        // Determine effective tempo
        let effective_tempo = if link_peers > 0 {
             tempo // Link Tempo
        } else if self.audio_bpm > 30.0 {
             self.audio_bpm // Audio Tempo
        } else {
             120.0 * self.speed as f64 // Manual Speed (Multiplier on 120 default?) 
             // Wait, self.speed was "Master Speed" in UI (0.1..5.0).
             // If we treat manual speed as multiplier on 120, that works.
             // Or we can add a base tempo field? For now, 120 * speed.
        };

        // Flywheel Logic (only run if we didn't just hard-snap)
        if !self.use_flywheel && !force_snap {
            self.flywheel_beat = link_beat;
            self.sync_mode = true;
        } else if !force_snap {
            // Predict next beat based on current flywheel + tempo
            // beat = beats/min * min/sec * sec
            // beat_delta = (tempo / 60.0) * dt
            // USE EFFECTIVE TEMPO
            let mut predicted_beat = self.flywheel_beat + (effective_tempo / 60.0) * dt;

            // Apply audio phase correction if hybrid sync is enabled
            if self.hybrid_sync && self.phase_error.abs() > 0.001 {
                // Gradually correct the phase error
                let correction_amount = self.phase_correction_rate * dt;
                let correction_to_apply = if self.phase_error.abs() < correction_amount {
                    self.phase_error // Apply remaining error if small
                } else {
                    self.phase_error.signum() * correction_amount // Apply partial correction
                };

                predicted_beat += correction_to_apply;
                self.phase_error -= correction_to_apply;

                // Decay phase error over time to prevent accumulation
                self.phase_error *= 0.95; // 5% decay per frame
            }

            // Check difference with Link (if available)
            let diff = (link_beat - predicted_beat).abs();

            // Configurable Thresholds
            let error_threshold = 0.5; // If off by more than half a beat, consider it an error (jump)
            let recovery_time = 1.0; // Seconds to wait before snapping (approx 2 beats at 120bpm)

            if diff > error_threshold && link_peers > 0 {
                // Significant deviation from Link
                self.sync_error_timer += dt as f32;
                self.sync_mode = false;

                if self.sync_error_timer > recovery_time {
                    // Snap to link beat
                    self.flywheel_beat = link_beat;
                    self.sync_error_timer = 0.0;
                    self.sync_mode = true;
                    self.phase_error = 0.0; // Reset audio phase error
                } else {
                    // Continue drifting/predicting but invalid sync
                    self.flywheel_beat = predicted_beat;
                }
            } else {
                // Small deviation or no Link - use predicted beat
                self.sync_error_timer = 0.0;
                self.sync_mode = true;

                // If Link is available, gently nudge towards it
                if link_peers > 0 {
                    let lerp_factor = 0.1; // Smooth correction
                    self.flywheel_beat = predicted_beat + (link_beat - predicted_beat) * lerp_factor;
                } else {
                    // No Link - just use predicted beat (audio-driven or manual)
                    self.flywheel_beat = predicted_beat;
                }
            }
        }

        // Use flywheel_beat for animations
        // Safety check: ensure beat is valid (not NaN or infinite)
        let beat = if self.flywheel_beat.is_finite() {
            self.flywheel_beat
        } else {
            log::warn!("Invalid flywheel_beat detected: {}, resetting to 0.0", self.flywheel_beat);
            self.flywheel_beat = 0.0;
            0.0
        };

        // 1. Clear all strips
        for strip in &mut state.strips {
            strip.data = vec![[0, 0, 0]; strip.pixel_count];
        }

        // 2. Apply Scene or fallback to raw masks
        if let Some(sel_id) = state.selected_scene_id {
            if let Some(scene) = state.scenes.iter().find(|s| s.id == sel_id).cloned() {
                match scene.kind.as_str() {
                    "Masks" => {
                        for mask in &scene.masks {
                            self.apply_mask_to_strips(mask, &mut state.strips, t, beat);
                        }
                    }
                    "Global" => {
                        if let Some(effect) = scene.global {
                            self.apply_global_effect(&effect, &mut state.strips, t, beat);
                        }
                    }
                    _ => {
                        for mask in &state.masks {
                            self.apply_mask_to_strips(mask, &mut state.strips, t, beat);
                        }
                    }
                }
            } else {
                // Selected scene not found, fallback
                for mask in &state.masks {
                    self.apply_mask_to_strips(mask, &mut state.strips, t, beat);
                }
            }
        } else {
            // No scene selected: use masks directly
            for mask in &state.masks {
                self.apply_mask_to_strips(mask, &mut state.strips, t, beat);
            }
        }

        // 3. Send to sACN
        // Coalesce data by universe
        let mut universe_data: std::collections::HashMap<u16, Vec<u8>> = std::collections::HashMap::new();
        
        let global_universe_offset = state.network.universe.saturating_sub(1);

        for strip in &state.strips {
             // specific strip universe + global offset (clamped to valid sACN range 1-63999)
             let u = strip.universe.saturating_add(global_universe_offset).min(63999).max(1);

             // sACN allows multiple strips in one universe if channels don't overlap
             let start = (strip.start_channel as usize).saturating_sub(1);
             
             // Ensure we have a buffer (512 bytes for DMX)
             let entry = universe_data.entry(u).or_insert_with(|| vec![0; 512]);
             
             for (i, pixel) in strip.data.iter().enumerate() {
                 let idx = start + i * 3;
                 // Bounds check: ensure idx, idx+1, idx+2 are all valid
                 if let Some(max_idx) = idx.checked_add(2) {
                     if max_idx < entry.len() {
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
        }
    
        
        // Debug: Log color data before sending
        static mut LAST_COLOR_LOG: f32 = 0.0;

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
            // let _ = self.sender.send(&[u], &data, Some(priority), dst_ip, None);
            let mut fixed_data = vec![0u8]; // Start Code
            fixed_data.extend_from_slice(&data);

            match self.sender.send(&[u], &fixed_data, Some(200), dst_ip, None) {
                Ok(_) => {
                    // Success, verbose logging might flood
                }
                Err(e) => {
                    println!("sACN Error sending to U{} (Dest: {:?}): {:?}", u, dst_ip, e);
                }
            }
        }
    }

    fn apply_mask_to_strips(&mut self, mask: &Mask, strips: &mut [PixelStrip], t: f32, beat: f64) {
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
            // Scanner Mask: A rectangular region with a scanning bar that sweeps back and forth
            // Get mask dimensions in local (unrotated) space
            let base_width = mask.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let base_height = mask.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let width = apply_lfo_modulation(base_width, &mask.params, "width", t, beat);
            let height = apply_lfo_modulation(base_height, &mask.params, "height", t, beat);
            // Debug: when true, fill all pixels inside mask with white
            let debug_fill = mask.params.get("debug_fill").and_then(|v| v.as_bool()).unwrap_or(false);

            // Get mask rotation
            let rotation_deg = mask.params.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            let rot_rad = rotation_deg.to_radians();
            let cos_rot = rot_rad.cos();
            let sin_rot = rot_rad.sin();



            // Get bar parameters
            let base_bar_width = mask.params.get("bar_width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
            let bar_width = apply_lfo_modulation(base_bar_width, &mask.params, "bar_width", t, beat);
            let hard_edge = mask.params.get("hard_edge").and_then(|v| v.as_bool()).unwrap_or(false);

            // Calculate bar position (scanning animation)
            let is_sync = mask.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(false);
            let phase = if is_sync {
                let rate_str = mask.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4");
                let divisor = match rate_str {
                    "4 Bar" => 16.0, "2 Bar" => 8.0, "1 Bar" => 4.0,
                    "1/2" => 2.0, "1/4" => 1.0, "1/8" => 0.5, _ => 1.0,
                };
                let start_pos = mask.params.get("start_pos").and_then(|v| v.as_str()).unwrap_or("Center");
                let offset = match start_pos {
                    "Right" => 0.25, "Left" => 0.75, _ => 0.0,
                };
                (beat / divisor + offset) * std::f64::consts::PI * 2.0
            } else {
                let speed = mask.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                (t * speed * self.speed) as f64
            };

            let motion = mask.params.get("motion").and_then(|v| v.as_str()).unwrap_or("Smooth");
            let osc_val = if motion == "Linear" {
                (2.0 / std::f64::consts::PI) * (phase.sin().asin())
            } else {
                phase.sin()
            };

            // Bar position in local space
            // Sweep the BAR CENTER within ±(width/2 - bar_width) so that
            // the bar's EDGES can exactly reach the mask edges without relying
            // on osc_val hitting perfect ±1.0. This prevents a dark sliver at
            // the mask boundaries, especially noticeable when rotated.
            let sweep_range = (width / 2.0) - bar_width;
            let bar_local_x = sweep_range * osc_val as f32;

            // Debug bar position - DETAILED
            static mut LAST_LOG_TIME: f32 = 0.0;
            let should_log_detailed = unsafe {
                if t - LAST_LOG_TIME > 0.5 { // Log every 0.5 seconds
                    LAST_LOG_TIME = t;
                    true
                } else {
                    false
                }
            };

            

            // Get color
            let m_color = mask.params.get("color").and_then(|v| {
                let arr = v.as_array()?;
                Some([arr.get(0)?.as_u64()? as u8, arr.get(1)?.as_u64()? as u8, arr.get(2)?.as_u64()? as u8])
            }).unwrap_or([0, 255, 255]);
            let final_color = get_color(m_color);

            // Process each strip
            for i in 0..strips.len() {
                let strip = &mut strips[i];
                let pixel_limit = strip.pixel_count.min(strip.data.len());

                for p in 0..pixel_limit {
                    // 1. Calculate pixel position in world space
                    let local_pos_x = if strip.flipped {
                        ((strip.pixel_count - 1).saturating_sub(p)) as f32 * strip.spacing
                    } else {
                        p as f32 * strip.spacing
                    };
                    let px = strip.x + local_pos_x;
                    let py = strip.y;

                    // 2. Transform to mask's local coordinate system
                    let dx = px - mx;
                    let dy = py - my;
                    let mask_local_x = dx * cos_rot + dy * sin_rot;
                    let mask_local_y = -dx * sin_rot + dy * cos_rot;

                    // 3. Check if pixel is within mask bounds (rectangular boundary)
                    let half_w = width / 2.0;
                    let half_h = height / 2.0;

                    // Add small epsilon for floating point tolerance
                    const EPSILON: f32 = 0.0001;

                    // Debug: Log pixels that SHOULD light up at extremes
                    if should_log_detailed && i == 0 {
                        let passes_bounds = (mask_local_x >= -(half_w + EPSILON) && mask_local_x <= (half_w + EPSILON)) &&
                                    (mask_local_y >= -(half_h + EPSILON) && mask_local_y <= (half_h + EPSILON));
                        let dist_to_bar = (mask_local_x - bar_local_x).abs();
                        let in_bar = dist_to_bar <= bar_width;

                        // Log pixels near mask edges
                        let near_left_edge = mask_local_x < -half_w + 0.3;
                        let near_right_edge = mask_local_x > half_w - 0.3;

                    }

                    if (mask_local_x >= -(half_w + EPSILON) && mask_local_x <= (half_w + EPSILON)) &&
                       (mask_local_y >= -(half_h + EPSILON) && mask_local_y <= (half_h + EPSILON)) {

                        if debug_fill {
                            // Visualization: show everything the mask considers "inside"
                            strip.data[p] = [255, 255, 255];
                            continue;
                        }

                        // 4. Check if pixel is hit by the scanning bar
                        let dist_to_bar = (mask_local_x - bar_local_x).abs();

                        if dist_to_bar <= bar_width {
                            // Pixel is inside mask AND hit by bar
                            let intensity = if hard_edge {
                                1.0
                            } else {
                                (1.0 - dist_to_bar / bar_width).max(0.0)
                            };

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
            }
        } else if mask.mask_type == "radial" {
             let base_radius = mask.params.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
             let radius = apply_lfo_modulation(base_radius, &mask.params, "radius", t, beat);
             let debug_fill = mask.params.get("debug_fill").and_then(|v| v.as_bool()).unwrap_or(false);
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

                let pixel_limit = strip.pixel_count.min(strip.data.len());
                for i in 0..pixel_limit {
                    let local_x = start_idx_x + (i as f32 * strip.spacing);
                    let local_y = 0.0;
                    
                    let (px, py) = if strip.flipped {
                         (strip.x - local_x, strip.y)
                    } else {
                         (strip.x + local_x, strip.y)
                    };

                    let dist = ((px - mx).powi(2) + (py - my).powi(2)).sqrt();
                    if dist < radius {
                         if debug_fill {
                             strip.data[i] = [255, 255, 255];
                             continue;
                         }
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
        } else if mask.mask_type == "burst" {
            // Burst Mask: Audio-reactive radial mask that grows/shrinks with music
            let base_radius = mask.params.get("base_radius").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
            let max_radius = mask.params.get("max_radius").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            let sensitivity = mask.params.get("sensitivity").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            let decay = mask.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(0.05) as f32;

            let color = mask.params.get("color").and_then(|v| {
                let arr = v.as_array()?;
                Some([arr.get(0)?.as_u64()? as u8, arr.get(1)?.as_u64()? as u8, arr.get(2)?.as_u64()? as u8])
            }).unwrap_or([255, 100, 0]);

            // Get audio volume
            let audio_vol = if let Some(audio) = &self.audio_listener {
                audio.current_volume.lock().map(|v| *v).unwrap_or(0.0)
            } else {
                0.0
            };

            // Calculate target radius
            let expansion = (audio_vol * sensitivity).min(1.0);
            let target_radius = base_radius + (max_radius - base_radius) * expansion;

            // Smooth to target
            let current_radius = self.burst_radius_states.entry(mask.id).or_insert(base_radius);
            *current_radius = *current_radius + (target_radius - *current_radius) * decay;

            let mx = mask.x;
            let my = mask.y;

            // Render like radial mask
            for strip in strips.iter_mut() {
                let pixel_count = strip.pixel_count.min(strip.data.len());
                for i in 0..pixel_count {
                    let local_x = if strip.flipped {
                         ((strip.pixel_count - 1).saturating_sub(i)) as f32 * strip.spacing
                    } else {
                         i as f32 * strip.spacing
                    };
                    let px = strip.x + local_x;
                    let py = strip.y;

                    let dist = ((px - mx).powi(2) + (py - my).powi(2)).sqrt();
                    if dist < *current_radius {
                        let intensity = (1.0 - dist / *current_radius).clamp(0.0, 1.0);

                        let r = (color[0] as f32 * intensity) as u8;
                        let g = (color[1] as f32 * intensity) as u8;
                        let b = (color[2] as f32 * intensity) as u8;

                        strip.data[i] = [
                            strip.data[i][0].saturating_add(r),
                            strip.data[i][1].saturating_add(g),
                            strip.data[i][2].saturating_add(b),
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
    
    pub fn get_sync_info(&self) -> (String, f64) {
        let peers = self.link.num_peers();
        if peers > 0 {
             let mut session_state = SessionState::new();
             self.link.capture_app_session_state(&mut session_state);
             (format!("LINK ({} Peers)", peers), session_state.tempo())
        } else if self.audio_bpm > 30.0 {
             ("AUDIO".to_string(), self.audio_bpm)
        } else {
             ("MANUAL".to_string(), 120.0 * self.speed as f64)
        }
    }
}

impl LightingEngine {
    fn apply_global_effect(&mut self, effect: &GlobalEffect, strips: &mut [PixelStrip], t: f32, beat: f64) {
        match effect.kind.as_str() {
            "Solid" => {
                // Use EXACT same color reading as masks
                let color = effect.params.get("color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8, arr.get(1)?.as_u64()? as u8, arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([255, 255, 255]);

                // Apply color EXACTLY like scanner masks do - with intensity and saturating_add
                for s in strips.iter_mut() {
                    let cnt = s.pixel_count.min(s.data.len());
                    for i in 0..cnt {
                        let intensity = 1.0; // Full intensity for solid colors
                        let r = (color[0] as f32 * intensity) as u8;
                        let g = (color[1] as f32 * intensity) as u8;
                        let b = (color[2] as f32 * intensity) as u8;

                        let curr = s.data[i];
                        s.data[i] = [
                            curr[0].saturating_add(r),
                            curr[1].saturating_add(g),
                            curr[2].saturating_add(b)
                        ];
                    }
                }
            }
            "Rainbow" => {
                let base_speed = effect.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
                let speed = apply_lfo_modulation(base_speed, &effect.params, "speed", t, beat);
                let hue = (t * speed * self.speed).fract();
                let c = hsv_to_rgb(hue, 1.0, 1.0);
                for s in strips.iter_mut() {
                    let cnt = s.pixel_count.min(s.data.len());
                    for i in 0..cnt { s.data[i] = c; }
                }
            }
            "Flash" => {
                // Use EXACT same color reading as masks
                let color = effect.params.get("color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8, arr.get(1)?.as_u64()? as u8, arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([255, 255, 255]);

                let rate_str = effect.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1 Bar");
                let divisor = match rate_str {
                    "4 Bar" => 16.0, "1 Bar" => 4.0, "1/2" => 2.0, "1/4" => 1.0, "1/8" => 0.5, _ => 4.0,
                };

                let decay = effect.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(5.0);

                // Calculate phase 0..1
                let phase = (beat / divisor).fract();

                // Exponential decay: starts at 1.0, drops quickly
                // To make it flash *on the beat*, we want peak at phase=0.
                let intensity = (1.0 - phase).powf(decay) as f32;

                // Clamp to ensure valid range
                let intensity = intensity.clamp(0.0, 1.0);

                // Always apply the color with intensity - don't black out
                // This prevents the "crash to black" issue
                for s in strips.iter_mut() {
                    let cnt = s.pixel_count.min(s.data.len());
                    for i in 0..cnt {
                        let r = (color[0] as f32 * intensity) as u8;
                        let g = (color[1] as f32 * intensity) as u8;
                        let b = (color[2] as f32 * intensity) as u8;
                        s.data[i] = [r, g, b];
                    }
                }
            }
            "Sparkle" => {
                let density = effect.params.get("density").and_then(|v| v.as_f64()).unwrap_or(0.05) as f32;
                let life = effect.params.get("life").and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
                let decay = effect.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(5.0);
                let color = effect.params.get("color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8, arr.get(1)?.as_u64()? as u8, arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([255, 255, 255]);

                const MAX_SPARKLES: usize = 500;

                // Spawn new sparkles
                if self.sparkle_states.len() < MAX_SPARKLES {
                    for strip in strips.iter() {
                        let pixel_count = strip.pixel_count.min(strip.data.len());
                        for i in 0..pixel_count {
                            if self.sparkle_states.len() >= MAX_SPARKLES {
                                break;
                            }
                            if rand::random::<f32>() < density {
                                self.sparkle_states.push(SparklePixel {
                                    strip_id: strip.id,
                                    pixel_index: i,
                                    birth_time: t,
                                    color,
                                });
                            }
                        }
                    }
                }

                // Render and cleanup sparkles
                self.sparkle_states.retain(|sparkle| {
                    let age = t - sparkle.birth_time;
                    if age > life {
                        return false;
                    }

                    if let Some(strip) = strips.iter_mut().find(|s| s.id == sparkle.strip_id) {
                        if sparkle.pixel_index < strip.data.len() {
                            let progress = age / life;
                            let intensity = (1.0 - progress).powf(decay as f32).clamp(0.0, 1.0);

                            let r = (sparkle.color[0] as f32 * intensity) as u8;
                            let g = (sparkle.color[1] as f32 * intensity) as u8;
                            let b = (sparkle.color[2] as f32 * intensity) as u8;

                            strip.data[sparkle.pixel_index] = [
                                strip.data[sparkle.pixel_index][0].saturating_add(r),
                                strip.data[sparkle.pixel_index][1].saturating_add(g),
                                strip.data[sparkle.pixel_index][2].saturating_add(b),
                            ];
                        }
                    }

                    true
                });
            }
            _ => {}
        }
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

/// Apply LFO modulation to a parameter value
fn apply_lfo_modulation(
    base_value: f32,
    params: &std::collections::HashMap<String, serde_json::Value>,
    param_name: &str,
    t: f32,
    beat: f64,
) -> f32 {
    let lfo_key = |suffix: &str| format!("{}_lfo_{}", param_name, suffix);

    let enabled = params.get(&lfo_key("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !enabled {
        return base_value;
    }

    let depth = params.get(&lfo_key("depth"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5) as f32;

    let waveform = params.get(&lfo_key("waveform"))
        .and_then(|v| v.as_str())
        .unwrap_or("sine");

    let is_sync = params.get(&lfo_key("sync"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let phase = if is_sync {
        let rate_str = params.get(&lfo_key("rate"))
            .and_then(|v| v.as_str())
            .unwrap_or("1/4");

        let divisor = match rate_str {
            "4 Bar" => 16.0, "2 Bar" => 8.0, "1 Bar" => 4.0,
            "1/2" => 2.0, "1/4" => 1.0, "1/8" => 0.5,
            _ => 1.0,
        };

        (beat / divisor).fract() as f32
    } else {
        let hz = params.get(&lfo_key("hz"))
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0) as f32;
        (t * hz).fract()
    };

    let wave_value = match waveform {
        "sine" => (phase * std::f32::consts::TAU).sin(),
        "triangle" => {
            let tri = if phase < 0.5 { phase * 2.0 } else { 2.0 - phase * 2.0 };
            tri * 2.0 - 1.0
        },
        "sawtooth" => phase * 2.0 - 1.0,
        _ => 0.0,
    };

    let modulation = wave_value * depth;
    base_value * (1.0 + modulation)
}
