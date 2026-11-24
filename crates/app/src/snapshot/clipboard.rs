//! The snapshot image on the system clipboard.
//!
//! On Windows GPUI publishes an image only under its own registered `"PNG"`
//! format, which Paint, Firefox and the usual chat clients do not read, so a
//! paste anywhere but a GPUI app finds nothing. The image is published as a
//! `CF_DIB` bitmap there instead — the format those apps paste from. Everywhere
//! else GPUI's writer is already the native one.

use gpui_kit::App;
#[cfg(not(windows))]
use gpui_kit::{ClipboardItem, Image, ImageFormat};

/// Put `png` where an image paste in another application will find it. GPUI's
/// writer is not asked on Windows, for the reason in the module doc.
#[cfg(not(windows))]
pub(crate) fn copy_image(cx: &mut App, png: Vec<u8>) -> Result<(), String> {
    cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
        ImageFormat::Png,
        png,
    )));
    Ok(())
}

#[cfg(windows)]
pub(crate) fn copy_image(_cx: &mut App, png: Vec<u8>) -> Result<(), String> {
    let pixmap = resvg::tiny_skia::Pixmap::decode_png(&png)
        .map_err(|error| format!("the snapshot could not be read back: {error}"))?;
    publish(&dib(&pixmap))
}

/// A `CF_DIB` bitmap: a 40-byte `BITMAPINFOHEADER`, then bottom-up 32-bit BGRA
/// rows. Alpha is written opaque because the image is flat; an app that honours
/// the fourth byte would otherwise paste the text over a see-through card.
pub(crate) fn dib(pixmap: &resvg::tiny_skia::Pixmap) -> Vec<u8> {
    let width = pixmap.width();
    let height = pixmap.height();
    let image_size = width as usize * height as usize * 4;

    let mut out = Vec::with_capacity(40 + image_size);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(height as i32).to_le_bytes()); // biHeight, positive: bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression: BI_RGB
    out.extend_from_slice(&(image_size as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&[0u8; 16]); // resolution, palette and colours used

    for row in pixmap.pixels().chunks_exact(width as usize).rev() {
        for pixel in row {
            let color = pixel.demultiply();
            out.extend_from_slice(&[color.blue(), color.green(), color.red(), 0xff]);
        }
    }
    out
}

/// Hand the bytes to the clipboard and close it, even on failure: the clipboard
/// is one global that a failure must not leave locked.
#[cfg(windows)]
fn publish(bytes: &[u8]) -> Result<(), String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};

    unsafe { OpenClipboard(None) }.map_err(|error| format!("the clipboard is busy: {error}"))?;
    let written = set_dib(bytes);
    let _ = unsafe { CloseClipboard() };
    written.map_err(|error| format!("the snapshot could not be copied: {error}"))
}

#[cfg(windows)]
fn set_dib(bytes: &[u8]) -> windows::core::Result<()> {
    use windows::core::Owned;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{EmptyClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_DIB;

    unsafe {
        EmptyClipboard()?;
        let global = Owned::new(GlobalAlloc(GMEM_MOVEABLE, bytes.len())?);
        let target = GlobalLock(*global);
        if target.is_null() {
            return Err(windows::core::Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast(), bytes.len());
        GlobalUnlock(*global).ok();
        SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(global.0)))?;
        // The system owns the memory once `SetClipboardData` succeeds.
        std::mem::forget(global);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::dib;

    fn pixmap(pixels: &[[u8; 4]]) -> resvg::tiny_skia::Pixmap {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(pixels.len() as u32, 1).expect("a pixmap");
        for (cell, [r, g, b, a]) in pixmap.pixels_mut().iter_mut().zip(pixels.iter().copied()) {
            *cell = resvg::tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, a)
                .expect("a valid premultiplied pixel");
        }
        pixmap
    }

    fn header(bytes: &[u8]) -> (u32, i32, i32, u16, u16, u32) {
        let u32_at =
            |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        (
            u32_at(0),
            u32_at(4) as i32,
            u32_at(8) as i32,
            u16::from_le_bytes(bytes[12..14].try_into().unwrap()),
            u16::from_le_bytes(bytes[14..16].try_into().unwrap()),
            u32_at(16),
        )
    }

    /// The header is what an application reads first; every field has to be the
    /// one a 32-bit `BI_RGB` bitmap needs.
    #[test]
    fn the_header_describes_a_32_bit_bitmap() {
        let bytes = dib(&pixmap(&[[1, 2, 3, 255], [4, 5, 6, 255]]));
        assert_eq!(header(&bytes), (40, 2, 1, 1, 32, 0));
    }

    /// Rows go bottom-up, and the bytes are BGRA rather than the RGBA the
    /// renderer produces.
    #[test]
    fn the_rows_are_bottom_up_bgr() {
        let mut two_rows = resvg::tiny_skia::Pixmap::new(1, 2).expect("a pixmap");
        let red =
            resvg::tiny_skia::PremultipliedColorU8::from_rgba(255, 0, 0, 255).expect("a pixel");
        let blue =
            resvg::tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 255, 255).expect("a pixel");
        two_rows.pixels_mut()[0] = red;
        two_rows.pixels_mut()[1] = blue;

        let bytes = dib(&two_rows);
        assert_eq!(&bytes[40..44], &[0xff, 0x00, 0x00, 0xff], "blue first");
        assert_eq!(&bytes[44..48], &[0x00, 0x00, 0xff, 0xff], "red last");
    }

    /// A transparent pixel still reads as its colour, so the card is not
    /// pasted as a hole in the image.
    #[test]
    fn a_transparent_pixel_is_written_opaque() {
        let bytes = dib(&pixmap(&[[0, 0, 0, 0]]));
        assert_eq!(&bytes[40..44], &[0x00, 0x00, 0x00, 0xff]);
    }
}
