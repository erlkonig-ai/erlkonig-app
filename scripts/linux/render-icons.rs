//! Render the Linux app icons from the production crown geometry.
//! cargo run --release -p erlkonig --example linux-icons -- OUTPUT_DIRECTORY
//! Writes OUTPUT_DIRECTORY/<n>x<n>/apps/ai.erlkonig.app.png in the hicolor
//! layout: a dark resting crown on a light rounded plate. No dependency; the
//! PNG encoder below stores the pixels uncompressed.

#[allow(dead_code)]
#[path = "../../crates/erlkonig/src/crown.rs"]
mod crown;

use std::path::Path;
use std::time::Duration;

const SIZES: [u32; 9] = [16, 22, 24, 32, 48, 64, 128, 256, 512];
const PLATE: [u8; 3] = [0xf4, 0xf1, 0xea];
const CROWN: [u8; 4] = [0x2a, 0x2a, 0x2a, 0xff];

fn main() {
    let Some(out) = std::env::args_os().nth(1) else {
        eprintln!("usage: linux-icons OUTPUT_DIRECTORY");
        std::process::exit(2);
    };
    let frame = crown::frame(Duration::ZERO, None, true, false);
    for side in SIZES {
        let dir = Path::new(&out).join(format!("{side}x{side}/apps"));
        std::fs::create_dir_all(&dir).expect("icon directory");
        let pixels = icon(&frame, side);
        std::fs::write(dir.join("ai.erlkonig.app.png"), png(side, side, &pixels))
            .expect("write icon");
    }
}

/// Plate with a 6% margin and 22% corner radius, crown at 68% width over it.
fn icon(frame: &crown::Frame, side: u32) -> Vec<u8> {
    let n = side as usize;
    let mut pixels = vec![0_u8; n * n * 4];
    let size = f64::from(side);
    let (margin, radius) = (size * 0.06, size * 0.22);
    for y in 0..n {
        for x in 0..n {
            // 4 × 4 samples per pixel against the rounded square.
            let mut inside = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (f64::from(sx) + 0.5) / 4.0;
                    let py = y as f64 + (f64::from(sy) + 0.5) / 4.0;
                    let dx = (margin + radius - px)
                        .max(px - (size - margin - radius))
                        .max(0.0);
                    let dy = (margin + radius - py)
                        .max(py - (size - margin - radius))
                        .max(0.0);
                    let edge =
                        px >= margin && px <= size - margin && py >= margin && py <= size - margin;
                    if edge && dx * dx + dy * dy <= radius * radius {
                        inside += 1;
                    }
                }
            }
            let at = (y * n + x) * 4;
            pixels[at..at + 3].copy_from_slice(&PLATE);
            pixels[at + 3] = (inside * 255 / 16) as u8;
        }
    }
    let width = ((size * 0.68).round() as u32).max(1);
    let height = ((f64::from(width) * crown::HEIGHT / crown::WIDTH).round() as u32).max(1);
    let (left, top) = (
        ((side - width) / 2) as usize,
        ((side - height) / 2) as usize,
    );
    let crown = frame.rgba(width, height, CROWN);
    for y in 0..height as usize {
        for x in 0..width as usize {
            let source = &crown[(y * width as usize + x) * 4..][..4];
            let at = ((top + y) * n + left + x) * 4;
            let alpha = f64::from(source[3]) / 255.0;
            let below = f64::from(pixels[at + 3]) / 255.0;
            let out = alpha + below * (1.0 - alpha);
            if out > 0.0 {
                for c in 0..3 {
                    let mixed = (f64::from(source[c]) * alpha
                        + f64::from(pixels[at + c]) * below * (1.0 - alpha))
                        / out;
                    pixels[at + c] = mixed.round() as u8;
                }
            }
            pixels[at + 3] = (out * 255.0).round() as u8;
        }
    }
    pixels
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// RGBA8 PNG with stored (uncompressed) deflate blocks.
fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for row in rgba.chunks(width as usize * 4) {
        raw.push(0); // Filter: none.
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1_u32, 0_u32);
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace.
    let mut file = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [(b"IHDR", header), (b"IDAT", zlib), (b"IEND", Vec::new())] {
        file.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = file.len();
        file.extend_from_slice(kind);
        file.extend_from_slice(&data);
        let crc = crc32(&file[start..]);
        file.extend_from_slice(&crc.to_be_bytes());
    }
    file
}
