use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct AudioListener {
    _stream: cpal::Stream, // Keep stream alive
    pub peak_detected: Arc<AtomicBool>,
    pub current_volume: Arc<Mutex<f32>>,
}

impl AudioListener {
    pub fn new() -> Option<Self> {
        let host = cpal::default_host();
        let device = host.default_input_device()?;
        let config = device.default_input_config().ok()?;

        let peak_flag = Arc::new(AtomicBool::new(false));
        let volume_level = Arc::new(Mutex::new(0.0));

        let peak_clone = peak_flag.clone();
        let vol_clone = volume_level.clone();

        let err_fn = |err| eprintln!("Audio stream error: {}", err);

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _: &_| check_audio(data, &peak_clone, &vol_clone),
                err_fn
            ).ok()?,
            _ => return None, // Only support F32 for simplicity right now
        };

        stream.play().ok()?;

        Some(Self {
            _stream: stream,
            peak_detected: peak_flag,
            current_volume: volume_level,
        })
    }
}

fn check_audio(data: &[f32], peak_flag: &Arc<AtomicBool>, vol_lock: &Arc<Mutex<f32>>) {
    // 1. Calc RMS (Volume)
    let mut sum_squares = 0.0;
    for &sample in data {
        sum_squares += sample * sample;
    }
    let rms = (sum_squares / data.len() as f32).sqrt();

    // Update volume for UI
    if let Ok(mut v) = vol_lock.try_lock() {
        // Smooth decay for visual
        *v = (*v * 0.9) + (rms * 0.1); 
    }

    // 2. Transient Detection (Simple Threshold)
    // In a real robust system we'd use flux/onset detection.
    // For now, if RMS > 0.1 (adjustable later) and we weren't just peaking...
    // Actually, Engine handles the Logic. We just report loud moments?
    // Let's implement a basic "schmitt trigger" or just report raw loudness?
    
    // Better: Reporting Peak only if it rises sharply?
    // Let's keep it simple: Just report "Is Loud". Engine checks rising edge.
    // Normalized approx check.
    if rms > 0.05 {
        peak_flag.store(true, Ordering::Relaxed);
    } else {
        peak_flag.store(false, Ordering::Relaxed);
    }
}
