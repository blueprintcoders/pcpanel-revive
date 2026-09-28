// The app icon, drawn in code so there's no image file to ship: a dark rounded tile with five dots in the
// knob layout of the Pro. Used at runtime for the tray and the settings window, and by build.rs for the exe.
// (Shared with build.rs through include!, so no inner doc comments here.)

/// n x n RGBA pixels, drawn on a 32-unit grid and scaled.
pub fn rgba(n: usize) -> Vec<u8> {
    const SS: usize = 4; // supersampling per axis, for smooth edges
    // K1 K2 on top, K3 K4 K5 below; everything centered on (16, 16).
    let dots = [(11.0, 12.0), (21.0, 12.0), (6.0, 20.0), (16.0, 20.0), (26.0, 20.0)];
    let colors = [[255.0, 59.0, 48.0], [255.0, 149.0, 0.0], [52.0, 199.0, 89.0], [0.0, 122.0, 255.0], [175.0, 82.0, 222.0]];
    let tile = [34.0, 34.0, 40.0];
    let unit = 32.0 / n as f32;
    let mut px = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let (mut rgb, mut alpha) = ([0.0f32; 3], 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = (x as f32 + (sx as f32 + 0.5) / SS as f32) * unit;
                    let fy = (y as f32 + (sy as f32 + 0.5) / SS as f32) * unit;
                    // Rounded square: 30 units wide, radius 6.
                    let (dx, dy) = (((fx - 16.0).abs() - 9.0).max(0.0), ((fy - 16.0).abs() - 9.0).max(0.0));
                    if dx * dx + dy * dy > 36.0 {
                        continue;
                    }
                    let c = dots.iter().position(|&(cx, cy)| (fx - cx).powi(2) + (fy - cy).powi(2) <= 6.5).map_or(tile, |i| colors[i]);
                    (0..3).for_each(|k| rgb[k] += c[k]);
                    alpha += 1.0;
                }
            }
            let total = (SS * SS) as f32;
            let [r, g, b] = rgb.map(|v| if alpha > 0.0 { (v / alpha) as u8 } else { 0 });
            px.extend_from_slice(&[r, g, b, (alpha / total * 255.0) as u8]);
        }
    }
    px
}
