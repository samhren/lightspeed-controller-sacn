//! Scanner Mask Collision Detection
//!
//! This module implements a scanner mask system for LED lighting control.
//! A scanner mask is a rectangular region containing a sweeping bar that moves
//! back and forth. LEDs light up when they are both inside the mask bounds
//! and near enough to the scanning bar.
//!
//! # Coordinate Systems
//!
//! - **World Space**: The global coordinate system where LED strips and masks
//!   are positioned. All positions use normalized float coordinates.
//!
//! - **Mask Local Space**: A coordinate system centered at the mask position
//!   (mask_x, mask_y) and aligned with the mask rotation. The bounds are
//!   x ∈ [-width/2, width/2] and y ∈ [-height/2, height/2].
//!
//! # Transformation
//!
//! To transform a point from world space to mask local space:
//! 1. Translate: subtract the mask position
//! 2. Rotate: apply inverse rotation (rotate by -mask_rotation)
//!
//! The inverse rotation matrix for angle θ is:
//! ```text
//! [ cos(θ)   sin(θ) ]
//! [-sin(θ)   cos(θ) ]
//! ```

use crate::model::PixelStrip;

/// Apply a scanner mask effect to LED strips.
///
/// # Parameters
///
/// * `mask_x` - Mask center X position in world space
/// * `mask_y` - Mask center Y position in world space
/// * `mask_width` - Width of the mask rectangle in local space
/// * `mask_height` - Height of the mask rectangle in local space
/// * `mask_rotation_degrees` - Mask rotation in degrees (0-360)
/// * `bar_position_normalized` - Bar position from -1.0 (left edge) to 1.0 (right edge)
/// * `bar_width` - Width of the scanning bar (distance threshold)
/// * `hard_edge` - If true, full intensity within bar_width; if false, linear falloff
/// * `color` - RGB color to apply [R, G, B]
/// * `strips` - Mutable slice of LED strips to modify
///
/// # How It Works
///
/// For each pixel in each strip:
/// 1. Calculate the pixel's position in world space based on strip parameters
/// 2. Transform the pixel position to the mask's local coordinate system
/// 3. Check if the pixel is inside the rectangular mask bounds
/// 4. Calculate distance from the pixel to the scanning bar center
/// 5. Apply color with intensity based on distance (if within bar_width)
///
/// # Examples
///
/// ```ignore
/// // Mask at center, 0.3x0.3 size, no rotation
/// // Bar at left edge, 0.1 wide, hard edge, cyan color
/// apply_scanner_mask(
///     0.5, 0.5,           // mask center
///     0.3, 0.3,           // mask size
///     0.0,                // no rotation
///     -1.0,               // bar at left edge
///     0.1,                // bar width
///     true,               // hard edge
///     [0, 255, 255],      // cyan
///     &mut strips
/// );
/// ```
pub fn apply_scanner_mask(
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
    // Precompute rotation matrix values for inverse rotation
    // We rotate by -θ to convert from world space to local space
    let rotation_rad = mask_rotation_degrees.to_radians();
    let cos_theta = rotation_rad.cos();
    let sin_theta = rotation_rad.sin();

    // Calculate bar center position in mask local space
    // bar_position_normalized ranges from -1.0 to 1.0
    // We scale by (width/2 - bar_width) so the bar EDGES reach the mask edges,
    // not the bar CENTER. This prevents the bar from being clipped at edges.
    let sweep_range = (mask_width / 2.0) - bar_width;
    let bar_center_x = sweep_range * bar_position_normalized;

    // Precompute half dimensions for bounds checking
    let half_width = mask_width / 2.0;
    let half_height = mask_height / 2.0;

    // Process each LED strip
    for strip in strips.iter_mut() {
        // Precompute strip rotation matrix values
        let strip_cos = strip.rotation.cos();
        let strip_sin = strip.rotation.sin();

        // Ensure we don't exceed array bounds
        let pixel_limit = strip.pixel_count.min(strip.data.len());

        // Process each pixel in the strip
        for pixel_index in 0..pixel_limit {
            // === 1. Calculate pixel position in world space ===

            // Distance along strip from start point
            let distance_along_strip = pixel_index as f32 * strip.spacing;

            // Apply strip rotation to get world position
            // The pixel moves along the strip's local x-axis
            let pixel_world_x = strip.x + distance_along_strip * strip_cos;
            let pixel_world_y = strip.y + distance_along_strip * strip_sin;

            // === 2. Transform to mask's local coordinate system ===

            // Translate: move origin to mask center
            let dx = pixel_world_x - mask_x;
            let dy = pixel_world_y - mask_y;

            // Rotate by -θ (inverse rotation) to align with mask's local axes
            // Using inverse rotation matrix:
            // [local_x]   [ cos(θ)  sin(θ)] [dx]
            // [local_y] = [-sin(θ)  cos(θ)] [dy]
            let local_x = dx * cos_theta + dy * sin_theta;
            let local_y = -dx * sin_theta + dy * cos_theta;

            // === 3. Check if pixel is inside rectangular mask bounds ===

            // Small epsilon for floating point tolerance at edges
            // This prevents pixels right at the boundary from being excluded
            // due to floating point rounding errors in the rotation transform
            const EPSILON: f32 = 0.0001;

            // Must satisfy: -half_width <= local_x <= half_width (with tolerance)
            //          AND: -half_height <= local_y <= half_height (with tolerance)
            if local_x < -(half_width + EPSILON) || local_x > (half_width + EPSILON) {
                continue; // Outside horizontal bounds
            }
            if local_y < -(half_height + EPSILON) || local_y > (half_height + EPSILON) {
                continue; // Outside vertical bounds
            }

            // === 4. Calculate distance to scanning bar ===

            // The bar is a vertical line at x = bar_center_x in local space
            // Distance is just the horizontal offset
            let distance_to_bar = (local_x - bar_center_x).abs();

            // === 5. Apply color if within bar width ===

            if distance_to_bar <= bar_width {
                // Calculate intensity based on distance
                let intensity = if hard_edge {
                    // Hard edge: full intensity anywhere within bar_width
                    1.0
                } else {
                    // Soft edge: linear falloff from 1.0 at center to 0.0 at bar_width
                    // intensity = 1.0 - (distance / bar_width)
                    (1.0 - distance_to_bar / bar_width).max(0.0)
                };

                // Apply intensity to color
                let r = (color[0] as f32 * intensity) as u8;
                let g = (color[1] as f32 * intensity) as u8;
                let b = (color[2] as f32 * intensity) as u8;

                // Add to existing pixel color (saturating to prevent overflow)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a test strip
    fn create_test_strip(x: f32, y: f32, rotation: f32, pixel_count: usize) -> PixelStrip {
        PixelStrip {
            id: 1,
            universe: 1,
            start_channel: 1,
            pixel_count,
            x,
            y,
            spacing: 0.01, // 1cm spacing in normalized coords
            rotation,
            color_order: "RGB".to_string(),
            data: vec![[0, 0, 0]; pixel_count],
        }
    }

    #[test]
    fn test_horizontal_mask_bar_at_center() {
        // Horizontal strip at y=0.5, running left to right
        let mut strips = vec![create_test_strip(0.0, 0.5, 0.0, 100)];

        // Mask at (0.5, 0.5), 0.3x0.2, no rotation
        // Bar at center (position = 0.0), width 0.05, cyan color
        apply_scanner_mask(
            0.5, 0.5,           // mask center
            0.3, 0.2,           // 0.3 wide, 0.2 tall
            0.0,                // no rotation
            0.0,                // bar at center
            0.05,               // bar width
            true,               // hard edge
            [0, 255, 255],      // cyan
            &mut strips,
        );

        // Pixels around x=0.5 should be lit (within mask and bar)
        // Pixel 50 is at x = 0.0 + 50*0.01 = 0.5, which is mask center
        assert_eq!(strips[0].data[50], [0, 255, 255], "Center pixel should be lit");

        // Pixels far from center should be dark
        assert_eq!(strips[0].data[0], [0, 0, 0], "Far left pixel should be dark");
        assert_eq!(strips[0].data[99], [0, 0, 0], "Far right pixel should be dark");
    }

    #[test]
    fn test_rotated_90_degrees() {
        // Vertical strip (rotated 90°) at x=0.5, starting at y=0.0
        let mut strips = vec![create_test_strip(0.5, 0.0, std::f32::consts::FRAC_PI_2, 100)];

        // Mask at (0.5, 0.5), 0.2x0.3, rotated 90°
        // This should align with the vertical strip
        apply_scanner_mask(
            0.5, 0.5,           // mask center
            0.2, 0.3,           // dimensions
            90.0,               // rotated 90°
            0.0,                // bar at center
            0.05,               // bar width
            true,               // hard edge
            [255, 0, 255],      // magenta
            &mut strips,
        );

        // Pixel 50 is at y = 0.0 + 50*0.01 = 0.5, which is mask center
        assert_eq!(strips[0].data[50], [255, 0, 255], "Center pixel should be lit with rotated mask");
    }

    #[test]
    fn test_bar_at_edges() {
        // Test that bar reaches both edges of the mask
        let mut strips = vec![create_test_strip(0.0, 0.5, 0.0, 100)];

        // Bar at left edge (position = -1.0)
        apply_scanner_mask(
            0.5, 0.5,
            0.3, 0.2,
            0.0,
            -1.0,               // bar at LEFT edge
            0.05,
            true,
            [255, 0, 0],        // red
            &mut strips,
        );

        // Left edge of mask is at x = 0.5 - 0.3/2 = 0.35
        // Pixel 35 is at x = 0.35, should be lit
        let left_lit = strips[0].data[35] != [0, 0, 0];
        assert!(left_lit, "Pixels at left edge should light when bar is at -1.0");

        // Clear and test right edge
        strips[0].data = vec![[0, 0, 0]; 100];

        apply_scanner_mask(
            0.5, 0.5,
            0.3, 0.2,
            0.0,
            1.0,                // bar at RIGHT edge
            0.05,
            true,
            [0, 255, 0],        // green
            &mut strips,
        );

        // Right edge of mask is at x = 0.5 + 0.3/2 = 0.65
        // Pixel 65 is at x = 0.65, should be lit
        let right_lit = strips[0].data[65] != [0, 0, 0];
        assert!(right_lit, "Pixels at right edge should light when bar is at 1.0");
    }

    #[test]
    fn test_soft_edge_falloff() {
        let mut strips = vec![create_test_strip(0.0, 0.5, 0.0, 100)];

        apply_scanner_mask(
            0.5, 0.5,
            0.3, 0.2,
            0.0,
            0.0,
            0.05,
            false,              // SOFT edge (linear falloff)
            [255, 255, 255],    // white
            &mut strips,
        );

        // Center pixel should be full brightness
        assert_eq!(strips[0].data[50], [255, 255, 255], "Center should be full brightness");

        // Pixels near the edge of bar_width should be dimmer
        // This test is approximate due to discretization
        let edge_pixel = strips[0].data[55]; // ~0.05 away from center
        assert!(edge_pixel[0] < 255 && edge_pixel[0] > 0, "Edge pixel should have partial brightness");
    }

    #[test]
    fn test_bounds_checking() {
        // Strip that extends beyond mask bounds
        let mut strips = vec![create_test_strip(0.0, 0.5, 0.0, 200)];

        apply_scanner_mask(
            0.5, 0.5,           // mask center
            0.2, 0.1,           // small mask: 0.2 wide, 0.1 tall
            0.0,
            0.0,                // bar at center
            0.2,                // very wide bar
            true,
            [255, 255, 0],      // yellow
            &mut strips,
        );

        // Pixels outside mask bounds should remain dark
        // Mask spans x = [0.4, 0.6], so pixels at x < 0.4 should be dark
        assert_eq!(strips[0].data[0], [0, 0, 0], "Pixel outside mask should be dark");
        assert_eq!(strips[0].data[199], [0, 0, 0], "Pixel outside mask should be dark");

        // Pixels inside mask bounds should be lit
        // Pixel 50 at x=0.5 is inside mask and hit by bar
        assert_eq!(strips[0].data[50], [255, 255, 0], "Pixel inside mask should be lit");
    }
}
