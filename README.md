# Lightspeed Controller

A high-performance, Rust-based LED lighting controller featuring dynamic scenes, mask-based visualizers, global effects, audio reactivity, and sACN output.

## Overview

Lightspeed Controller is a professional-grade lighting control application designed for managing complex LED strip installations. Create intricate lighting scenes using layered geometric masks, apply global effects across your entire setup, and react to audio in real-time. With persistent storage, MIDI integration, and granular per-strip control, Lightspeed offers a complete solution for live lighting performance and installation work.

## Features

### Scene Management
- **Multiple Scenes**: Create, name, and switch between different lighting configurations
- **Scene Types**:
    - **Masks**: Compositional scenes using geometric masks to reveal or hide underlying patterns
    - **Global**: Apply effects directly to strips without masking
- **Instant Switching**: Seamlessly transition between scenes during performance

### Dynamic Visualizers
- **Scanner Mask**: Scanning bar effect with configurable width, speed, and motion easing
  - Motion types: Sine, Triangle, Sawtooth/Unidirectional
  - Adjustable width and speed parameters
- **Radial Mask**: Expanding/contracting circular pulse effects
- **Linear Mask**: Standard linear gradients and wipe effects
- **LFO Modulation**: Modulate parameters (width, height, speed) using Low Frequency Oscillators for evolving, dynamic looks

### Global Effects
- **Per-Strip Targeting**: Apply effects to all strips or specific subsets
  - Create complex multi-zone looks
  - Different colors and patterns on separate fixtures
- **Stackable Effects**: Layer multiple effects within a single scene
- **Effect Types**:
    - **Solid**: Static color fills
    - **Rainbow**: Scrolling rainbow gradients
    - **Flash**: Strobe/flash effects with adjustable speed
    - **Sparkle**: Randomized sparkle pixels with density and decay controls

### Audio Reactivity
- **Live Audio Input**: React to music and sound in real-time
- **Audio-driven Modulation**: Sync lighting effects to audio levels and beats

### Integration & Control
- **Hex Code Input**: Copy/paste hex color codes (e.g., `#FF00FF`) directly into the color picker
- **MIDI Support**: Trigger scenes using Launchpad or other generic MIDI controllers
- **Ableton Link**: Sync with DAWs and other music software for tempo-locked performances
- **Database Persistence**: All configurations automatically saved to local SQLite database

### Visual Canvas
- **Interactive Layout**: Drag and drop LED strips and masks
- **Pan & Zoom**: Navigate complex setups with right-click drag and scroll wheel
- **Visual Feedback**: See your lighting design in real-time

## Installation

### Prerequisites
- [Rust & Cargo](https://rustup.rs/) (latest stable version)
- macOS, Linux, or Windows

### Build & Run
```bash
# Clone the repository
git clone https://github.com/samhren/lightspeed-controller-sacn.git
cd lightspeed-controller-sacn

# Run in release mode for optimal performance
cargo run --release
```

For development:
```bash
cargo run
```

## Usage

### Basic Workflow
1. **Add LED Strips**: Define your physical LED strip layout on the canvas
2. **Configure Output**: Set up sACN universe and DMX addressing for each strip
3. **Create Scenes**: Build lighting looks using masks or global effects
4. **Connect MIDI**: Map MIDI controls to trigger scenes
5. **Perform**: Switch between scenes and adjust parameters in real-time

### Controls
- **Left Click**: Select strips or scenes
- **Right Click & Drag**: Pan the canvas view
- **Scroll Wheel**: Zoom in/out of the canvas
- **Drag**: Reposition LED strips or visualizer masks

### Configuration
- **sACN Output**: Configure in strip properties (universe, start address, pixel count)
- **MIDI Input**: Automatically detects connected MIDI devices
- **Audio Input**: Select input device from system preferences

## Protocol Support
- **Output**: E1.31 (sACN) for DMX512-compatible LED controllers
- **MIDI**: Standard MIDI input for scene triggering and control
- **Ableton Link**: Network tempo synchronization

## Troubleshooting

### Audio Input Not Working
- Ensure microphone/audio input permissions are granted
- Check system audio settings and select the correct input device

### MIDI Device Not Detected
- Verify MIDI device is connected before launching the application
- Check MIDI device compatibility and drivers

### sACN Not Sending
- Verify network interface is correctly configured
- Ensure no firewall is blocking UDP port 5568
- Check universe numbers don't conflict with other sACN devices

## License
MIT License - see LICENSE file for details

## Acknowledgments
Built with Google Antigravity and Claude Code.
