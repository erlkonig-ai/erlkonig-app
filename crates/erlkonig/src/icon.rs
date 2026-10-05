//! The menu-bar template icon, drawn by scripts/make-icon.py.

pub const MENUBAR_PNG: &[u8] = include_bytes!("../../../assets/menubar.png");

/// Decodes the icon to RGBA8: (pixels, width, height).
pub fn menubar_rgba() -> Result<(Vec<u8>, u32, u32), String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(MENUBAR_PNG));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("icon too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(format!("icon is {:?}/{:?}, expected RGBA8", info.color_type, info.bit_depth));
    }
    buf.truncate(info.buffer_size());
    Ok((buf, info.width, info.height))
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_decodes_as_a_36px_template() {
        let (rgba, w, h) = super::menubar_rgba().unwrap();
        assert_eq!((w, h), (36, 36));
        assert_eq!(rgba.len(), 36 * 36 * 4);
        // Template images are black; only alpha carries the shape.
        assert!(rgba.chunks(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0));
        assert!(rgba.chunks(4).any(|p| p[3] == 255));
    }
}
