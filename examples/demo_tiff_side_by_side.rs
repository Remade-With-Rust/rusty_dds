//! Headful side-by-side demo: **.TIF → .DDS → decode**
//!
//! Left: source TIFF (as RGBA)  
//! Center: rusty_dds encode → decode  
//! Right: Microsoft DirectXTex Compress → Decompress (if harness built)
//!
//! ```text
//! cargo run --release --example demo_tiff_side_by_side -- [path/to/image.tif]
//! ```
//!
//! Optional: build `dxtex_roundtrip` under `tools/dxtex_decode_bench` for the
//! Microsoft column. Drag-drop or Open… accepts `.tif` / `.tiff`.

use eframe::egui;
use rusty_dds::{
    max_abs_diff, psnr_rgba8, DecodeContent, Dds, EncodeLayout, SubresourceId,
};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let initial = args.first().map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 780.0])
            .with_title("rusty_dds — TIFF → DDS side-by-side vs DirectXTex"),
        ..Default::default()
    };
    eframe::run_native(
        "rusty_dds TIFF demo",
        options,
        Box::new(move |cc| Ok(Box::new(DemoApp::new(cc, initial)))),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FmtChoice {
    Bc1,
    Bc3,
    Bc7,
    Rgba8,
}

impl FmtChoice {
    fn label(self) -> &'static str {
        match self {
            Self::Bc1 => "BC1",
            Self::Bc3 => "BC3",
            Self::Bc7 => "BC7",
            Self::Rgba8 => "RGBA8",
        }
    }
    fn content(self) -> DecodeContent {
        match self {
            Self::Bc1 => DecodeContent::Bc1,
            Self::Bc3 => DecodeContent::Bc3,
            Self::Bc7 => DecodeContent::Bc7,
            Self::Rgba8 => DecodeContent::Rgba8,
        }
    }
    fn dxgi(self) -> &'static str {
        match self {
            Self::Bc1 => "BC1_UNORM",
            Self::Bc3 => "BC3_UNORM",
            Self::Bc7 => "BC7_UNORM",
            Self::Rgba8 => "R8G8B8A8_UNORM",
        }
    }
}

struct Panel {
    title: String,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    texture: Option<egui::TextureHandle>,
    encode_ns: Option<f64>,
    decode_ns: Option<f64>,
    dds_bytes: Option<usize>,
    psnr_db: Option<f64>,
    max_abs: Option<u8>,
    note: String,
}

impl Panel {
    fn ensure_texture(&mut self, ctx: &egui::Context, id: &str) {
        if self.texture.is_some() || self.rgba.is_empty() {
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [self.width as usize, self.height as usize],
            &self.rgba,
        );
        self.texture = Some(ctx.load_texture(id, image, egui::TextureOptions::NEAREST));
    }
}

struct DemoApp {
    root: PathBuf,
    source_path: Option<PathBuf>,
    fmt: FmtChoice,
    status: String,
    source: Option<Panel>,
    rusty: Option<Panel>,
    dxtex: Option<Panel>,
    last_dds_path: Option<PathBuf>,
    dirty: bool,
}

impl DemoApp {
    fn new(_cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = Self {
            root,
            source_path: None,
            fmt: FmtChoice::Bc7,
            status: "Open a .tif / .tiff, or use the bundled sample.".into(),
            source: None,
            rusty: None,
            dxtex: None,
            last_dds_path: None,
            dirty: false,
        };
        if let Some(p) = initial {
            app.load_tiff(&p);
        } else {
            let sample = app.root.join("examples/assets/demo_sample.tif");
            if !sample.exists() {
                if let Err(e) = write_sample_tiff(&sample) {
                    app.status = format!("Could not write sample TIFF: {e}");
                }
            }
            if sample.exists() {
                app.load_tiff(&sample);
            }
        }
        app
    }

