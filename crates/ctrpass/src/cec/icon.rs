//! Square RGB565 icons, stored as 8x8 tiles with pixels in Z order.

use crate::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    pub size: usize,
    /// Row-major RGB565 pixels.
    pub pixels: Vec<u16>,
}

pub fn rgb565(r: u8, g: u8, b: u8) -> u16 {
    (u16::from(r >> 3) << 11) | (u16::from(g >> 2) << 5) | u16::from(b >> 3)
}

pub fn rgb888(p: u16) -> [u8; 3] {
    let r = ((p >> 11) & 0x1f) as u32;
    let g = ((p >> 5) & 0x3f) as u32;
    let b = (p & 0x1f) as u32;
    [
        (r * 255 / 31) as u8,
        (g * 255 / 63) as u8,
        (b * 255 / 31) as u8,
    ]
}

/// Position of pixel `i` inside an 8x8 tile.
fn z_order(i: usize) -> (usize, usize) {
    let (mut x, mut y) = (0, 0);
    for bit in 0..3 {
        x |= ((i >> (2 * bit)) & 1) << bit;
        y |= ((i >> (2 * bit + 1)) & 1) << bit;
    }
    (x, y)
}

impl Icon {
    pub fn filled(size: usize, colour: u16) -> Self {
        Icon {
            size,
            pixels: vec![colour; size * size],
        }
    }

    pub fn byte_len(size: usize) -> usize {
        size * size * 2
    }

    /// Reads a `size` icon from tiled bytes.
    pub fn decode(size: usize, bytes: &[u8]) -> Result<Self, Error> {
        if size % 8 != 0 || bytes.len() < Self::byte_len(size) {
            return Err(Error::Format(format!(
                "{size}x{size} icon needs {:#x} bytes",
                Self::byte_len(size)
            )));
        }
        let mut pixels = vec![0u16; size * size];
        let mut i = 0;
        for ty in (0..size).step_by(8) {
            for tx in (0..size).step_by(8) {
                for k in 0..64 {
                    let (x, y) = z_order(k);
                    pixels[(ty + y) * size + tx + x] =
                        u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]);
                    i += 1;
                }
            }
        }
        Ok(Icon { size, pixels })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::byte_len(self.size));
        for ty in (0..self.size).step_by(8) {
            for tx in (0..self.size).step_by(8) {
                for k in 0..64 {
                    let (x, y) = z_order(k);
                    out.extend_from_slice(
                        &self.pixels[(ty + y) * self.size + tx + x].to_le_bytes(),
                    );
                }
            }
        }
        out
    }

    /// Scales row-major RGB pixels of any size to a `size` icon by nearest neighbour.
    pub fn from_rgb(size: usize, width: usize, height: usize, rgb: &[[u8; 3]]) -> Self {
        let mut pixels = Vec::with_capacity(size * size);
        for y in 0..size {
            for x in 0..size {
                let [r, g, b] = rgb[(y * height / size) * width + x * width / size];
                pixels.push(rgb565(r, g, b));
            }
        }
        Icon { size, pixels }
    }

    pub fn to_rgb(&self) -> Vec<[u8; 3]> {
        self.pixels.iter().map(|&p| rgb888(p)).collect()
    }

    /// The icon used when none is given: a ring around a dot on a teal ground.
    pub fn standard(size: usize) -> Self {
        let ground = rgb565(0x1f, 0x8a, 0x8a);
        let mark = rgb565(0xf4, 0xf1, 0xe8);
        let c = size as f32 / 2.0;
        let pixels = (0..size * size)
            .map(|i| {
                let dx = (i % size) as f32 + 0.5 - c;
                let dy = (i / size) as f32 + 0.5 - c;
                let d = (dx * dx + dy * dy).sqrt() / c;
                if d < 0.22 || (0.48..0.66).contains(&d) {
                    mark
                } else {
                    ground
                }
            })
            .collect();
        Icon { size, pixels }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_round_trip() {
        let icon = Icon {
            size: 24,
            pixels: (0..576).map(|i| i as u16 * 37).collect(),
        };
        let bytes = icon.encode();
        assert_eq!(bytes.len(), 0x480);
        assert_eq!(&bytes[..4], &[0, 0, 37, 0]);
        // The third pixel of the first tile sits one row down.
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 24 * 37);
        assert_eq!(Icon::decode(24, &bytes).unwrap(), icon);
    }

    #[test]
    fn scaling_and_colours() {
        let rgb = vec![[255, 0, 0], [0, 0, 255], [0, 255, 0], [255, 255, 255]];
        let icon = Icon::from_rgb(8, 2, 2, &rgb);
        assert_eq!(icon.pixels[0], 0xf800);
        assert_eq!(icon.pixels[7], 0x001f);
        assert_eq!(icon.pixels[63], 0xffff);
        assert_eq!(rgb888(0xffff), [255, 255, 255]);
        assert_ne!(
            Icon::standard(48).pixels[0],
            Icon::standard(48).pixels[24 * 48 + 24]
        );
    }
}
