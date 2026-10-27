//! CPU decodes of camera JPEGs, used only when the capture path delivered no decoded picture.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::vision::{Frame, LumaPlane};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// Decodes the luma plane only, into a buffer kept across frames.
#[derive(Default)]
pub struct LumaDecoder {
    plane: Option<LumaPlane>,
}

impl LumaDecoder {
    pub fn decode(&mut self, jpeg: &[u8]) -> Result<&LumaPlane> {
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::Luma);
        let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
        decoder
            .decode_headers()
            .context("reading the camera jpeg's headers")?;
        let info = decoder.info().context("the camera jpeg has no size")?;
        let len = decoder
            .output_buffer_size()
            .context("the camera jpeg is too large")?;
        let plane = self.plane.get_or_insert_with(|| LumaPlane {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        });
        plane.pixels.resize(len, 0);
        decoder
            .decode_into(&mut plane.pixels)
            .context("decoding the camera jpeg")?;
        plane.width = u32::from(info.width);
        plane.height = u32::from(info.height);
        Ok(plane)
    }
}

/// Full-size RGB for the classifier, decoded once per event candidate rather than per frame.
pub fn decode_rgb(jpeg: &[u8], captured_at: DateTime<Utc>) -> Result<Frame> {
    let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .context("decoding the camera jpeg")?
        .to_rgb8();
    Ok(Frame {
        width: image.width(),
        height: image.height(),
        rgb: image.into_raw(),
        captured_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg_of(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let frame = Frame {
            width,
            height,
            rgb: rgb.repeat((width * height) as usize),
            captured_at: Utc::now(),
        };
        crate::snapshot::frame_to_jpeg_bytes(&frame).unwrap()
    }

    #[test]
    fn luma_matches_the_bt601_luma_of_the_picture() {
        let jpeg = jpeg_of(64, 36, [200, 100, 50]);
        let mut decoder = LumaDecoder::default();
        let plane = decoder.decode(&jpeg).unwrap();
        assert_eq!((plane.width, plane.height), (64, 36));
        assert!(plane.is_well_formed());
        let expected = (200 * 299 + 100 * 587 + 50 * 114) / 1000;
        let got = plane.pixels[plane.pixels.len() / 2] as i32;
        assert!((got - expected).abs() <= 3, "luma {got} vs {expected}");
    }

    #[test]
    fn the_buffer_follows_the_picture_size() {
        let mut decoder = LumaDecoder::default();
        assert_eq!(
            decoder
                .decode(&jpeg_of(64, 36, [9; 3]))
                .unwrap()
                .pixels
                .len(),
            64 * 36
        );
        let plane = decoder.decode(&jpeg_of(16, 8, [9; 3])).unwrap();
        assert_eq!(
            (plane.width, plane.height, plane.pixels.len()),
            (16, 8, 128)
        );
    }

    #[test]
    fn rgb_decode_and_garbage() {
        let frame = decode_rgb(&jpeg_of(32, 18, [10, 200, 30]), Utc::now()).unwrap();
        assert_eq!((frame.width, frame.height), (32, 18));
        assert!(frame.is_well_formed());
        assert!(decode_rgb(b"not a jpeg", Utc::now()).is_err());
        assert!(LumaDecoder::default().decode(b"\xff\xd8garbage").is_err());
    }
}