    fn load_tiff(&mut self, path: &Path) {
        match load_tiff_rgba8(path) {
            Ok((w, h, rgba)) => {
                self.source_path = Some(path.to_path_buf());
                self.source = Some(Panel {
                    title: format!("Source TIFF — {}", path.file_name().unwrap().to_string_lossy()),
                    rgba,
                    width: w,
                    height: h,
                    texture: None,
                    encode_ns: None,
                    decode_ns: None,
                    dds_bytes: None,
                    psnr_db: None,
                    max_abs: None,
                    note: format!("{w}×{h} RGBA8"),
                });
                self.rusty = None;
                self.dxtex = None;
                self.dirty = true;
                self.status = format!("Loaded {}", path.display());
            }
            Err(e) => {
                self.status = format!("TIFF load failed: {e}");
            }
        }
    }

    fn run_pipeline(&mut self) {
        let Some(src) = self.source.as_ref() else {
            self.status = "Load a TIFF first.".into();
            return;
        };
        let w = src.width;
        let h = src.height;
        let pixels = src.rgba.clone();
        let content = self.fmt.content();

        // --- rusty_dds ---
        let layout = EncodeLayout::flat_2d(content, w, h);
        let t0 = Instant::now();
        let dds = match Dds::encode_from_rgba8(&pixels, layout) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("rusty_dds encode failed: {e}");
                return;
            }
        };
        let encode_ns = t0.elapsed().as_nanos() as f64;
        let dds_bytes = dds.data.len();

        let out_dir = self.root.join("target/demo_tiff");
        let _ = fs::create_dir_all(&out_dir);
        let dds_path = out_dir.join(format!("rusty_{}.dds", self.fmt.label().to_lowercase()));
        {
            let mut f = File::create(&dds_path).unwrap();
            dds.write(&mut f).unwrap();
            let _ = f.flush();
        }
        self.last_dds_path = Some(dds_path.clone());

        let t1 = Instant::now();
        let img = match dds.decode_rgba8(SubresourceId::mip_layer(0, 0)) {
            Ok(i) => i,
            Err(e) => {
                self.status = format!("rusty_dds decode failed: {e}");
                return;
            }
        };
        let decode_ns = t1.elapsed().as_nanos() as f64;
        let psnr = psnr_rgba8(&img.pixels, &pixels);
        let mad = max_abs_diff(&img.pixels, &pixels);

        self.rusty = Some(Panel {
            title: format!("rusty_dds — {} encode → decode", self.fmt.label()),
            rgba: img.pixels,
            width: img.width,
            height: img.height,
            texture: None,
            encode_ns: Some(encode_ns),
            decode_ns: Some(decode_ns),
            dds_bytes: Some(dds_bytes),
            psnr_db: psnr.filter(|p| p.is_finite()),
            max_abs: mad,
            note: format!("wrote {}", dds_path.display()),
        });

        // --- DirectXTex (optional) ---
        self.dxtex = run_dxtex_roundtrip(
            &self.root,
            &out_dir,
            &pixels,
            w,
            h,
            self.fmt,
            &pixels,
        );

        self.dirty = false;
        let rusty_rt = encode_ns + decode_ns;
        let msg = if let Some(dx) = &self.dxtex {
            let dx_rt = dx.encode_ns.unwrap_or(0.0) + dx.decode_ns.unwrap_or(0.0);
            if dx_rt > 0.0 {
                format!(
                    "Round-trip: rusty_dds {:.2} ms vs DirectXTex {:.2} ms ({:.1}× faster)",
                    rusty_rt / 1e6,
                    dx_rt / 1e6,
                    dx_rt / rusty_rt
                )
            } else {
                format!("rusty_dds round-trip {:.2} ms — DirectXTex: {}", rusty_rt / 1e6, dx.note)
            }
        } else {
            format!(
                "rusty_dds round-trip {:.2} ms — DirectXTex harness not found",
                rusty_rt / 1e6
            )
        };
        self.status = msg;
    }
}

