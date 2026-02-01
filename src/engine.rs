use crate::model::{AppState, Mask, PixelStrip, NetworkConfig, GlobalEffect};
use crate::audio::AudioListener;
use sacn::source::SacnSource;
use std::time::Instant;
use log::{info, debug, warn, error};

use rusty_link::{AblLink, SessionState};

struct SparklePixel {
    strip_id: u64,
    pixel_index: usize,
    birth_time: f32,
    color: [u8; 3],
}

struct PulseState {
    strip_id: u64,
    position: f32,      // Current head position in pixels
    last_update: f32,   // Time of last update
}

struct GlitchPixel {
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

    // Audio Snap Phase Tracking (PLL-style)
    last_audio_beat_time: Option<Instant>,
    phase_offset: f64,           // Accumulated phase offset from audio sync
    last_onset_time: Option<Instant>,  // For minimum interval enforcement

    // Sparkle effect state tracking
    sparkle_states: Vec<SparklePixel>,
    // Pulse Wave effect state tracking
    pulse_states: Vec<PulseState>,
    // Glitch Sparkle effect state tracking
    glitch_states: Vec<GlitchPixel>,
    glitch_sparkle_accumulator: f32,
    // Burst effect radius smoothing per-mask
    burst_radius_states: std::collections::HashMap<u64, f32>,
}

