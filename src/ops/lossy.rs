//! Lossy image recompression for `compress`.

use std::collections::HashSet;

use image::codecs::jpeg::JpegEncoder;
use image::{
    DynamicImage, ExtendedColorType, GrayImage, ImageFormat, RgbImage, imageops::FilterType,
};
use lopdf::{Document, Object, ObjectId, Stream};
use rayon::prelude::*;

use crate::doc;

/// Colour channels of an image, for the colour spaces a JPEG can stand in for.
fn channels(d: &Document, stream: &Stream) -> Option<u8> {
    match doc::resolve(d, stream.dict.get(b"ColorSpace").ok()?) {
        Object::Name(n) if n == b"DeviceGray" => Some(1),
        Object::Name(n) if n == b"DeviceRGB" => Some(3),
        Object::Array(a) => match a.first()?.as_name().ok()? {
            b"CalGray" => Some(1),
            b"CalRGB" => Some(3),
            b"ICCBased" => {
                let profile = doc::resolve(d, a.get(1)?).as_stream().ok()?;
                let n = doc::resolve(d, profile.dict.get(b"N").ok()?)
                    .as_i64()
                    .ok()?;
                matches!(n, 1 | 3).then_some(n as u8)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Re-encodes one image as JPEG, scaled to fit `max_edge`, if that makes it smaller.
fn shrink(d: &Document, stream: &Stream, quality: u8, max_edge: Option<u32>) -> Option<Stream> {
    let dict = &stream.dict;
    let int = |key: &[u8]| {
        dict.get(key)
            .ok()
            .and_then(|o| doc::resolve(d, o).as_i64().ok())
    };
    let flag = |key: &[u8]| {
        dict.get(key)
            .ok()
            .and_then(|o| doc::resolve(d, o).as_bool().ok())
            .unwrap_or(false)
    };
    // Masks and remapped sample values depend on exact pixel values, which JPEG does not keep.
    let exact = flag(b"ImageMask")
        || dict.has(b"Decode")
        || dict
            .get(b"Mask")
            .is_ok_and(|m| doc::resolve(d, m).as_array().is_ok())
        || int(b"SMaskInData").unwrap_or(0) != 0;
    if exact || int(b"BitsPerComponent") != Some(8) {
        return None;
    }
    let (width, height) = (
        u32::try_from(int(b"Width")?).ok()?,
        u32::try_from(int(b"Height")?).ok()?,
    );
    let channels = channels(d, stream)?;
    let filters: Vec<&[u8]> = match dict.get(b"Filter").ok().map(|f| doc::resolve(d, f)) {
        None => Vec::new(),
        Some(Object::Name(n)) => vec![n.as_slice()],
        Some(Object::Array(a)) => a.iter().filter_map(|o| o.as_name().ok()).collect(),
        Some(_) => return None,
    };
    let image = match filters.as_slice() {
        [b"DCTDecode"] => {
            let image =
                image::load_from_memory_with_format(&stream.content, ImageFormat::Jpeg).ok()?;
            // A JPEG whose channels disagree with the colour space (CMYK, for one) is left alone.
            (image.color().channel_count() == channels).then_some(image)?
        }
        [] | [b"FlateDecode"] => {
            let raw = if filters.is_empty() {
                stream.content.clone()
            } else {
                stream.decompressed_content().ok()?
            };
            if raw.len() != width as usize * height as usize * channels as usize {
                return None;
            }
            match channels {
                1 => DynamicImage::ImageLuma8(GrayImage::from_raw(width, height, raw)?),
                _ => DynamicImage::ImageRgb8(RgbImage::from_raw(width, height, raw)?),
            }
        }
        _ => return None,
    };
    let image = match max_edge {
        Some(edge) if image.width().max(image.height()) > edge => {
            image.resize(edge, edge, FilterType::CatmullRom)
        }
        _ => image,
    };

    let mut jpeg = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg, quality);
    match channels {
        1 => encoder.encode(
            image.to_luma8().as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::L8,
        ),
        _ => encoder.encode(
            image.to_rgb8().as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        ),
    }
    .ok()?;
    if jpeg.len() >= stream.content.len() {
        return None;
    }
    let mut dict = dict.clone();
    dict.set("Width", image.width() as i64);
    dict.set("Height", image.height() as i64);
    dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
    dict.remove(b"DecodeParms");
    // The payload is already compressed; deflating it again would only cost time.
    Some(Stream::new(dict, jpeg).with_compression(false))
}

/// Recompresses every eligible image and returns how many were replaced.
pub fn recompress_images(d: &mut Document, quality: u8, max_edge: Option<u32>) -> usize {
    // Soft masks carry alpha, where JPEG artefacts show up as halos.
    let masks: HashSet<ObjectId> = d
        .objects
        .values()
        .filter_map(|o| {
            o.as_stream()
                .ok()?
                .dict
                .get(b"SMask")
                .ok()?
                .as_reference()
                .ok()
        })
        .collect();
    let images: Vec<(ObjectId, &Stream)> = d
        .objects
        .iter()
        .filter_map(|(id, o)| Some((*id, o.as_stream().ok()?)))
        .filter(|(id, s)| {
            !masks.contains(id)
                && s.dict
                    .get(b"Subtype")
                    .is_ok_and(|t| t.as_name().ok() == Some(b"Image"))
        })
        .collect();
    let replaced: Vec<(ObjectId, Stream)> = images
        .par_iter()
        .filter_map(|&(id, s)| Some((id, shrink(d, s, quality, max_edge)?)))
        .collect();
    let count = replaced.len();
    for (id, stream) in replaced {
        d.objects.insert(id, Object::Stream(stream));
    }
    count
}
