//! Usage: cargo run --release (from tools/make-icon). Writes into ../../assets.
use std::{error::Error, fs, path::PathBuf};

use resvg::{tiny_skia, usvg};

// Sizes up to this use the simplified drawing, where thin details would blur.
const SMALL_MAX: u32 = 24;
const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 128];
const PNG_SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];

fn render(svg: &str, size: u32) -> Result<tiny_skia::Pixmap, Box<dyn Error>> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default())?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("bad size")?;
    let scale = size as f32 / tree.size().width();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

fn main() -> Result<(), Box<dyn Error>> {
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let full = fs::read_to_string(assets.join("icon.svg"))?;
    let small = fs::read_to_string(assets.join("icon-small.svg"))?;
    let svg_for = |size: u32| if size <= SMALL_MAX { &small } else { &full };

    // Small sizes go to ICON_PREVIEW_DIR for eyeballing; only 256 and 512 are kept in assets.
    let preview = PathBuf::from(std::env::var("ICON_PREVIEW_DIR").unwrap_or_else(|_| ".".into()));
    fs::create_dir_all(&preview)?;
    for size in PNG_SIZES {
        let png = render(svg_for(size), size)?.encode_png()?;
        if size >= 256 {
            fs::write(assets.join(format!("icon-{size}.png")), &png)?;
        } else {
            fs::write(preview.join(format!("icon-{size}.png")), &png)?;
        }
    }

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in ICO_SIZES.into_iter().chain([256]) {
        let p = render(svg_for(size), size)?;
        let image = ico::IconImage::from_rgba_data(
            size,
            size,
            p.pixels()
                .iter()
                .flat_map(|px| {
                    let c = px.demultiply();
                    [c.red(), c.green(), c.blue(), c.alpha()]
                })
                .collect(),
        );
        // ico crate stores 256 as PNG and smaller sizes as BMP, which suits old shells.
        dir.add_entry(ico::IconDirEntry::encode(&image)?);
    }
    dir.write(fs::File::create(assets.join("icon.ico"))?)?;
    Ok(())
}