impl LightingEngine {
    pub fn new() -> Self {
        info!("[LIGHTS] Initializing sACN (E1.31) network stack...");

        let local_addr = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
        debug!("[LIGHTS] Binding to address: {}", local_addr);

        let sender = SacnSource::with_ip("Lightspeed", local_addr)
            .unwrap_or_else(|e| {
                error!("[LIGHTS] Failed to create sACN sender: {:?}", e);
                warn!("[LIGHTS] Attempting fallback configuration...");
                // Try with explicit IPv4 any address as fallback
                SacnSource::with_ip("Lightspeed", "0.0.0.0:0".parse().unwrap())
                    .expect("Critical: Cannot initialize network stack")
            });

        info!("[LIGHTS] sACN sender initialized successfully");
        debug!("[LIGHTS] Source name: 'Lightspeed', ready for multicast/unicast");

        let link = AblLink::new(120.0);
        link.enable(true);
        info!("[LIGHTS] Ableton Link enabled at 120 BPM");
        
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
            phase_offset: 0.0,
            last_onset_time: None,
            sparkle_states: Vec::new(),
            pulse_states: Vec::new(),
            glitch_states: Vec::new(),
            glitch_sparkle_accumulator: 0.0,
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
            // Use the new onset detection system
            let (is_onset, onset_strength, vol) = if let Ok(state) = audio.audio_state.lock() {
                (state.is_onset, state.onset_strength, state.current_volume)
            } else {
                // Fallback to legacy volume-based detection
                let vol = audio.current_volume.lock()
                    .map(|v| *v)
                    .unwrap_or(0.0);
                let threshold = 0.5 - (self.audio_sensitivity * 0.45);
                let is_peak = vol > threshold && !self.was_peaking;
                (is_peak, if is_peak { 1.0 } else { 0.0 }, vol)
            };

            // Apply sensitivity threshold to onset strength
            let sensitivity_threshold = 0.5 - (self.audio_sensitivity * 0.45);
            let beat_detected = is_onset && onset_strength > sensitivity_threshold;

            if beat_detected {
                let now_t = Instant::now();

                // Enforce minimum interval between detected beats (prevents double triggers)
                // 200ms minimum = 300 BPM max, keeps it conservative
                let min_interval_ok = self.last_onset_time
                    .map(|t| now_t.duration_since(t).as_secs_f64() > 0.2)
                    .unwrap_or(true);

                if min_interval_ok {
                    self.last_onset_time = Some(now_t);

                    // 1. Audio BPM Detection (improved tap tempo)
                    if let Some(last) = self.last_tap_time {
                        let delta = now_t.duration_since(last).as_secs_f64();
                        // Filter reasonable range: 40 BPM (1.5s) to 200 BPM (0.3s)
                        if delta > 0.3 && delta < 1.5 {
                            self.tap_intervals.push(delta);
                            if self.tap_intervals.len() > 12 {
                                self.tap_intervals.remove(0);
                            }

                            // Calculate BPM using weighted median (recent beats weighted higher)
                            if self.tap_intervals.len() >= 2 {
                                let mut weighted: Vec<(f64, f64)> = self.tap_intervals
                                    .iter()
                                    .enumerate()
                                    .map(|(i, &interval)| {
                                        // More recent = higher weight
                                        let weight = (i + 1) as f64;
                                        (interval, weight)
                                    })
                                    .collect();

                                weighted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

                                // Find weighted median
                                let total_weight: f64 = weighted.iter().map(|(_, w)| w).sum();
                                let mut cumulative = 0.0;
                                let mut avg_interval = weighted[0].0;
                                for (interval, weight) in &weighted {
                                    cumulative += weight;
                                    if cumulative >= total_weight / 2.0 {
                                        avg_interval = *interval;
                                        break;
                                    }
                                }

                                self.audio_bpm = 60.0 / avg_interval;
                            }
                        } else if delta > 2.0 {
                            // Reset if too long silence
                            self.tap_intervals.clear();
                        }
                    }
                    self.last_tap_time = Some(now_t);

                    // 2. Phase correction for hybrid sync
                    if self.hybrid_sync {
                        // Get current effective BPM
                        let current_bpm = if self.link.num_peers() > 0 {
                            tempo
                        } else if self.audio_bpm > 30.0 {
                            self.audio_bpm
                        } else {
                            120.0 * self.speed as f64
                        };

                        // Only do phase correction if we have a stable tempo estimate
                        // (at least 4 consistent beat intervals)
                        let have_stable_tempo = self.tap_intervals.len() >= 4;

                        if current_bpm > 30.0 && have_stable_tempo {
                            // Check if this beat is near where we expect it
                            // based on the established tempo
                            let expected_interval = 60.0 / current_bpm;
                            let actual_interval = self.last_audio_beat_time
                                .map(|t| now_t.duration_since(t).as_secs_f64())
                                .unwrap_or(expected_interval);

                            // Only apply correction if beat is within 30% of expected timing
                            let timing_ratio = actual_interval / expected_interval;
                            let is_on_beat = timing_ratio > 0.7 && timing_ratio < 1.3;

                            if is_on_beat {
                                // Current beat phase (0.0 to 1.0)
                                let current_phase = (self.flywheel_beat + self.phase_offset).fract();

                                // Audio beat should be at phase 0.0
                                // Calculate shortest distance to phase 0
                                let phase_error = if current_phase < 0.5 {
                                    -current_phase  // We're slightly past the beat, pull back
                                } else {
                                    1.0 - current_phase  // We're before the beat, push forward
                                };

                                // Gentle correction - only correct 40% per beat
                                let correction = phase_error * 0.4;
                                self.phase_offset += correction;
                            }

                            self.last_audio_beat_time = Some(now_t);
                        } else if !have_stable_tempo {
                            // Still building up tempo estimate, just track time
                            self.last_audio_beat_time = Some(now_t);
                        }
                    }
                }
            }

            self.was_peaking = vol > (0.5 - self.audio_sensitivity * 0.45);
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
            let predicted_beat = self.flywheel_beat + (effective_tempo / 60.0) * dt;

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
                    self.phase_offset = 0.0; // Reset audio phase offset
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

        // Gradually decay phase offset when not receiving audio beats
        // This prevents permanent drift if audio stops
        if self.hybrid_sync {
            let decay_rate = 0.02; // Decay 2% per frame
            self.phase_offset *= 1.0 - decay_rate;
        }

        // Use flywheel_beat + phase_offset for animations
        // Safety check: ensure beat is valid (not NaN or infinite)
        let raw_beat = self.flywheel_beat + self.phase_offset;
        let beat = if raw_beat.is_finite() {
            raw_beat
        } else {
            log::warn!("Invalid beat detected: {}, resetting", raw_beat);
            self.flywheel_beat = 0.0;
            self.phase_offset = 0.0;
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
                        for config in &scene.global_effects {
                             self.apply_global_effect(&config.effect, &mut state.strips, t, beat, config.targets.as_ref());
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
                        info!("[LIGHTS] Registered sACN Universe {}", u);
                    },
                    Err(e) => {
                        error!("[LIGHTS] Failed to register sACN Universe {}: {:?}", u, e);
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
                    // Success - use trace level to avoid flooding logs
                }
                Err(e) => {
                    warn!("[LIGHTS] sACN send error on Universe {} (Dest: {:?}): {:?}", u, dst_ip, e);
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

            let unidirectional = mask.params.get("unidirectional").and_then(|v| v.as_bool()).unwrap_or(false);
            let motion = mask.params.get("motion").and_then(|v| v.as_str()).unwrap_or("Smooth");
            
            let osc_val = if unidirectional {
                 // Sawtooth wave: -1.0 to 1.0
                 let norm_phase = (phase / (std::f64::consts::PI * 2.0)).fract();
                 // fract() returns [0, 1) (if positive). phase is usually positive (t*speed or beat).
                 // We want strictly 0..1 then map to -1..1
                 let p = if norm_phase < 0.0 { norm_phase + 1.0 } else { norm_phase };
                 p * 2.0 - 1.0
            } else if motion == "Linear" {
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
        } else if mask.mask_type == "orbit" {
            // Orbit Mask: A bar that traces around the perimeter of a rectangle
            // Goes: top (left→right) → right (top→bottom) → bottom (right→left) → left (bottom→top)
            let width = mask.params.get("width").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let height = mask.params.get("height").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let bar_width = mask.params.get("bar_width").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
            let hard_edge = mask.params.get("hard_edge").and_then(|v| v.as_bool()).unwrap_or(false);
            let constant_speed = mask.params.get("constant_speed").and_then(|v| v.as_bool()).unwrap_or(false);

            // Calculate raw phase (0 to 1 for one full orbit)
            let is_sync = mask.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(false);
            let raw_phase = if is_sync {
                let rate_str = mask.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4");
                let divisor = match rate_str {
                    "4 Bar" => 16.0, "2 Bar" => 8.0, "1 Bar" => 4.0,
                    "1/2" => 2.0, "1/4" => 1.0, "1/8" => 0.5, _ => 1.0,
                };
                beat / divisor
            } else {
                let speed = mask.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                (t * speed * self.speed / 4.0) as f64 // Divide by 4 to normalize
            };

            let half_w = width / 2.0;
            let half_h = height / 2.0;

            // Calculate side and progress based on constant_speed setting
            let (side, side_progress): (u32, f32) = if constant_speed {
                // Constant speed: bar moves at same speed on all sides
                // Each side still starts on the beat, but shorter sides finish early and pause
                let phase = (raw_phase * 4.0).rem_euclid(4.0);
                let current_side = phase.floor() as u32;
                let beat_progress = phase.fract() as f32; // 0..1 within current beat

                // The longest side takes the full beat, shorter sides finish early
                let max_side = width.max(height);
                let current_side_length = match current_side {
                    0 | 2 => width,  // Top/bottom edges
                    _ => height,     // Left/right edges
                };

                // How much of the beat this side needs (relative to longest side)
                let side_duration_ratio = current_side_length / max_side;

                // Calculate progress: if beat_progress exceeds side_duration_ratio, return -1.0 to hide bar
                let progress = if beat_progress >= side_duration_ratio {
                    -1.0 // Finished, hide bar until next beat
                } else {
                    beat_progress / side_duration_ratio // Scale progress to 0..1
                };

                (current_side, progress)
            } else {
                // Equal time per side (original behavior)
                let phase = (raw_phase * 4.0).rem_euclid(4.0);
                (phase.floor() as u32, phase.fract() as f32)
            };

            // Calculate bar center position based on which side we're on
            // The bar is always perpendicular to the direction of travel
            let (bar_center_x, bar_center_y, is_horizontal) = match side {
                0 => {
                    // Top edge: moving left to right, bar is vertical
                    let x = -half_w + side_progress * width;
                    (x, -half_h, false)
                }
                1 => {
                    // Right edge: moving top to bottom, bar is horizontal
                    let y = -half_h + side_progress * height;
                    (half_w, y, true)
                }
                2 => {
                    // Bottom edge: moving right to left, bar is vertical
                    let x = half_w - side_progress * width;
                    (x, half_h, false)
                }
                _ => {
                    // Left edge: moving bottom to top, bar is horizontal
                    let y = half_h - side_progress * height;
                    (-half_w, y, true)
                }
            };

            // Only render if bar is visible (side_progress >= 0, otherwise waiting for next beat)
            if side_progress >= 0.0 {
                // Get color
                let m_color = mask.params.get("color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8, arr.get(1)?.as_u64()? as u8, arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([0, 255, 255]);
                let final_color = get_color(m_color);

                // Process each strip
                for strip in strips.iter_mut() {
                    let pixel_limit = strip.pixel_count.min(strip.data.len());

                    for p in 0..pixel_limit {
                        // Calculate pixel position in world space
                        let local_pos_x = if strip.flipped {
                            ((strip.pixel_count - 1).saturating_sub(p)) as f32 * strip.spacing
                        } else {
                            p as f32 * strip.spacing
                        };
                        let px = strip.x + local_pos_x;
                        let py = strip.y;

                        // Transform to mask's local coordinate system (no rotation for orbit)
                        let mask_local_x = px - mx;
                        let mask_local_y = py - my;

                        // Check if pixel is within mask bounds
                        const EPSILON: f32 = 0.0001;
                        if (mask_local_x >= -(half_w + EPSILON) && mask_local_x <= (half_w + EPSILON)) &&
                           (mask_local_y >= -(half_h + EPSILON) && mask_local_y <= (half_h + EPSILON)) {

                            // Calculate distance to bar based on bar orientation
                            let dist_to_bar = if is_horizontal {
                                // Bar is horizontal (on left/right edges) - check Y distance
                                (mask_local_y - bar_center_y).abs()
                            } else {
                                // Bar is vertical (on top/bottom edges) - check X distance
                                (mask_local_x - bar_center_x).abs()
                            };

                            if dist_to_bar <= bar_width {
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
        // Include phase offset for audio sync
        self.flywheel_beat + self.phase_offset
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
    fn apply_global_effect(&mut self, effect: &GlobalEffect, strips: &mut [PixelStrip], t: f32, beat: f64, targets: Option<&Vec<u64>>) {
        match effect.kind.as_str() {
            "Solid" => {
                // Use EXACT same color reading as masks
                let color = effect.params.get("color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8, arr.get(1)?.as_u64()? as u8, arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([255, 255, 255]);

                // Apply color EXACTLY like scanner masks do - with intensity and saturating_add
                for s in strips.iter_mut() {
                    if let Some(t) = targets { if !t.contains(&s.id) { continue; } }
                    
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
                    if let Some(t) = targets { if !t.contains(&s.id) { continue; } }
                    
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
                    if let Some(t) = targets { if !t.contains(&s.id) { continue; } }
                    
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
                        if let Some(t) = targets { if !t.contains(&strip.id) { continue; } }
                        
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
                    // Filter: Only process sparkles belonging to targeted strips of THIS effect
                    if let Some(t) = targets { if !t.contains(&sparkle.strip_id) { return true; } }
                    
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
            "ColorWash" => {
                // Parse parameters
                let color_a = effect.params.get("color_a").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8,
                          arr.get(1)?.as_u64()? as u8,
                          arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([255, 0, 0]);

                let color_b = effect.params.get("color_b").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8,
                          arr.get(1)?.as_u64()? as u8,
                          arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([0, 0, 255]);

                let sync_to_beat = effect.params.get("sync_to_beat").and_then(|v| v.as_bool()).unwrap_or(false);
                let rate_str = effect.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1 Bar");
                let period = effect.params.get("period").and_then(|v| v.as_f64()).unwrap_or(4.0);

                // Calculate phase (0.0 to 1.0)
                let phase = if sync_to_beat {
                    let divisor = match rate_str {
                        "4 Bar" => 16.0,
                        "2 Bar" => 8.0,
                        "1 Bar" => 4.0,
                        "1/2" => 2.0,
                        "1/4" => 1.0,
                        "1/8" => 0.5,
                        _ => 1.0,
                    };
                    (beat / divisor).fract()
                } else {
                    (t as f64 / period).fract()
                };

                // Apply sine wave for smooth oscillation
                let sine_phase = ((phase * 2.0 * std::f64::consts::PI).sin() + 1.0) / 2.0;

                // Linear interpolation between color_a and color_b
                let r = (color_a[0] as f64 * (1.0 - sine_phase) + color_b[0] as f64 * sine_phase) as u8;
                let g = (color_a[1] as f64 * (1.0 - sine_phase) + color_b[1] as f64 * sine_phase) as u8;
                let b = (color_a[2] as f64 * (1.0 - sine_phase) + color_b[2] as f64 * sine_phase) as u8;

                // Apply to all targeted strips
                for strip in strips.iter_mut() {
                    if let Some(t) = targets {
                        if !t.contains(&strip.id) {
                            continue;
                        }
                    }

                    for pixel in &mut strip.data {
                        *pixel = [r, g, b];
                    }
                }
            }
            "GlitchSparkle" => {
                // Parse parameters
                let background_color = effect.params.get("background_color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8,
                          arr.get(1)?.as_u64()? as u8,
                          arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([0, 0, 0]);

                let sparkle_color = effect.params.get("sparkle_color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8,
                          arr.get(1)?.as_u64()? as u8,
                          arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([255, 255, 255]);

                let density = effect.params.get("density").and_then(|v| v.as_f64()).unwrap_or(0.05) as f32;
                let fade_time = effect.params.get("fade_time").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
                let decay = effect.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(5.0);

                const MAX_GLITCH_SPARKLES: usize = 500;

                // Step 1: Fill background color on all targeted strips
                for strip in strips.iter_mut() {
                    if let Some(t) = targets {
                        if !t.contains(&strip.id) {
                            continue;
                        }
                    }

                    for pixel in &mut strip.data {
                        *pixel = background_color;
                    }
                }

                // Step 2: Spawn new sparkles using accumulator for constant rate
                if self.glitch_states.len() < MAX_GLITCH_SPARKLES {
                    // Count total pixels in targeted strips
                    let mut total_pixels = 0;
                    let mut eligible_pixels = Vec::new();

                    for strip in strips.iter() {
                        if let Some(t) = targets {
                            if !t.contains(&strip.id) {
                                continue;
                            }
                        }

                        let pixel_count = strip.pixel_count.min(strip.data.len());
                        for i in 0..pixel_count {
                            eligible_pixels.push((strip.id, i));
                            total_pixels += 1;
                        }
                    }

                    // Calculate expected sparkles this frame and accumulate
                    // Density represents target percentage of pixels sparkling at any time
                    // Adjust spawn rate based on fade_time to maintain constant coverage
                    let target_coverage = total_pixels as f32 * density;
                    let spawn_rate_per_second = target_coverage / fade_time.max(0.1);
                    let fps_estimate = 60.0; // Assume 60fps for spawn rate calculation
                    let expected_sparkles = spawn_rate_per_second / fps_estimate;
                    self.glitch_sparkle_accumulator += expected_sparkles;

                    // Spawn whole number of sparkles, keep fractional part
                    let sparkles_to_spawn = self.glitch_sparkle_accumulator.floor() as usize;
                    self.glitch_sparkle_accumulator -= sparkles_to_spawn as f32;

                    // Spawn sparkles at random positions
                    for _ in 0..sparkles_to_spawn.min(MAX_GLITCH_SPARKLES - self.glitch_states.len()) {
                        if eligible_pixels.is_empty() {
                            break;
                        }

                        // Pick a random pixel
                        let idx = (rand::random::<f32>() * eligible_pixels.len() as f32) as usize % eligible_pixels.len();
                        let (strip_id, pixel_index) = eligible_pixels[idx];

                        self.glitch_states.push(GlitchPixel {
                            strip_id,
                            pixel_index,
                            birth_time: t,
                            color: sparkle_color,
                        });
                    }
                }

                // Step 3: Render and cleanup sparkles
                self.glitch_states.retain(|sparkle| {
                    // Filter by target strips
                    if let Some(t) = targets {
                        if !t.contains(&sparkle.strip_id) {
                            return true; // Keep but don't render
                        }
                    }

                    // Check lifetime
                    let age = t - sparkle.birth_time;
                    if age > fade_time {
                        return false; // Remove dead sparkles
                    }

                    // Render to strip
                    if let Some(strip) = strips.iter_mut().find(|s| s.id == sparkle.strip_id) {
                        if sparkle.pixel_index < strip.data.len() {
                            let progress = age / fade_time;
                            let intensity = (1.0 - progress).powf(decay as f32).clamp(0.0, 1.0);

                            let r = (sparkle.color[0] as f32 * intensity) as u8;
                            let g = (sparkle.color[1] as f32 * intensity) as u8;
                            let b = (sparkle.color[2] as f32 * intensity) as u8;

                            // Additive blending on top of background
                            strip.data[sparkle.pixel_index] = [
                                strip.data[sparkle.pixel_index][0].saturating_add(r),
                                strip.data[sparkle.pixel_index][1].saturating_add(g),
                                strip.data[sparkle.pixel_index][2].saturating_add(b),
                            ];
                        }
                    }

                    true // Keep sparkle alive
                });
            }
            "PulseWave" => {
                // Parse parameters
                let color = effect.params.get("color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8,
                          arr.get(1)?.as_u64()? as u8,
                          arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([255, 255, 255]);

                let sync = effect.params.get("sync").and_then(|v| v.as_bool()).unwrap_or(true);
                let rate_str = effect.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4");
                let manual_speed = effect.params.get("speed").and_then(|v| v.as_f64()).unwrap_or(5.0);
                let tail_length = effect.params.get("tail_length").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
                let decay = effect.params.get("decay").and_then(|v| v.as_f64()).unwrap_or(2.0) as f32;
                let direction = effect.params.get("direction").and_then(|v| v.as_str()).unwrap_or("Forward");

                // Calculate speed (pixels per second)
                let speed = if sync {
                    // Beat-synced: use beat phase to calculate position
                    let divisor = match rate_str {
                        "4 Bar" => 16.0,
                        "2 Bar" => 8.0,
                        "1 Bar" => 4.0,
                        "1/2" => 2.0,
                        "1/4" => 1.0,
                        "1/8" => 0.5,
                        _ => 1.0,
                    };
                    // Speed to complete one cycle per divisor
                    10.0 * divisor // Tuned so it looks good
                } else {
                    manual_speed
                } as f32;

                // Collect strip info and update positions
                let mut strip_positions: Vec<(u64, usize, f32)> = Vec::new();

                for strip in strips.iter() {
                    if let Some(t) = targets {
                        if !t.contains(&strip.id) {
                            continue;
                        }
                    }

                    // Find or create pulse state for this strip
                    let pulse_state = self.pulse_states.iter_mut().find(|p| p.strip_id == strip.id);

                    let position = if let Some(state) = pulse_state {
                        // Update existing position
                        let dt = t - state.last_update;
                        state.last_update = t;
                        state.position += speed * dt;

                        // Handle wrapping/bouncing based on direction
                        let strip_len = strip.pixel_count as f32;
                        match direction {
                            "Reverse" => {
                                state.position = state.position % strip_len;
                                strip_len - state.position
                            }
                            "Bounce" => {
                                let cycle_len = strip_len * 2.0;
                                let pos_in_cycle = state.position % cycle_len;
                                if pos_in_cycle < strip_len {
                                    pos_in_cycle
                                } else {
                                    cycle_len - pos_in_cycle
                                }
                            }
                            _ => { // "Forward"
                                state.position = state.position % strip_len;
                                state.position
                            }
                        }
                    } else {
                        // Create new pulse state
                        self.pulse_states.push(PulseState {
                            strip_id: strip.id,
                            position: 0.0,
                            last_update: t,
                        });
                        0.0
                    };

                    strip_positions.push((strip.id, strip.pixel_count, position));
                }

                // Now render pulses to strips
                for (strip_id, pixel_count, position) in strip_positions {
                    if let Some(strip_mut) = strips.iter_mut().find(|s| s.id == strip_id) {
                        for i in 0..pixel_count {
                            let pixel_pos = i as f32;
                            let distance = (pixel_pos - position).abs();

                            if distance < tail_length {
                                let intensity = (1.0 - distance / tail_length).powf(decay).clamp(0.0, 1.0);
                                let r = (color[0] as f32 * intensity) as u8;
                                let g = (color[1] as f32 * intensity) as u8;
                                let b = (color[2] as f32 * intensity) as u8;

                                if i < strip_mut.data.len() {
                                    strip_mut.data[i] = [
                                        strip_mut.data[i][0].saturating_add(r),
                                        strip_mut.data[i][1].saturating_add(g),
                                        strip_mut.data[i][2].saturating_add(b),
                                    ];
                                }
                            }
                        }
                    }
                }
            }
            "ZoneAlternate" => {
                // Parse parameters
                let group_a: Vec<u64> = effect.params.get("group_a_strips")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let group_b: Vec<u64> = effect.params.get("group_b_strips")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let group_a_color = effect.params.get("group_a_color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8,
                          arr.get(1)?.as_u64()? as u8,
                          arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([255, 0, 0]);

                let group_b_color = effect.params.get("group_b_color").and_then(|v| {
                    let arr = v.as_array()?;
                    Some([arr.get(0)?.as_u64()? as u8,
                          arr.get(1)?.as_u64()? as u8,
                          arr.get(2)?.as_u64()? as u8])
                }).unwrap_or([0, 0, 255]);

                let rate_str = effect.params.get("rate").and_then(|v| v.as_str()).unwrap_or("1/4");
                let mode = effect.params.get("mode").and_then(|v| v.as_str()).unwrap_or("Swap");

                // Calculate beat phase and determine active group
                let divisor = match rate_str {
                    "4 Bar" => 16.0,
                    "2 Bar" => 8.0,
                    "1 Bar" => 4.0,
                    "1/2" => 2.0,
                    "1/4" => 1.0,
                    "1/8" => 0.5,
                    _ => 1.0,
                };
                let phase = (beat / divisor).fract();
                let a_is_active = phase < 0.5;

                // Determine colors based on mode
                let (color_when_a_active, color_when_b_active) = match mode {
                    "Pulse" => {
                        // Pulse mode: active gets color, inactive gets black
                        if a_is_active {
                            (group_a_color, [0, 0, 0])
                        } else {
                            ([0, 0, 0], group_b_color)
                        }
                    }
                    _ => { // "Swap" mode (default)
                        // Swap mode: groups trade colors
                        if a_is_active {
                            (group_a_color, group_b_color)
                        } else {
                            (group_b_color, group_a_color)
                        }
                    }
                };

                // Apply colors to strips based on group membership
                for strip in strips.iter_mut() {
                    if let Some(t) = targets {
                        if !t.contains(&strip.id) {
                            continue;
                        }
                    }

                    let color = if group_a.contains(&strip.id) {
                        color_when_a_active
                    } else if group_b.contains(&strip.id) {
                        color_when_b_active
                    } else {
                        continue; // Skip strips not in either group
                    };

                    for pixel in &mut strip.data {
                        *pixel = color;
                    }
                }
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
