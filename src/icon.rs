//! Procedural application icon, shared by `build.rs` (the .ico embedded in the exe)
//! and the runtime tray icon. No binary assets are needed.

const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 16.0),
    (4.0, 12.5),
    (7.0, 19.0),
    (9.6, 18.0),
    (6.6, 11.6),
    (11.6, 11.6),
];

fn inside_arrow(x: f32, y: f32) -> bool {
    // Even-odd ray casting.
    let mut inside = false;
    let n = ARROW.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ARROW[i];
        let (xj, yj) = ARROW[j];
        if ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Render a `size` x `size` RGBA icon: dark rounded tile, neon ghost arrows, glow.
pub fn render_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    let ss = 3; // supersampling per axis
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let fx = (px as f32 + (sx as f32 + 0.5) / ss as f32) / s;
                    let fy = (py as f32 + (sy as f32 + 0.5) / ss as f32) / s;
                    // Rounded tile.
                    let (cx, cy) = (fx - 0.5, fy - 0.5);
                    let rad = 0.22;
                    let qx = cx.abs() - (0.5 - rad);
                    let qy = cy.abs() - (0.5 - rad);
                    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt()
                        + qx.max(qy).min(0.0)
                        - rad;
                    if d > 0.0 {
                        continue;
                    }
                    let mut col = [0.063, 0.071, 0.090]; // #101216
                    // Soft green glow toward the arrow.
                    let gd = ((fx - 0.42).powi(2) + (fy - 0.45).powi(2)).sqrt();
                    let glow = (1.0 - gd / 0.55).clamp(0.0, 1.0).powi(2) * 0.28;
                    col = [
                        col[0] + 0.1 * glow,
                        col[1] + 0.96 * glow,
                        col[2] + 0.69 * glow,
                    ];
                    // Three arrows: two faded "ghosts" and the bright one.
                    for (k, (alpha, ox)) in [(0.25f32, -0.17f32), (0.5, -0.09), (1.0, 0.0)]
                        .iter()
                        .enumerate()
                    {
                        // Arrow units are ~0..20; 0.027 per unit makes it ~54% of the tile.
                        let unit = 0.027;
                        let ax = (fx - 0.40 - ox) / unit;
                        let ay = (fy - 0.20) / unit;
                        if inside_arrow(ax, ay) {
                            let bright = if k == 2 {
                                [0.89, 1.0, 0.95]
                            } else {
                                [0.41, 0.96, 0.69]
                            };
                            col = [
                                col[0] * (1.0 - alpha) + bright[0] * alpha,
                                col[1] * (1.0 - alpha) + bright[1] * alpha,
                                col[2] * (1.0 - alpha) + bright[2] * alpha,
                            ];
                        }
                    }
                    r += col[0];
                    g += col[1];
                    b += col[2];
                    a += 1.0;
                }
            }
            let n = (ss * ss) as f32;
            let i = ((py * size + px) * 4) as usize;
            if a > 0.0 {
                out[i] = (r / a * 255.0).clamp(0.0, 255.0) as u8;
                out[i + 1] = (g / a * 255.0).clamp(0.0, 255.0) as u8;
                out[i + 2] = (b / a * 255.0).clamp(0.0, 255.0) as u8;
                out[i + 3] = (a / n * 255.0) as u8;
            }
        }
    }
    out
}

/// Build a multi-size .ico (BMP/DIB entries) from the procedural icon.
pub fn build_ico(sizes: &[u32]) -> Vec<u8> {
    let mut images: Vec<Vec<u8>> = Vec::new();
    for &size in sizes {
        let rgba = render_rgba(size);
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
        dib.extend_from_slice(&(size as i32).to_le_bytes());
        dib.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // XOR + AND heights
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        for y in (0..size).rev() {
            for x in 0..size {
                let i = ((y * size + x) * 4) as usize;
                dib.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        let row = size.div_ceil(32) * 4;
        dib.extend(std::iter::repeat_n(0u8, (row * size) as usize)); // AND mask (unused with alpha)
        images.push(dib);
    }
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (i, &size) in sizes.iter().enumerate() {
        ico.push(if size >= 256 { 0 } else { size as u8 });
        ico.push(if size >= 256 { 0 } else { size as u8 });
        ico.extend_from_slice(&[0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(images[i].len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += images[i].len() as u32;
    }
    for img in images {
        ico.extend_from_slice(&img);
    }
    ico
}