impl eframe::App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drag-drop
        ctx.input(|i| {
            for f in &i.raw.dropped_files {
                if let Some(path) = &f.path {
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if ext == "tif" || ext == "tiff" {
                        return Some(path.clone());
                    }
                }
            }
            None
        })
        .map(|p| self.load_tiff(&p));

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("TIFF → DDS → decode");
                ui.separator();
                if ui.button("Open TIFF…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("TIFF", &["tif", "tiff"])
                        .pick_file()
                    {
                        self.load_tiff(&path);
                    }
                }
                if ui.button("Sample TIFF").clicked() {
                    let sample = self.root.join("examples/assets/demo_sample.tif");
                    let _ = write_sample_tiff(&sample);
                    self.load_tiff(&sample);
                }
                ui.separator();
                ui.label("Format:");
                for f in [
                    FmtChoice::Bc7,
                    FmtChoice::Bc3,
                    FmtChoice::Bc1,
                    FmtChoice::Rgba8,
                ] {
                    if ui
                        .selectable_label(self.fmt == f, f.label())
                        .clicked()
                    {
                        self.fmt = f;
                        self.dirty = true;
                    }
                }
                ui.separator();
                let run = ui
                    .add_enabled(self.source.is_some(), egui::Button::new("Encode → Decode"))
                    .clicked()
                    || self.dirty;
                if run && self.source.is_some() {
                    self.run_pipeline();
                }
            });
            ui.label(&self.status);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_size();
            let col_w = (avail.x - 16.0) / 3.0;
            ui.horizontal(|ui| {
                show_panel(ui, ctx, &mut self.source, "src", col_w, avail.y - 8.0);
                show_panel(ui, ctx, &mut self.rusty, "rusty", col_w, avail.y - 8.0);
                show_panel(ui, ctx, &mut self.dxtex, "dxtex", col_w, avail.y - 8.0);
            });
        });
    }
}

fn show_panel(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    panel: &mut Option<Panel>,
    tex_id: &str,
    width: f32,
    height: f32,
) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_size(egui::vec2(width - 8.0, height - 8.0));
                match panel {
                    None => {
                        ui.centered_and_justified(|ui| {
                            ui.label(egui::RichText::new("—").weak());
                        });
                    }
                    Some(p) => {
                        p.ensure_texture(ctx, tex_id);
                        ui.label(egui::RichText::new(&p.title).strong());
                        ui.label(&p.note);
                        if let (Some(e), Some(d)) = (p.encode_ns, p.decode_ns) {
                            ui.monospace(format!(
                                "encode {:>8.2} µs   decode {:>8.2} µs   Σ {:>8.2} µs",
                                e / 1e3,
                                d / 1e3,
                                (e + d) / 1e3
                            ));
                        } else if let Some(e) = p.encode_ns {
                            ui.monospace(format!("encode {:>8.2} µs", e / 1e3));
                        }
                        if let Some(b) = p.dds_bytes {
                            ui.monospace(format!("DDS payload {b} bytes"));
                        }
                        if let Some(psnr) = p.psnr_db {
                            ui.monospace(format!(
                                "PSNR vs source {psnr:.2} dB   max|Δ|={}",
                                p.max_abs.map(|m| m.to_string()).unwrap_or_else(|| "—".into())
                            ));
                        } else if p.encode_ns.is_none() && p.decode_ns.is_none() {
                            // source panel
                        } else if p.psnr_db.is_none() && p.max_abs == Some(0) {
                            ui.monospace("bit-exact vs source");
                        }
                        ui.add_space(6.0);
                        if let Some(tex) = &p.texture {
                            let max_w = ui.available_width() - 8.0;
                            let max_h = ui.available_height() - 8.0;
                            let aspect = p.width as f32 / p.height as f32;
                            let mut disp_w = max_w;
                            let mut disp_h = disp_w / aspect;
                            if disp_h > max_h {
                                disp_h = max_h;
                                disp_w = disp_h * aspect;
                            }
                            ui.image((tex.id(), egui::vec2(disp_w, disp_h)));
                        }
                    }
                }
            });
        },
    );
}

