//! Image overlays drawn on screenshots to help the model judge coordinates.
//!
//! The grid overlay draws faint vertical/horizontal rules every [`GRID_STEP`]
//! pixels and labels them with their pixel coordinate, so the model can read a
//! target's position straight off the axes instead of estimating it.
//!
//! Text is rendered with a tiny built-in 3x5 bitmap digit font — no font asset
//! or extra dependency — which is all the labels (numbers) need.

use image::{Rgb, RgbImage};

/// Grid spacing in screenshot pixels.
const GRID_STEP: u32 = 100;
/// Label scale (each font pixel becomes SCALE x SCALE).
const LABEL_SCALE: u32 = 2;

const GRID_COLOR: Rgb<u8> = Rgb([255, 0, 255]); // magenta rules
const LABEL_FG: Rgb<u8> = Rgb([255, 255, 0]); // yellow digits
const LABEL_BG: Rgb<u8> = Rgb([0, 0, 0]); // dark backing for legibility

/// 3x5 bitmaps for digits 0-9. Each row's low 3 bits are pixels (bit2=left).
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b001, 0b010, 0b010], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
];

const DIGIT_W: u32 = 3;
const DIGIT_H: u32 = 5;

/// Blend `color` into the pixel at 50% so underlying content stays visible.
fn blend_half(img: &mut RgbImage, x: u32, y: u32, color: Rgb<u8>) {
    if x >= img.width() || y >= img.height() {
        return;
    }
    let p = img.get_pixel_mut(x, y);
    for i in 0..3 {
        p.0[i] = ((p.0[i] as u16 + color.0[i] as u16) / 2) as u8;
    }
}

fn set_px(img: &mut RgbImage, x: u32, y: u32, color: Rgb<u8>) {
    if x < img.width() && y < img.height() {
        img.put_pixel(x, y, color);
    }
}

/// Draw a single digit's SCALE-scaled bitmap with top-left at (x, y).
fn draw_digit(img: &mut RgbImage, x: u32, y: u32, digit: u8, scale: u32, color: Rgb<u8>) {
    let glyph = &DIGITS[digit as usize];
    for (row, bits) in glyph.iter().enumerate() {
        for col in 0..DIGIT_W {
            if bits & (1 << (DIGIT_W - 1 - col)) != 0 {
                for dy in 0..scale {
                    for dx in 0..scale {
                        set_px(
                            img,
                            x + col * scale + dx,
                            y + row as u32 * scale + dy,
                            color,
                        );
                    }
                }
            }
        }
    }
}

/// Pixel width of `n` rendered at `scale` (digits + 1px spacing between them).
fn number_width(n: u32, scale: u32) -> u32 {
    let digits = n.to_string().len() as u32;
    digits * DIGIT_W * scale + digits.saturating_sub(1) * scale
}

/// Draw `n` as decimal digits with a dark backing box, top-left at (x, y).
fn draw_number(img: &mut RgbImage, x: u32, y: u32, n: u32, scale: u32) {
    let w = number_width(n, scale);
    let h = DIGIT_H * scale;
    // Backing box (1px padding) for contrast against busy backgrounds.
    for by in y.saturating_sub(1)..(y + h + 1) {
        for bx in x.saturating_sub(1)..(x + w + 1) {
            set_px(img, bx, by, LABEL_BG);
        }
    }
    let mut cx = x;
    for ch in n.to_string().bytes() {
        let d = ch - b'0';
        draw_digit(img, cx, y, d, scale, LABEL_FG);
        cx += DIGIT_W * scale + scale;
    }
}

/// Overlay a labeled coordinate grid on the image, in place.
pub fn draw_grid(img: &mut RgbImage) {
    let (w, h) = (img.width(), img.height());

    // Vertical rules + x labels along the top.
    let mut gx = GRID_STEP;
    while gx < w {
        for y in 0..h {
            blend_half(img, gx, y, GRID_COLOR);
        }
        draw_number(img, gx + 2, 2, gx, LABEL_SCALE);
        gx += GRID_STEP;
    }

    // Horizontal rules + y labels along the left.
    let mut gy = GRID_STEP;
    while gy < h {
        for x in 0..w {
            blend_half(img, x, gy, GRID_COLOR);
        }
        draw_number(img, 2, gy + 2, gy, LABEL_SCALE);
        gy += GRID_STEP;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_width_matches_digits() {
        // 1 digit: 3*scale; 4 digits: 4*3*scale + 3*scale spacing.
        assert_eq!(number_width(5, 2), 3 * 2);
        assert_eq!(number_width(1200, 2), 4 * 3 * 2 + 3 * 2);
    }

    #[test]
    fn draw_grid_marks_pixels_without_panicking() {
        let mut img = RgbImage::from_pixel(300, 250, Rgb([20, 20, 20]));
        draw_grid(&mut img);
        // A vertical rule at x=100 should have altered some pixels in that column.
        let changed = (0..img.height()).any(|y| img.get_pixel(100, y).0 != [20, 20, 20]);
        assert!(changed, "expected grid rule at x=100 to change pixels");
        // Labels exist near the top-left of the first rule.
        let label_area_changed =
            (2..14).any(|x| (2..14).any(|y| img.get_pixel(x + 100, y).0 != [20, 20, 20]));
        assert!(
            label_area_changed,
            "expected an x label near the first rule"
        );
    }

    #[test]
    fn draw_grid_handles_tiny_image() {
        // Smaller than one grid step — must not panic, just no rules.
        let mut img = RgbImage::from_pixel(40, 30, Rgb([0, 0, 0]));
        draw_grid(&mut img);
    }
}
