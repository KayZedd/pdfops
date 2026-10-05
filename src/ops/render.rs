//! Commands that produce images: render (pages to PNG) and images (embedded images).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Args;
use hayro::hayro_interpret::font::GlyphRun;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    BlendMode, CacheKey, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageData,
    ImageDrawProps, InterpreterCache, InterpreterSettings, LumaData, SoftMask, interpret_page,
};
use hayro::hayro_syntax::object::{Array, Dict, Name};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{BezPath, Rect};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::progress::Progress;
use crate::{doc, pagespec};

/// Largest raster edge in pixels; the rasteriser addresses pixels with 16 bits.
const MAX_EDGE: f32 = 16000.0;
/// Largest raster in pixels. A page can claim to be kilometres wide; 64 megapixels
/// (256 MB of RGBA) is beyond what any reader of the image needs.
const MAX_PIXELS: f32 = 64.0e6;

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
    pub min_size: Option<u32>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

/// Pixels per point for a page of `w` by `h` points: the requested resolution,
/// lowered as far as the size caps demand.
pub(crate) fn raster_scale(w: f32, h: f32, dpi: f32) -> f32 {
    let (w, h) = (w.max(1.0), h.max(1.0));
    (dpi / 72.0)
        .min(MAX_EDGE / w.max(h))
        .min((MAX_PIXELS / (w * h)).sqrt())
}

/// Rasterises one page and returns PNG bytes with the pixel size.
pub fn page_png(
    page: &Page<'_>,
    settings: &InterpreterSettings,
    dpi: f32,
) -> Result<(Vec<u8>, u16, u16)> {
    let (w, h) = page.render_dimensions();
    let scale = raster_scale(w, h, dpi);
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        settings,
        &RenderSettings::default(),
        &hayro::PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let (width, height) = (pixmap.width(), pixmap.height());
    let png = pixmap
        .into_png()
        .map_err(|e| anyhow!("cannot encode page image: {e}"))?;
    Ok((png, width, height))
}

pub fn check_dpi(dpi: f32) -> Result<f32> {
    if !dpi.is_finite() || dpi <= 0.0 {
        bail!("dpi must be positive");
    }
    Ok(dpi)
}

pub fn render(a: RenderArgs) -> Result<Value> {
    let dpi = check_dpi(a.dpi.unwrap_or(150.0))?;
    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let all = pdf.pages();
    let pages = pagespec::parse_or_all(a.pages.as_deref(), all.len() as u32)?;
    std::fs::create_dir_all(&a.out_dir)?;

    let settings = InterpreterSettings::default();
    let progress = Progress::new("render", pages.len());
    let files: Vec<Value> = pages
        .par_iter()
        .map(|&n| {
            let (png, width, height) = page_png(&all[n as usize - 1], &settings, dpi)?;
            let path = a.out_dir.join(format!("page-{n:04}.png"));
            std::fs::write(&path, png)
                .with_context(|| format!("cannot write {}", path.display()))?;
            progress.tick(json!({"page": n, "file": path}));
            Ok(json!({"page": n, "file": path, "width": width, "height": height}))
        })
        .collect::<Result<_>>()?;
    Ok(json!({"dpi": dpi, "files": files}))
}

/// An image's identity with its width and height in pixels.
type Listed = (u128, u32, u32);

/// What a page's content stream does with each image it draws.
enum Mode<'m> {
    /// Record identity and size, without decoding.
    List(Vec<Listed>),
    /// Decode and write the images this page was assigned.
    Export {
        wanted: &'m HashMap<u128, PathBuf>,
        done: Vec<(u128, Result<PathBuf, String>)>,
    },
}

/// A drawing target that ignores everything except images.
///
/// Running the real interpreter finds images wherever they are drawn from
/// (page resources, nested forms, inline data) and decodes every filter and
/// colour space the renderer supports.
struct ImageSink<'m>(Mode<'m>);

