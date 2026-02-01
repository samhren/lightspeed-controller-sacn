use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use log::{info, debug, warn};

/// Audio state shared between the audio callback and the engine
pub struct AudioState {
    pub current_volume: f32,      // Smoothed RMS for UI display
    pub onset_strength: f32,      // How strong is the current onset (0-1)
    pub is_onset: bool,           // True on the frame an onset is detected
    prev_rms: f32,                // For onset detection
    onset_cooldown: u32,          // Prevent double-triggers
}

impl Default for AudioState {
    fn default() -> Self {
        Self {
            current_volume: 0.0,
            onset_strength: 0.0,
            is_onset: false,
            prev_rms: 0.0,
            onset_cooldown: 0,
        }
    }
}

pub struct AudioListener {
    _stream: cpal::Stream, // Keep stream alive
    pub peak_detected: Arc<AtomicBool>,
    pub current_volume: Arc<Mutex<f32>>,
    pub audio_state: Arc<Mutex<AudioState>>,
}

impl AudioListener {
    pub fn new() -> Option<Self> {
        debug!("[AUDIO] Initializing audio input...");

        let host = cpal::default_host();
        debug!("[AUDIO] Using host: {:?}", host.id());

        let device = match host.default_input_device() {
            Some(d) => {
                if let Ok(name) = d.name() {
                    info!("[AUDIO] Using input device: {}", name);
                }
                d
            }
            None => {
                warn!("[AUDIO] No audio input device found");
                return None;
            }
        };

        let config = match device.default_input_config() {
            Ok(c) => {
                debug!("[AUDIO] Config: {} Hz, {} channels, {:?}",
                    c.sample_rate().0, c.channels(), c.sample_format());
                c
            }
            Err(e) => {
                warn!("[AUDIO] Failed to get input config: {:?}", e);
                return None;
            }
        };

        let peak_flag = Arc::new(AtomicBool::new(false));
        let volume_level = Arc::new(Mutex::new(0.0));
        let audio_state = Arc::new(Mutex::new(AudioState::default()));

        let peak_clone = peak_flag.clone();
        let vol_clone = volume_level.clone();
        let state_clone = audio_state.clone();

        // Get sample rate for cooldown calculation
        let sample_rate = config.sample_rate().0;

        let err_fn = |err| eprintln!("Audio stream error: {}", err);

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                match device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _: &_| check_audio(data, &peak_clone, &vol_clone, &state_clone, sample_rate),
                    err_fn
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("[AUDIO] Failed to build input stream: {:?}", e);
                        return None;
                    }
                }
            },
            other => {
                warn!("[AUDIO] Unsupported sample format: {:?}", other);
                return None;
            }
        };

        if let Err(e) = stream.play() {
            warn!("[AUDIO] Failed to start audio stream: {:?}", e);
            return None;
        }

        info!("[AUDIO] Audio input initialized successfully");

        Some(Self {
            _stream: stream,
            peak_detected: peak_flag,
            current_volume: volume_level,
            audio_state,
        })
    }
}

fn check_audio(
    data: &[f32],
    peak_flag: &Arc<AtomicBool>,
    vol_lock: &Arc<Mutex<f32>>,
    state_lock: &Arc<Mutex<AudioState>>,
    sample_rate: u32,
) {
    if data.is_empty() {
        return;
    }

    // Calculate RMS
    let sum_squares: f32 = data.iter().map(|&s| s * s).sum();
    let rms = (sum_squares / data.len() as f32).sqrt();

    // Update legacy volume for backward compatibility
    if let Ok(mut v) = vol_lock.try_lock() {
        // Less aggressive smoothing for more responsive display
        *v = (*v * 0.7) + (rms * 0.3);
    }

    // Update audio state with onset detection
    if let Ok(mut state) = state_lock.try_lock() {
        // Smooth volume for UI (less aggressive decay)
        state.current_volume = state.current_volume * 0.7 + rms * 0.3;

        // Decrement cooldown
        if state.onset_cooldown > 0 {
            state.onset_cooldown = state.onset_cooldown.saturating_sub(data.len() as u32);
        }

        // Simple but robust onset detection
        // Only trigger on significant volume increases
        let rms_delta = rms - state.prev_rms;

        // Require BOTH absolute volume AND significant rise
        let is_loud_enough = rms > 0.08;
        let is_rising = rms_delta > 0.02;

        // Onset strength based on how much louder than before
        let onset_strength = if is_loud_enough && is_rising {
            (rms_delta * 3.0).min(1.0)
        } else {
            0.0
        };

        state.onset_strength = onset_strength;

        // Longer cooldown: ~120ms to prevent double triggers (max ~500 BPM)
        let min_cooldown_samples = sample_rate * 12 / 100; // 120ms

        // Detect onset: must be loud, rising, and not in cooldown
        state.is_onset = onset_strength > 0.1 && state.onset_cooldown == 0;

        if state.is_onset {
            state.onset_cooldown = min_cooldown_samples;
        }

        state.prev_rms = rms;
    }

    // Legacy peak detection (keeping for compatibility)
    if rms > 0.05 {
        peak_flag.store(true, Ordering::Relaxed);
    } else {
        peak_flag.store(false, Ordering::Relaxed);
    }
}
