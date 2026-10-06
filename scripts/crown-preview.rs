//! Standalone, dependency-free contact sheet from the production crown module.
//! Compile with rustc (not Cargo), then pass the destination SVG path.

#[allow(dead_code)]
#[path = "../crates/erlkonig/src/crown.rs"]
mod crown;

use std::fmt::Write as _;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let destination = std::env::args()
        .nth(1)
        .ok_or("usage: crown-preview OUTPUT.svg")?;
    let mut svg = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1420\" height=\"610\" viewBox=\"0 0 1420 610\"><title>Erlkönig crown — real transfer activity and exact rest</title><rect width=\"1420\" height=\"610\" fill=\"#fafafa\"/><style>text{font-family:system-ui,sans-serif;font-size:14px} .label{font-size:12px}</style><text x=\"24\" y=\"32\" font-size=\"22\">Erlkönig · transfer wavelet</text><text x=\"24\" y=\"58\">Canonical silhouette · 0.85 px maximum motion · 160 ms attack · 600 ms to exact rest · 18 pt native size</text>",
    );
    let cases = [
        ("Idle", 0, None, false),
        ("Attack · 80 ms", 80, Some(80), false),
        ("Transferring · 400 ms", 400, Some(400), false),
        ("Transferring · 1000 ms", 1000, Some(1000), false),
        ("Quiet · 300 ms", 1300, Some(1000), false),
        ("Quiet · 600 ms (rest)", 1600, Some(1000), false),
        ("Reduced motion + bytes", 1000, Some(1000), true),
    ];
    for row in 0..2 {
        let top = 82 + row * 250;
        let (background, foreground) = if row == 0 {
            ("#fafafa", "#171717")
        } else {
            ("#222326", "#fafafa")
        };
        writeln!(
            svg,
            "<rect x=\"0\" y=\"{top}\" width=\"1420\" height=\"250\" fill=\"{background}\"/>"
        )?;
        for (column, (label, now, last, reduced)) in cases.iter().enumerate() {
            let x = 24 + column * 200;
            let activity = last.map(|last| crown::TransferActivity {
                began_at: Duration::ZERO,
                last_transfer_at: Duration::from_millis(last),
            });
            let frame = crown::frame(Duration::from_millis(*now), activity, *reduced);
            let points = frame
                .points
                .iter()
                .map(|p| format!("{:.4},{:.4}", p.x, p.y))
                .collect::<Vec<_>>()
                .join(" ");
            writeln!(
                svg,
                "<g fill=\"{foreground}\"><text x=\"{x}\" y=\"{}\">{label}</text><polygon points=\"{points}\" transform=\"translate({}, {}) scale(3)\"/><text class=\"label\" x=\"{x}\" y=\"{}\">vector × 3</text>",
                top + 22,
                x + 28,
                top + 30,
                top + 146
            )?;
            let (pixels, width, _) = frame.rgba();
            writeln!(
                svg,
                "<g transform=\"translate({}, {}) scale(2)\">",
                x + 28,
                top + 149
            )?;
            for (index, p) in pixels
                .chunks_exact(4)
                .enumerate()
                .filter(|(_, p)| p[3] != 0)
            {
                writeln!(
                    svg,
                    "<rect x=\"{}\" y=\"{}\" width=\"1\" height=\"1\" opacity=\"{:.4}\"/>",
                    index % width as usize,
                    index / width as usize,
                    f64::from(p[3]) / 255.0
                )?;
            }
            writeln!(
                svg,
                "</g><text class=\"label\" x=\"{x}\" y=\"{}\">template × 2</text><polygon points=\"{points}\" transform=\"translate({}, {}) scale(.5)\"/><text class=\"label\" x=\"{}\" y=\"{}\">18 pt</text></g>",
                top + 234,
                x + 144,
                top + 181,
                x + 136,
                top + 222
            )?;
        }
    }
    svg.push_str("</svg>\n");
    std::fs::write(&destination, svg)?;
    println!("{destination}");
    Ok(())
}