impl<'a> Device<'a> for ImageSink<'_> {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_image(&mut self, image: Image<'a, '_>, _: ImageDrawProps<'a>) {
        let key = image.cache_key();
        match &mut self.0 {
            Mode::List(found) => {
                if !found.iter().any(|f| f.0 == key) {
                    found.push((key, image.width(), image.height()));
                }
            }
            Mode::Export { wanted, done } => {
                if let Some(stem) = wanted
                    .get(&key)
                    .filter(|_| !done.iter().any(|d| d.0 == key))
                {
                    done.push((key, export(&image, stem)));
                }
            }
        }
    }
}

fn interpret<'m>(page: &Page<'_>, mode: Mode<'m>) -> Mode<'m> {
    let (w, h) = page.render_dimensions();
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, w as f64, h as f64),
        &cache,
        page.xref(),
        InterpreterSettings::default(),
    );
    let mut sink = ImageSink(mode);
    interpret_page(page, &mut context, &mut sink);
    sink.0
}

fn write_png(
    path: &Path,
    width: u32,
    height: u32,
    color: png::ColorType,
    data: &[u8],
) -> Result<(), String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(data).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    std::fs::write(path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Interleaves an alpha plane into colour samples of `channels` bytes per pixel.
fn with_alpha(color: &[u8], channels: usize, alpha: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(color.len() + alpha.len());
    for (pixel, a) in color.chunks_exact(channels).zip(alpha) {
        out.extend_from_slice(pixel);
        out.push(*a);
    }
    out
}

/// The stream's filter name, when it has exactly one.
fn sole_filter(dict: &Dict<'_>) -> Option<Vec<u8>> {
    if let Some(name) = dict.get::<Name<'_>>(b"Filter") {
        return Some(name.as_ref().to_vec());
    }
    let mut names = dict.get::<Array<'_>>(b"Filter")?.iter::<Name<'_>>();
    let first = names.next()?;
    names.next().is_none().then(|| first.as_ref().to_vec())
}

/// Writes one image and returns its path, or why it cannot be exported.
fn export(image: &Image<'_, '_>, stem: &Path) -> Result<PathBuf, String> {
    let mut result = Err("cannot decode image data".to_string());
    match image {
        Image::Raster(raster) => {
            let stream = raster.stream();
            // JPEG and JPEG 2000 payloads are complete files already; copying keeps them lossless.
            let passthrough = match sole_filter(stream.dict()).as_deref() {
                Some(b"DCTDecode" | b"DCT") => Some("jpg"),
                Some(b"JPXDecode") => Some("jp2"),
                _ => None,
            };
            if let Some(ext) = passthrough {
                let path = stem.with_extension(ext);
                return std::fs::write(&path, stream.raw_data())
                    .map(|_| path.clone())
                    .map_err(|e| format!("cannot write {}: {e}", path.display()));
            }
            raster.with_rgba(
                |data, alpha| {
                    let (pixels, width, height, channels) = match &data {
                        ImageData::Rgb(rgb) => (&rgb.data, rgb.width, rgb.height, 3),
                        ImageData::Luma(luma) => (&luma.data, luma.width, luma.height, 1),
                    };
                    let count = width as usize * height as usize;
                    if pixels.len() != count * channels {
                        result = Err("unexpected image data size".to_string());
                        return;
                    }
                    let alpha = alpha.filter(|a| {
                        a.width == width && a.height == height && a.data.len() == count
                    });
                    let path = stem.with_extension("png");
                    let written = match (channels, alpha) {
                        (3, None) => write_png(&path, width, height, png::ColorType::Rgb, pixels),
                        (3, Some(a)) => write_png(
                            &path,
                            width,
                            height,
                            png::ColorType::Rgba,
                            &with_alpha(pixels, 3, &a.data),
                        ),
                        (_, None) => {
                            write_png(&path, width, height, png::ColorType::Grayscale, pixels)
                        }
                        (_, Some(a)) => write_png(
                            &path,
                            width,
                            height,
                            png::ColorType::GrayscaleAlpha,
                            &with_alpha(pixels, 1, &a.data),
                        ),
                    };
                    result = written.map(|_| path);
                },
                None,
            );
        }
        Image::Stencil(stencil) => stencil.with_stencil(
            |LumaData {
                 data,
                 width,
                 height,
                 ..
             },
             _| {
                let path = stem.with_extension("png");
                result = if data.len() == width as usize * height as usize {
                    write_png(&path, width, height, png::ColorType::Grayscale, &data).map(|_| path)
                } else {
                    Err("unexpected mask data size".to_string())
                };
            },
            None,
        ),
    }
    result
}

