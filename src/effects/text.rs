//! Tiny GPU-batched 5x7 lettering for compact, click-through effect indicators.
//! Row runs, rather than individual pixels, keep the vertex count small. No fonts,
//! textures, windows or per-frame heap allocations are needed.

use glam::Vec2;

use super::Batch;
use crate::settings::presets::Rgb;

fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' | '×' => [0, 17, 10, 4, 10, 17, 0],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '.' => [0, 0, 0, 0, 0, 6, 6],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        _ => [0; 7],
    }
}

pub fn draw(batch: &mut Batch, text: &str, at: Vec2, pixel: f32, color: Rgb, alpha: f32) {
    for (i, c) in text.chars().enumerate() {
        for (row, bits) in glyph(c).iter().enumerate() {
            let mut column = 0;
            while column < 5 {
                if bits & (1 << (4 - column)) == 0 {
                    column += 1;
                    continue;
                }
                let first = column;
                while column < 5 && bits & (1 << (4 - column)) != 0 {
                    column += 1;
                }
                batch.rect(
                    at + Vec2::new((i * 6 + first) as f32, row as f32) * pixel,
                    Vec2::new((column - first) as f32 * pixel, pixel),
                    color,
                    alpha,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hud_alphabet_and_multiplier_have_valid_glyphs() {
        for c in
            "MADE IN HEAVEN TIME ACCELERATION AWAKENING UNIVERSE RESET DECELERATING 0123456789.×"
                .chars()
                .filter(|c| *c != ' ')
        {
            let g = glyph(c);
            assert!(g.iter().any(|r| *r != 0), "missing glyph {c}");
            assert!(g.iter().all(|r| *r <= 31));
        }
        let mut b = Batch::new(Vec2::ZERO, Vec2::splat(1000.0));
        draw(
            &mut b,
            "MADE IN HEAVEN",
            Vec2::splat(10.0),
            1.0,
            [1.0; 3],
            1.0,
        );
        assert!(!b.is_empty());
        assert!(b.verts.len() < 2000);
    }
}