fn run_dxtex_roundtrip(
    root: &Path,
    out_dir: &Path,
    pixels: &[u8],
    w: u32,
    h: u32,
    fmt: FmtChoice,
    source: &[u8],
) -> Option<Panel> {
    let exe = [
        root.join("tools/dxtex_decode_bench/build/dxtex_roundtrip.exe"),
        root.join("tools/dxtex_decode_bench/build/Release/dxtex_roundtrip.exe"),
        root.join("tools/dxtex_decode_bench/build/dxtex_roundtrip"),
    ]
    .into_iter()
    .find(|p| p.exists())?;

    let in_rgba = out_dir.join("dx_in.rgba");
    let out_rgba = out_dir.join("dx_out.rgba");
    let out_json = out_dir.join("dx_roundtrip.json");
    fs::write(&in_rgba, pixels).ok()?;

    let status = Command::new(&exe)
        .arg(&in_rgba)
        .arg(w.to_string())
        .arg(h.to_string())
        .arg(fmt.dxgi())
        .arg(&out_rgba)
        .arg(&out_json)
        .status()
        .ok()?;
    if !status.success() {
        return Some(Panel {
            title: "Microsoft DirectXTex".into(),
            rgba: Vec::new(),
            width: w,
            height: h,
            texture: None,
            encode_ns: None,
            decode_ns: None,
            dds_bytes: None,
            psnr_db: None,
            max_abs: None,
            note: "roundtrip harness failed".into(),
        });
    }

    let rgba = fs::read(&out_rgba).ok()?;
    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_json).ok()?).ok()?;
    let encode_ns = meta["encode_ns"].as_f64();
    let decode_ns = meta["decode_ns"].as_f64();
    let dds_bytes = meta["encoded_bytes"].as_u64().map(|v| v as usize);
    let psnr = psnr_rgba8(&rgba, source);
    let mad = max_abs_diff(&rgba, source);

    Some(Panel {
        title: format!("DirectXTex — {} Compress → Decompress", fmt.label()),
        rgba,
        width: w,
        height: h,
        texture: None,
        encode_ns,
        decode_ns,
        dds_bytes,
        psnr_db: psnr.filter(|p| p.is_finite()),
        max_abs: mad,
        note: "CPU Compress (BC7=QUICK)".into(),
    })
}

fn load_tiff_rgba8(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut dec = tiff::decoder::Decoder::new(BufReader::new(file)).map_err(|e| e.to_string())?;
    let (w, h) = dec.dimensions().map_err(|e| e.to_string())?;
    let color = dec.colortype().map_err(|e| e.to_string())?;
    let img = dec.read_image().map_err(|e| e.to_string())?;

    use tiff::decoder::DecodingResult;
    use tiff::ColorType;

    let rgba = match (color, img) {
        (ColorType::RGB(8), DecodingResult::U8(data)) => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for chunk in data.chunks_exact(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            out
        }
        (ColorType::RGBA(8), DecodingResult::U8(data)) => data,
        (ColorType::Gray(8), DecodingResult::U8(data)) => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for g in data {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        (other, _) => {
            return Err(format!(
                "unsupported TIFF color type {other:?} (need 8-bit Gray/RGB/RGBA)"
            ));
        }
    };
    if rgba.len() != (w * h * 4) as usize {
        return Err(format!(
            "pixel buffer size mismatch: got {} want {}",
            rgba.len(),
            w * h * 4
        ));
    }
    // Opaque-aware for BC1 demos: leave alpha as-is.
    Ok((w, h, rgba))
}

fn write_sample_tiff(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    const W: u32 = 512;
    const H: u32 = 512;
    let mut rgb = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            // Photo-ish synthetic: warm gradient + diagonal stripe + soft vignette.
            let stripe = (((x + y) / 32) % 2) as f32;
            let vig = 1.0 - 0.35 * ((fx - 0.5).powi(2) + (fy - 0.5).powi(2)).sqrt() * 2.0;
            let r = ((0.15 + 0.75 * fx + 0.1 * stripe) * vig * 255.0).clamp(0.0, 255.0) as u8;
            let g = ((0.20 + 0.55 * fy + 0.15 * (1.0 - stripe)) * vig * 255.0).clamp(0.0, 255.0)
                as u8;
            let b = ((0.55 + 0.35 * (1.0 - fx) + 0.1 * fy) * vig * 255.0).clamp(0.0, 255.0) as u8;
            rgb.extend_from_slice(&[r, g, b]);
        }
    }
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut enc = tiff::encoder::TiffEncoder::new(BufWriter::new(file)).map_err(|e| e.to_string())?;
    enc.write_image::<tiff::encoder::colortype::RGB8>(W, H, &rgb)
        .map_err(|e| e.to_string())?;
    Ok(())
}
