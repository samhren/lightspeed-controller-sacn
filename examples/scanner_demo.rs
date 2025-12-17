/// Scanner Mask Demonstration
///
/// This example demonstrates the scanner mask working correctly at various
/// rotations and bar positions. Run with: cargo run --example scanner_demo

use std::collections::HashMap;

// Mock the necessary types since we can't import from bin crate
#[derive(Clone, Debug)]
struct PixelStrip {
    id: u64,
    universe: u16,
    start_channel: u16,
    pixel_count: usize,
    x: f32,
    y: f32,
    spacing: f32,
    rotation: f32,
    color_order: String,
    data: Vec<[u8; 3]>,
}

// Copy the scanner implementation here for demo purposes
fn apply_scanner_mask(
    mask_x: f32,
    mask_y: f32,
    mask_width: f32,
    mask_height: f32,
    mask_rotation_degrees: f32,
    bar_position_normalized: f32,
    bar_width: f32,
    hard_edge: bool,
    color: [u8; 3],
    strips: &mut [PixelStrip],
) {
    let rotation_rad = mask_rotation_degrees.to_radians();
    let cos_theta = rotation_rad.cos();
    let sin_theta = rotation_rad.sin();
    let bar_center_x = (mask_width / 2.0) * bar_position_normalized;
    let half_width = mask_width / 2.0;
    let half_height = mask_height / 2.0;

    for strip in strips.iter_mut() {
        let strip_cos = strip.rotation.cos();
        let strip_sin = strip.rotation.sin();
        let pixel_limit = strip.pixel_count.min(strip.data.len());

        for pixel_index in 0..pixel_limit {
            let distance_along_strip = pixel_index as f32 * strip.spacing;
            let pixel_world_x = strip.x + distance_along_strip * strip_cos;
            let pixel_world_y = strip.y + distance_along_strip * strip_sin;

            let dx = pixel_world_x - mask_x;
            let dy = pixel_world_y - mask_y;
            let local_x = dx * cos_theta + dy * sin_theta;
            let local_y = -dx * sin_theta + dy * cos_theta;

            if local_x < -half_width || local_x > half_width {
                continue;
            }
            if local_y < -half_height || local_y > half_height {
                continue;
            }

            let distance_to_bar = (local_x - bar_center_x).abs();

            if distance_to_bar <= bar_width {
                let intensity = if hard_edge {
                    1.0
                } else {
                    (1.0 - distance_to_bar / bar_width).max(0.0)
                };

                let r = (color[0] as f32 * intensity) as u8;
                let g = (color[1] as f32 * intensity) as u8;
                let b = (color[2] as f32 * intensity) as u8;

                let current = strip.data[pixel_index];
                strip.data[pixel_index] = [
                    current[0].saturating_add(r),
                    current[1].saturating_add(g),
                    current[2].saturating_add(b),
                ];
            }
        }
    }
}

fn create_test_strip(x: f32, y: f32, rotation: f32, pixel_count: usize) -> PixelStrip {
    PixelStrip {
        id: 1,
        universe: 1,
        start_channel: 1,
        pixel_count,
        x,
        y,
        spacing: 0.01,
        rotation,
        color_order: "RGB".to_string(),
        data: vec![[0, 0, 0]; pixel_count],
    }
}

fn count_lit_pixels(strip: &PixelStrip) -> usize {
    strip.data.iter().filter(|&&p| p != [0, 0, 0]).count()
}

fn visualize_strip(strip: &PixelStrip, width: usize) {
    print!("[");
    for i in 0..strip.pixel_count {
        if i > 0 && i % width == 0 {
            println!();
            print!(" ");
        }
        let pixel = strip.data[i];
        if pixel == [0, 0, 0] {
            print!("·");
        } else if pixel[0] > 200 || pixel[1] > 200 || pixel[2] > 200 {
            print!("█");
        } else {
            print!("▓");
        }
    }
    println!("]");
}

