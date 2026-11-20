//! The window icon, decoded from the PNG embedded in the binary.

use eframe::egui::IconData;

const PNG: &[u8] = include_bytes!("../../../assets/icon-256.png");

pub fn window_icon() -> Option<IconData> {
    decode(PNG)
}

fn decode(bytes: &[u8]) -> Option<IconData> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some(IconData {
        rgba: buf,
        width: info.width,
        height: info.height,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_png_is_256_rgba() {
        let icon = super::window_icon().expect("icon decodes");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        assert!(icon.rgba.chunks(4).any(|p| p[3] == 255));
        assert_eq!(icon.rgba[3], 0, "corner is transparent");
    }
}
