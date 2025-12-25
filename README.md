# Lightspeed Controller

A high-performance, Rust-based LED lighting controller featuring dynamic scenes, mask-based visualizers, global effects, and sACN output.

## Overview

Lightspeed Controller is designed for managing complex LED strip setups. It allows you to create scenes composed of layered masks (Scanner, Radial, Linear) or apply global effects across your entire setup. With persistent storage, MIDI integration, and granular per-strip control, it offers a robust solution for live lighting performance.

## Key Features

### 🎨 Scene Management
- **Multiple Scenes**: Create, name, and switch between different lighting configurations.
- **Scene Types**:
    - **Masks**: Compositional scenes using geometric masks to reveal or hide underlying patterns.
    - **Global**: Apply effects to strips directly without masking.

### 🎭 dynamic Visualizers
- **Scanner Mask**: A scanning bar effect with configurable width, speed, and motion easing (Sine, Triangle, **Sawtooth/Unidirectional**).
- **Radial Mask**: Expanding/contracting circular pulse effects.
- **Linear Mask**: Standard linear gradients and wipe effects.
- **LFO Modulation**: most parameters (width, height, speed) can be modulated by Low Frequency Oscillators.

### 🌍 Global Effects
- **Per-Strip Targeting**: Apply effects to **All Strips** or a specific subset of strips. This allows for complex looks like different colored rows or distinct patterns on separate fixtures.
- **Stackable**: Layer multiple effects within a single scene.
- **Effect Types**:
    - **Solid**: Static color.
    - **Rainbow**: Scrolling rainbow gradient.
    - **Flash**: Strobe/flash effects with speed control.
    - **Sparkle**: Randomized sparkle pixels with density and decay control.

### 🛠️ Usability Enhancements
- **Hex Code Input**: Easily copy/paste hex color codes (e.g., `#FF00FF`) directly into the color picker.
- **MIDI Support**: Trigger scenes using a Launchpad or other generic MIDI controllers.
- **Database Persistence**: All configurations are automatically saved to a local SQLite database, preserving your work between sessions.

## Installation

### Prerequisites
- [Rust & Cargo](https://rustup.rs/)

### Build & Run
```bash
# Clone the repository
git clone <repository-url>
cd Lights

# Run in release mode for best performance
cargo run --release
```

## Controls

- **Left Click**: Select strips or scenes.
- **Right Click & Drag**: Pan the canvas view.
- **Scroll Wheel**: Zoom in/out of the canvas.
- **Drag (on items)**: Move LED strips or visualizer masks.

## Protocol Support
- **Output**: E1.31 (sACN) for controlling compatible LED controllers.

## License
[License Information Here]