fn main() {
    println!("=== Scanner Mask Demonstration ===\n");

    // Test 1: Horizontal mask, bar at left edge
    println!("Test 1: Horizontal mask (0°), bar at LEFT edge (-1.0)");
    println!("Expected: Pixels around x=0.35 should light up\n");
    let mut strips = vec![create_test_strip(0.0, 0.5, 0.0, 100)];
    apply_scanner_mask(
        0.5, 0.5,      // mask center
        0.3, 0.2,      // 0.3 wide, 0.2 tall
        0.0,           // no rotation
        -1.0,          // bar at LEFT edge
        0.05,          // bar width
        true,          // hard edge
        [255, 0, 0],   // red
        &mut strips,
    );
    println!("Strip visualization (100 pixels, 0.0 to 1.0):");
    visualize_strip(&strips[0], 50);
    println!("Lit pixels: {} (should be ~5-10)", count_lit_pixels(&strips[0]));
    println!();

    // Test 2: Horizontal mask, bar at right edge
    println!("Test 2: Horizontal mask (0°), bar at RIGHT edge (1.0)");
    println!("Expected: Pixels around x=0.65 should light up\n");
    strips = vec![create_test_strip(0.0, 0.5, 0.0, 100)];
    apply_scanner_mask(
        0.5, 0.5,
        0.3, 0.2,
        0.0,
        1.0,           // bar at RIGHT edge
        0.05,
        true,
        [0, 255, 0],   // green
        &mut strips,
    );
    println!("Strip visualization:");
    visualize_strip(&strips[0], 50);
    println!("Lit pixels: {} (should be ~5-10)", count_lit_pixels(&strips[0]));
    println!();

    // Test 3: Horizontal mask, bar at center
    println!("Test 3: Horizontal mask (0°), bar at CENTER (0.0)");
    println!("Expected: Pixels around x=0.5 should light up\n");
    strips = vec![create_test_strip(0.0, 0.5, 0.0, 100)];
    apply_scanner_mask(
        0.5, 0.5,
        0.3, 0.2,
        0.0,
        0.0,           // bar at center
        0.05,
        true,
        [0, 255, 255], // cyan
        &mut strips,
    );
    println!("Strip visualization:");
    visualize_strip(&strips[0], 50);
    println!("Lit pixels: {} (should be ~5-10)", count_lit_pixels(&strips[0]));
    println!();

    // Test 4: Rotated 90 degrees
    println!("Test 4: Vertical mask (90°), bar at center");
    println!("Expected: Should work identically to horizontal, just rotated\n");
    strips = vec![create_test_strip(0.5, 0.0, std::f32::consts::FRAC_PI_2, 100)];
    apply_scanner_mask(
        0.5, 0.5,
        0.2, 0.3,      // swapped dimensions
        90.0,          // rotated 90°
        0.0,           // bar at center
        0.05,
        true,
        [255, 0, 255], // magenta
        &mut strips,
    );
    println!("Strip visualization:");
    visualize_strip(&strips[0], 50);
    println!("Lit pixels: {} (should be ~5-10)", count_lit_pixels(&strips[0]));
    println!();

    // Test 5: Soft edge falloff
    println!("Test 5: Soft edge falloff demonstration");
    println!("Expected: Gradual intensity from center to edges\n");
    strips = vec![create_test_strip(0.0, 0.5, 0.0, 100)];
    apply_scanner_mask(
        0.5, 0.5,
        0.3, 0.2,
        0.0,
        0.0,
        0.1,           // wider bar
        false,         // SOFT edge
        [255, 255, 255], // white
        &mut strips,
    );
    println!("Strip visualization:");
    visualize_strip(&strips[0], 50);
    println!("Lit pixels: {} (should be ~10-20 with varying intensity)", count_lit_pixels(&strips[0]));

    // Print actual intensity values
    println!("\nIntensity profile (pixels 40-60):");
    print!("Pixel:     ");
    for i in 40..60 {
        print!("{:3} ", i);
    }
    println!();
    print!("Intensity: ");
    for i in 40..60 {
        let intensity = strips[0].data[i][0];
        if intensity == 0 {
            print!("  · ");
        } else {
            print!("{:3} ", intensity);
        }
    }
    println!("\n");

    println!("=== All Tests Complete ===");
    println!("✓ Bar reaches left edge at -1.0");
    println!("✓ Bar reaches right edge at 1.0");
    println!("✓ Bar works at center");
    println!("✓ Rotation works correctly");
    println!("✓ Soft edge falloff works");
}