pub fn images(a: ImagesArgs) -> Result<Value> {
    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let all = pdf.pages();
    let pages = pagespec::parse_or_all(a.pages.as_deref(), all.len() as u32)?;
    let min = a.min_size.unwrap_or(1);
    std::fs::create_dir_all(&a.out_dir)?;

    let listed: Vec<(u32, Vec<Listed>)> = pages
        .par_iter()
        .map(
            |&n| match interpret(&all[n as usize - 1], Mode::List(Vec::new())) {
                Mode::List(found) => (n, found),
                Mode::Export { .. } => unreachable!("interpret returns the mode it was given"),
            },
        )
        .collect();

    // An image drawn on several pages is exported once, under the first page that uses it.
    let mut owner: HashMap<u128, (u32, u32, u32)> = HashMap::new();
    let mut jobs: Vec<(u32, HashMap<u128, PathBuf>)> = Vec::new();
    for (n, found) in listed {
        let mut wanted = HashMap::new();
        for (key, w, h) in found {
            if w >= min && h >= min && !owner.contains_key(&key) {
                owner.insert(key, (n, w, h));
                wanted.insert(
                    key,
                    a.out_dir
                        .join(format!("page-{n:04}-img-{:02}", wanted.len() + 1)),
                );
            }
        }
        if !wanted.is_empty() {
            jobs.push((n, wanted));
        }
    }

    let progress = Progress::new("images", jobs.len());
    let results: Vec<Value> = jobs
        .par_iter()
        .flat_map_iter(|(n, wanted)| {
            let done = match interpret(
                &all[*n as usize - 1],
                Mode::Export {
                    wanted,
                    done: Vec::new(),
                },
            ) {
                Mode::Export { done, .. } => done,
                Mode::List(_) => unreachable!("interpret returns the mode it was given"),
            };
            progress.tick(json!({"page": n, "images": done.len()}));
            let owner = &owner;
            done.into_iter().map(move |(key, outcome)| {
                let (_, w, h) = owner[&key];
                match outcome {
                    Ok(path) => json!({"page": n, "file": path, "width": w, "height": h}),
                    Err(reason) => json!({"page": n, "width": w, "height": h, "skipped": reason}),
                }
            })
        })
        .collect();
    let exported = results.iter().filter(|r| r.get("file").is_some()).count();
    Ok(json!({"exported": exported, "images": results}))
}

#[cfg(test)]
mod tests {
    use super::raster_scale;

    #[test]
    fn rasters_keep_the_requested_resolution_until_a_cap_applies() {
        // A4 at 150 dpi is far below every cap.
        assert!((raster_scale(595.0, 842.0, 150.0) - 150.0 / 72.0).abs() < 1e-6);
        // A page 20000 points square is held to 64 megapixels.
        let scale = raster_scale(20000.0, 20000.0, 600.0);
        assert!((20000.0 * scale).powi(2) <= 64.0e6 * 1.001 && 20000.0 * scale > 7900.0);
        // A long strip is held to the longest edge the rasteriser can address.
        assert!(100000.0 * raster_scale(100000.0, 10.0, 600.0) <= 16000.0 * 1.001);
        assert!(raster_scale(0.0, 0.0, 72.0).is_finite());
    }
}
