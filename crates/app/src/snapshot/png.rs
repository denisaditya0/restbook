//! SVG to PNG.
//!
//! The snapshot is described as SVG and rasterized here. This is the one place
//! the app depends on a renderer rather than on GPUI: a release build cannot
//! read pixels back from its own window, so an image has to be produced from
//! what the model says rather than captured from the screen.
//!
//! Fonts come from the system rather than from the binary: the SVG names a
//! monospace family, and `fontdb` resolves it the same way the OS would. A
//! machine with no such family still renders, just in whatever the fallback is.

/// Rasterize an SVG document to PNG bytes at `scale` (2 draws the image at
/// twice its logical size, which is what keeps text crisp on a retina display
/// and in a chat client that upscales).
pub(crate) fn render(svg: &str, scale: f32) -> Result<Vec<u8>, String> {
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_system_fonts();

    let tree = resvg::usvg::Tree::from_str(svg, &options)
        .map_err(|error| format!("the snapshot could not be parsed: {error}"))?;

    let scale = if scale > 0.0 { scale } else { 1.0 };
    let size = tree
        .size()
        .to_int_size()
        .scale_by(scale)
        .ok_or_else(|| "the snapshot is too large to draw at this size".to_owned())?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| "the snapshot is too large to draw".to_owned())?;

    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
        .encode_png()
        .map_err(|error| format!("the snapshot could not be encoded: {error}"))
}

#[cfg(test)]
mod tests {
    use super::render;

    const DOC: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"40\" height=\"20\">\
                       <rect width=\"40\" height=\"20\" fill=\"#101010\"/>\
                       <text x=\"4\" y=\"14\" font-family=\"monospace\" font-size=\"10\" fill=\"#eeeeee\">ok</text>\
                       </svg>";

    /// A PNG always starts with the same eight bytes; anything else is not one.
    #[test]
    fn the_output_is_a_png() {
        let bytes = render(DOC, 1.0).expect("the document rasterizes");
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    /// At 2x the pixel dimensions double, which is the whole point of the scale.
    #[test]
    fn a_scale_multiplies_the_pixels() {
        let i32_at = |bytes: &[u8], offset: usize| {
            i32::from_be_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ])
        };
        // IHDR width and height sit at these fixed offsets in a PNG.
        let one = render(DOC, 1.0).expect("rasterizes");
        let two = render(DOC, 2.0).expect("rasterizes");
        assert_eq!(i32_at(&two, 16), i32_at(&one, 16) * 2);
        assert_eq!(i32_at(&two, 20), i32_at(&one, 20) * 2);
    }

    /// A malformed document is an error the dialog can report, not a panic.
    #[test]
    fn a_broken_document_is_an_error() {
        assert!(render("<svg", 1.0).is_err());
    }
}
