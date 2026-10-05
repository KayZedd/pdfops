//! Commands that produce images: render (pages to PNG) and images (embedded images).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings};
use lopdf::{Document, Object, ObjectId, Stream};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{doc, pagespec};

/// Largest raster edge in pixels; the rasteriser addresses pixels with 16 bits.
const MAX_EDGE: f32 = 16000.0;

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct RenderArgs {
    /// PDF file to render
    pub input: PathBuf,
    /// Directory for the PNG files, created if missing
    #[arg(short, long)]
    pub out_dir: PathBuf,
    /// Pages to render, e.g. "1,3-5" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Resolution in dots per inch (default: 150)
    #[arg(long)]
    pub dpi: Option<f32>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct ImagesArgs {
    /// PDF file to extract images from
    pub input: PathBuf,
    /// Directory for the image files, created if missing
    #[arg(short, long)]
    pub out_dir: PathBuf,
    /// Pages to scan, e.g. "1-10" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Skip images narrower or shorter than this many pixels (default: 1)
    #[arg(long)]
    pub min_size: Option<i64>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

pub fn render(a: RenderArgs) -> Result<Value> {
    let dpi = a.dpi.unwrap_or(150.0);
    if !dpi.is_finite() || dpi <= 0.0 {
        bail!("dpi must be positive");
    }
    let bytes =
        std::fs::read(&a.input).with_context(|| format!("cannot read {}", a.input.display()))?;
    let pdf = Pdf::new_with_password(bytes, a.password.as_deref().unwrap_or(""))
        .map_err(|e| anyhow!("cannot open {}: {e:?}", a.input.display()))?;
    let all = pdf.pages();
    let pages = pagespec::parse_or_all(a.pages.as_deref(), all.len() as u32)?;
    std::fs::create_dir_all(&a.out_dir)?;

    let settings = InterpreterSettings::default();
    let files: Vec<Value> = pages
        .par_iter()
        .map(|&n| {
            let page = &all[n as usize - 1];
            let (w, h) = page.render_dimensions();
            let scale = (dpi / 72.0).min(MAX_EDGE / w.max(h).max(1.0));
            let pixmap = hayro::render(
                page,
                &RenderCache::new(),
                &settings,
                &RenderSettings::default(),
                &hayro::PixmapSettings {
                    x_scale: scale,
                    y_scale: scale,
                    bg_color: WHITE,
                },
            );
            let (width, height) = (pixmap.width(), pixmap.height());
            let path = a.out_dir.join(format!("page-{n:04}.png"));
            let png = pixmap
                .into_png()
                .map_err(|e| anyhow!("cannot encode page {n}: {e}"))?;
            std::fs::write(&path, png)
                .with_context(|| format!("cannot write {}", path.display()))?;
            Ok(json!({"page": n, "file": path, "width": width, "height": height}))
        })
        .collect::<Result<_>>()?;
    Ok(json!({"dpi": dpi, "files": files}))
}

/// Image XObjects referenced directly by a page's resources.
fn page_images(d: &Document, page: ObjectId) -> Vec<(ObjectId, &Stream)> {
    let xobjects = doc::inherited(d, page, b"Resources")
        .and_then(|r| r.as_dict().ok())
        .and_then(|r| r.get(b"XObject").ok())
        .and_then(|x| doc::resolve(d, x).as_dict().ok());
    let Some(xobjects) = xobjects else {
        return Vec::new();
    };
    xobjects
        .iter()
        .filter_map(|(_, v)| {
            let id = v.as_reference().ok()?;
            let stream = d.get_object(id).ok()?.as_stream().ok()?;
            (stream.dict.get(b"Subtype").ok()?.as_name().ok()? == b"Image").then_some((id, stream))
        })
        .collect()
}

fn filters(stream: &Stream) -> Vec<Vec<u8>> {
    match stream.dict.get(b"Filter") {
        Ok(Object::Name(n)) => vec![n.clone()],
        Ok(Object::Array(a)) => a
            .iter()
            .filter_map(|o| o.as_name().ok().map(<[u8]>::to_vec))
            .collect(),
        _ => Vec::new(),
    }
}

/// Writes one image and returns its file name, or why it cannot be exported.
fn export(d: &Document, stream: &Stream, stem: &Path) -> Result<PathBuf, String> {
    let filters = filters(stream);
    // JPEG and JPEG 2000 payloads are complete files already; copy them untouched.
    let passthrough = match filters.as_slice() {
        [f] if f == b"DCTDecode" => Some("jpg"),
        [f] if f == b"JPXDecode" => Some("jp2"),
        _ => None,
    };
    let path;
    let bytes;
    if let Some(ext) = passthrough {
        path = stem.with_extension(ext);
        bytes = stream.content.clone();
    } else {
        let int = |key: &[u8]| {
            stream
                .dict
                .get(key)
                .ok()
                .and_then(|o| doc::resolve(d, o).as_i64().ok())
        };
        let (w, h) = (int(b"Width").unwrap_or(0), int(b"Height").unwrap_or(0));
        if int(b"BitsPerComponent") != Some(8) {
            return Err("only 8-bit raw images are supported".into());
        }
        if w <= 0 || h <= 0 || w > u32::MAX as i64 || h > u32::MAX as i64 {
            return Err("invalid image size".into());
        }
        let raw = if filters.is_empty() {
            stream.content.clone()
        } else {
            stream
                .decompressed_content()
                .map_err(|e| format!("cannot decode: {e}"))?
        };
        let pixels = (w as usize)
            .checked_mul(h as usize)
            .ok_or("invalid image size")?;
        // The channel count follows from the data length, which also covers ICC based spaces.
        let (color, rgb) = match raw.len().checked_div(pixels) {
            Some(1) => (png::ColorType::Grayscale, raw),
            Some(3) => (png::ColorType::Rgb, raw),
            Some(4) => (
                png::ColorType::Rgb,
                raw.as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(cmyk_to_rgb)
                    .collect(),
            ),
            _ => return Err("unsupported colour space".into()),
        };
        let channels = if color == png::ColorType::Grayscale {
            1
        } else {
            3
        };
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, w as u32, h as u32);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&rgb[..pixels * channels])
            .map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
        path = stem.with_extension("png");
        bytes = out;
    }
    std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn cmyk_to_rgb(p: &[u8; 4]) -> [u8; 3] {
    let k = 255 - p[3] as u32;
    [0, 1, 2].map(|i| ((255 - p[i] as u32) * k / 255) as u8)
}

pub fn images(a: ImagesArgs) -> Result<Value> {
    let d = doc::load(&a.input, a.password.as_deref())?;
    let ids = doc::page_ids(&d);
    let pages = pagespec::parse_or_all(a.pages.as_deref(), ids.len() as u32)?;
    let min = a.min_size.unwrap_or(1);
    std::fs::create_dir_all(&a.out_dir)?;

    // An image reused on several pages is exported once, under its first page.
    let mut seen = HashSet::new();
    let mut jobs = Vec::new();
    for &n in &pages {
        for (id, stream) in page_images(&d, ids[n as usize - 1]) {
            if seen.insert(id) {
                jobs.push((n, id, stream));
            }
        }
    }
    let results: Vec<Value> = jobs
        .par_iter()
        .filter_map(|&(n, id, stream)| {
            let dim = |key: &[u8]| {
                stream
                    .dict
                    .get(key)
                    .ok()
                    .and_then(|o| doc::resolve(&d, o).as_i64().ok())
                    .unwrap_or(0)
            };
            let (w, h) = (dim(b"Width"), dim(b"Height"));
            if w < min || h < min {
                return None;
            }
            let stem = a.out_dir.join(format!("page-{n:04}-img-{}", id.0));
            Some(match export(&d, stream, &stem) {
                Ok(path) => json!({"page": n, "file": path, "width": w, "height": h}),
                Err(reason) => json!({"page": n, "width": w, "height": h, "skipped": reason}),
            })
        })
        .collect();
    let exported = results.iter().filter(|r| r.get("file").is_some()).count();
    Ok(json!({"exported": exported, "images": results}))
}
