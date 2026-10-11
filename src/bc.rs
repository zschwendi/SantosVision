//! Preserve the original BC texture contents on Metal devices without BC support.
//! Decode only incoming upload regions; desktop/native BC devices keep GPU decoding.
use anyhow::{bail, Context, Result};
use wgpu::TextureFormat as T;

pub fn decoded_format(format: T) -> Option<T> {
    Some(match format {
        T::Bc1RgbaUnorm | T::Bc2RgbaUnorm | T::Bc3RgbaUnorm | T::Bc7RgbaUnorm => T::Rgba8Unorm,
        T::Bc1RgbaUnormSrgb | T::Bc2RgbaUnormSrgb | T::Bc3RgbaUnormSrgb | T::Bc7RgbaUnormSrgb => T::Rgba8UnormSrgb,
        T::Bc4RUnorm => T::R8Unorm, T::Bc4RSnorm => T::R8Snorm,
        T::Bc5RgUnorm => T::Rg8Unorm, T::Bc5RgSnorm => T::Rg8Snorm,
        T::Bc6hRgbUfloat | T::Bc6hRgbFloat => T::Rgba16Float,
        _ => return None,
    })
}

pub struct Upload { pub bytes: Vec<u8>, pub bytes_per_row: u32, pub rows_per_image: u32 }

#[allow(clippy::too_many_arguments)]
pub fn decode(format: T, data: &[u8], width: u32, height: u32, layers: u32,
              bytes_per_row: u32, rows_per_image: u32) -> Result<Upload> {
    let output_format = decoded_format(format).context("not a BC texture")?;
    let pixel_bytes = output_format.block_copy_size(None).context("decoded pixel size")? as usize;
    let block_bytes = format.block_copy_size(None).context("compressed block size")? as usize;
    let (w, h, layers) = (width as usize, height as usize, layers as usize);
    if w == 0 || h == 0 || layers == 0 { bail!("empty compressed upload"); }
    let (bx, by) = (w.div_ceil(4), h.div_ceil(4));
    let row_pitch = bytes_per_row as usize;
    let image_rows = rows_per_image.max(by as u32) as usize;
    if row_pitch < bx * block_bytes { bail!("compressed row pitch is too short"); }
    let needed = (layers - 1).checked_mul(row_pitch).and_then(|n| n.checked_mul(image_rows))
        .and_then(|n| n.checked_add((by - 1) * row_pitch + bx * block_bytes)).context("upload size overflow")?;
    if data.len() < needed { bail!("truncated compressed upload: {} < {needed}", data.len()); }
    let output_size = w.checked_mul(h).and_then(|n| n.checked_mul(layers)).and_then(|n| n.checked_mul(pixel_bytes))
        .context("decoded upload size overflow")?;
    let mut bytes = vec![0; output_size];
    for layer in 0..layers { for block_y in 0..by { for block_x in 0..bx {
        let offset = layer * row_pitch * image_rows + block_y * row_pitch + block_x * block_bytes;
        let input = &data[offset..offset + block_bytes];
        let mut block = [0u8; 128];
        match format {
            T::Bc1RgbaUnorm | T::Bc1RgbaUnormSrgb => bcdec_rs::bc1(input, &mut block, 16),
            T::Bc2RgbaUnorm | T::Bc2RgbaUnormSrgb => bcdec_rs::bc2(input, &mut block, 16),
            T::Bc3RgbaUnorm | T::Bc3RgbaUnormSrgb => bcdec_rs::bc3(input, &mut block, 16),
            T::Bc7RgbaUnorm | T::Bc7RgbaUnormSrgb => bcdec_rs::bc7(input, &mut block, 16),
            T::Bc4RUnorm | T::Bc4RSnorm => bcdec_rs::bc4(input, &mut block, 4, format == T::Bc4RSnorm),
            T::Bc5RgUnorm | T::Bc5RgSnorm => bcdec_rs::bc5(input, &mut block, 8, format == T::Bc5RgSnorm),
            T::Bc6hRgbUfloat | T::Bc6hRgbFloat => {
                let mut rgb = [0u16; 48];
                bcdec_rs::bc6h_half(input, &mut rgb, 12, format == T::Bc6hRgbFloat);
                for i in 0..16 { for c in 0..4 {
                    let half = if c == 3 { 0x3c00 } else { rgb[i * 3 + c] };
                    block[i * 8 + c * 2..i * 8 + c * 2 + 2].copy_from_slice(&half.to_le_bytes());
                } }
            }
            _ => unreachable!(),
        }
        for y in 0..4.min(h - block_y * 4) {
            let pixels = 4.min(w - block_x * 4);
            let dst = ((layer * h + block_y * 4 + y) * w + block_x * 4) * pixel_bytes;
            let src = y * 4 * pixel_bytes;
            bytes[dst..dst + pixels * pixel_bytes].copy_from_slice(&block[src..src + pixels * pixel_bytes]);
        }
    } } }
    Ok(Upload { bytes, bytes_per_row: width * pixel_bytes as u32, rows_per_image: height })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bc1_edges_padded_rows_and_array_layers() {
        // Uniform red and green 565 endpoints, with padding between block rows/layers.
        let mut input = vec![0; 64];
        for (off, color) in [(0, 0xf800u16), (8, 0xf800), (32, 0x07e0), (40, 0x07e0)] {
            input[off..off + 2].copy_from_slice(&color.to_le_bytes());
        }
        let upload = decode(T::Bc1RgbaUnorm, &input, 5, 3, 2, 16, 2).unwrap();
        assert_eq!((upload.bytes_per_row, upload.rows_per_image), (20, 3));
        assert_eq!(upload.bytes.len(), 120);
        assert!(upload.bytes[..60].chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
        assert!(upload.bytes[60..].chunks_exact(4).all(|p| p == [0, 255, 0, 255]));
    }
    #[test]
    fn rejects_short_payload_and_preserves_format_semantics() {
        assert!(decode(T::Bc3RgbaUnorm, &[0; 15], 4, 4, 1, 16, 1).is_err());
        assert_eq!(decoded_format(T::Bc7RgbaUnormSrgb), Some(T::Rgba8UnormSrgb));
        assert_eq!(decoded_format(T::Bc5RgSnorm), Some(T::Rg8Snorm));
        let hdr = decode(T::Bc6hRgbUfloat, &[0; 16], 4, 4, 1, 16, 1).unwrap();
        assert_eq!(hdr.bytes.len(), 128);
        assert!(hdr.bytes.chunks_exact(8).all(|p| p[6..8] == [0, 0x3c]));
    }
}
