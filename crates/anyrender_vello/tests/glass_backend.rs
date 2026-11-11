//! Integration tests for the glass backend slot of the Vello renderers.

use std::sync::{Arc, Mutex, OnceLock};

use anyrender::glass::{GlassGroup, GlassMaterial, GlassShape, GlassVariant, ShapeKind};
use anyrender::{ImageRenderer, PaintScene, WindowRenderer};
use anyrender_vello::glass::{FrameInputs, GlassBackend, GlassRegions};
use anyrender_vello::{VelloImageRenderer, VelloScenePainter, VelloWindowRenderer};
use kurbo::{Affine, Rect, RoundedRectRadii};
use peniko::{Color, Fill};
use vello::glass::{GlassFrame, GlassPasses, GlassPhase, MarkerPlacement};
use wgpu::util::DeviceExt;
use wgpu::{CommandEncoder, Device, Queue};

const SIZE: u32 = 128;
const BACKGROUND: Color = Color::from_rgb8(0, 128, 255);
/// The test backend grows each group's box by this much for its region.
const GROW: f64 = 10.4;

/// What the test backend saw.
#[derive(Debug, Default)]
struct Calls {
    begin_frames: usize,
    groups: Vec<Arc<GlassGroup>>,
    inputs: Vec<FrameInputs>,
    frames: Vec<(Vec<MarkerPlacement>, u32)>,
    records: Vec<u32>,
}

/// A backend whose region is the group's box grown by [`GROW`] and whose composite is the box.
struct Fake {
    calls: Arc<Mutex<Calls>>,
    /// Pushes the composite's right edge 20 px past the region.
    poke_out: bool,
    /// Writes solid magenta into each glass slot.
    magenta: bool,
    redraw: bool,
}

impl Fake {
    fn new() -> (Self, Arc<Mutex<Calls>>) {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let fake = Self {
            calls: Arc::clone(&calls),
            poke_out: false,
            magenta: false,
            redraw: false,
        };
        (fake, calls)
    }
}

fn group_box(group: &GlassGroup, scale: f64) -> Rect {
    group
        .shapes
        .iter()
        .map(|shape| {
            let ShapeKind::RoundedRect { rect, .. } = &shape.kind else {
                panic!("the test backend handles rounded rectangles only");
            };
            (Affine::scale(scale) * shape.transform).transform_rect_bbox(*rect)
        })
        .reduce(|a, b| a.union(b))
        .expect("draw_glass skips empty groups")
}

impl GlassPasses for Fake {
    fn prepare(&mut self, _device: &Device, _queue: &Queue, frame: &GlassFrame<'_>) {
        let mut calls = self.calls.lock().unwrap();
        calls.frames.push((frame.markers.to_vec(), frame.phases));
    }

    fn record(&mut self, device: &Device, encoder: &mut CommandEncoder, phase: &GlassPhase<'_>) {
        self.calls.lock().unwrap().records.push(phase.index);
        if !self.magenta {
            return;
        }
        assert_eq!(phase.glass_atlas.format(), wgpu::TextureFormat::Rgba16Float);
        // Half-float 1.0 and 0.0: opaque premultiplied magenta.
        let (one, zero) = (0x3c00_u16.to_le_bytes(), [0, 0]);
        let texel = [one, zero, one, one].concat();
        for marker in phase.markers {
            let c = marker.composite;
            let (width, height) = ((c.x1 - c.x0) as u32, (c.y1 - c.y0) as u32);
            let row = (width * 8).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let mut data = vec![0_u8; (row * height) as usize];
            for y in 0..height {
                for x in 0..width {
                    let at = (y * row + x * 8) as usize;
                    data[at..at + 8].copy_from_slice(&texel);
                }
            }
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("test.magenta"),
                contents: &data,
                usage: wgpu::BufferUsages::COPY_SRC,
            });
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: phase.glass_atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: marker.glass_origin.0,
                        y: marker.glass_origin.1,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

impl GlassBackend for Fake {
    fn regions(&self, group: &GlassGroup, scale: f64) -> GlassRegions {
        let composite = group_box(group, scale);
        let region = composite.inflate(GROW, GROW);
        let composite = if self.poke_out {
            Rect {
                x1: region.x1 + 20.0,
                ..composite
            }
        } else {
            composite
        };
        GlassRegions { region, composite }
    }

    fn begin_frame(&mut self, groups: &[Arc<GlassGroup>], inputs: &FrameInputs) {
        let mut calls = self.calls.lock().unwrap();
        calls.begin_frames += 1;
        calls.groups.extend(groups.iter().cloned());
        calls.inputs.push(*inputs);
    }

    fn wants_redraw(&self) -> bool {
        self.redraw
    }
}

fn rect_group(rect: Rect) -> GlassGroup {
    GlassGroup::new(vec![GlassShape::new(
        0,
        Affine::IDENTITY,
        ShapeKind::RoundedRect {
            rect,
            radii: RoundedRectRadii::from_single_radius(4.0),
            corner: anyrender::glass::CornerStyle::Circular,
        },
        GlassMaterial::new(GlassVariant::Regular),
    )])
}

fn background(painter: &mut impl PaintScene) {
    painter.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        BACKGROUND,
        None,
        &Rect::new(0.0, 0.0, f64::from(SIZE), f64::from(SIZE)),
    );
}

/// Returns an image renderer, or `None` with a logged reason when there is no GPU adapter.
fn image_renderer() -> Option<VelloImageRenderer> {
    let context = wgpu_context::WGPUContext::new();
    let adapter = pollster::block_on(
        context
            .instance
            .request_adapter(&wgpu::RequestAdapterOptions::default()),
    );
    if let Err(error) = adapter {
        eprintln!("skipping: no GPU adapter ({error})");
        return None;
    }
    Some(VelloImageRenderer::new(SIZE, SIZE))
}

fn render(
    renderer: &mut VelloImageRenderer,
    draw: impl FnOnce(&mut VelloScenePainter<'_, '_>),
) -> Vec<u8> {
    let mut pixels = Vec::new();
    renderer.render_to_vec(draw, &mut pixels);
    pixels
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * SIZE + x) * 4) as usize;
    pixels[at..at + 4].try_into().unwrap()
}

/// Log messages, captured for every test in this binary.
fn logged() -> &'static Mutex<Vec<String>> {
    static LOGGED: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    static LOGGER: Capture = Capture;
    struct Capture;
    impl log::Log for Capture {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            logged().lock().unwrap().push(record.args().to_string());
        }
        fn flush(&self) {}
    }
    LOGGED.get_or_init(|| {
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Warn);
        Mutex::new(Vec::new())
    })
}

#[test]
fn renderers_are_send() {
    fn _assert_send<T: Send>() {}
    _assert_send::<VelloImageRenderer>();
    _assert_send::<VelloWindowRenderer>();
}

#[test]
fn draw_glass_without_a_backend_fills_the_tint() {
    let mut scene = vello::Scene::new();
    let mut painter = VelloScenePainter::new(&mut scene);
    assert!(!painter.supports_glass());
    painter.draw_glass(&rect_group(Rect::new(10.0, 10.0, 50.0, 50.0)));
    assert!(
        !scene.encoding().is_empty(),
        "the tint fallback fills the shape"
    );
    assert!(scene.encoding().glass.is_empty());
}

#[test]
fn window_renderer_wants_redraw_when_its_backend_does() {
    let mut renderer = VelloWindowRenderer::new();
    assert!(!renderer.wants_redraw());
    let (mut fake, _) = Fake::new();
    fake.redraw = true;
    renderer.set_glass_backend(Box::new(fake));
    assert!(renderer.wants_redraw());
}

#[test]
fn empty_group_pushes_no_marker() {
    let Some(mut renderer) = image_renderer() else {
        return;
    };
    let (fake, calls) = Fake::new();
    renderer.set_glass_backend(Box::new(fake));
    render(&mut renderer, |painter| {
        assert!(painter.supports_glass());
        background(painter);
        painter.draw_glass(&GlassGroup::new(Vec::new()));
    });
    assert_eq!(calls.lock().unwrap().begin_frames, 0);

    render(&mut renderer, |painter| {
        painter.draw_glass(&GlassGroup::new(Vec::new()));
        painter.draw_glass(&rect_group(Rect::new(16.0, 16.0, 48.0, 48.0)));
    });
    let calls = calls.lock().unwrap();
    assert_eq!(calls.begin_frames, 1);
    assert_eq!(calls.groups.len(), 1, "the empty group isn't handed over");
    let (markers, _) = &calls.frames[0];
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].id, 0, "the first non-empty group is marker 0");
}

#[test]
fn draw_glass_reads_the_scale_from_the_frame_inputs() {
    let Some(mut renderer) = image_renderer() else {
        return;
    };
    let (fake, calls) = Fake::new();
    renderer.set_glass_backend(Box::new(fake));
    renderer.set_glass_frame_inputs(FrameInputs::new(1.5, 2.0));
    render(&mut renderer, |painter| {
        painter.draw_glass(&rect_group(Rect::new(8.0, 8.0, 24.0, 24.0)));
    });
    let calls = calls.lock().unwrap();
    assert_eq!(calls.inputs, vec![FrameInputs::new(1.5, 2.0)]);
    let (markers, _) = &calls.frames[0];
    // The box (8, 8, 24, 24) at scale 2 is (16, 16, 48, 48), already on tiles.
    let c = markers[0].composite;
    assert_eq!((c.x0, c.y0, c.x1, c.y1), (16, 16, 48, 48));
}

#[test]
fn draw_glass_clamps_a_composite_outside_its_region() {
    let Some(mut renderer) = image_renderer() else {
        return;
    };
    logged();
    let (mut fake, calls) = Fake::new();
    fake.poke_out = true;
    renderer.set_glass_backend(Box::new(fake));
    render(&mut renderer, |painter| {
        background(painter);
        painter.draw_glass(&rect_group(Rect::new(16.0, 16.0, 48.0, 48.0)));
        painter.draw_glass(&rect_group(Rect::new(64.0, 64.0, 96.0, 96.0)));
    });
    let calls = calls.lock().unwrap();
    let (markers, _) = &calls.frames[0];
    assert_eq!(markers.len(), 2);
    for m in markers {
        let (r, c) = (m.region, m.composite);
        assert!(
            r.x0 <= c.x0 && r.y0 <= c.y0 && c.x1 <= r.x1 && c.y1 <= r.y1,
            "{m:?}"
        );
    }
    let outside = logged()
        .lock()
        .unwrap()
        .iter()
        .filter(|message| message.starts_with("glass.regions.composite_outside"))
        .count();
    // Once per frame, though both groups poke out. Other tests don't poke out.
    assert_eq!(outside, 1);
}

#[test]
fn frame_without_glass_with_a_backend_is_stock() {
    let Some(mut renderer) = image_renderer() else {
        return;
    };
    let stock = render(&mut renderer, |painter| background(painter));
    let (fake, calls) = Fake::new();
    renderer.set_glass_backend(Box::new(fake));
    let with_backend = render(&mut renderer, |painter| background(painter));
    assert!(
        stock == with_backend,
        "a frame without glass renders as stock"
    );
    assert_eq!(calls.lock().unwrap().begin_frames, 0);
    assert!(
        !renderer
            .vello_renderer()
            .unwrap()
            .glass_pipelines_compiled()
    );
}

#[test]
fn glass_frame_calls_the_backend_once_per_phase() {
    let Some(mut renderer) = image_renderer() else {
        return;
    };
    let (fake, calls) = Fake::new();
    renderer.set_glass_backend(Box::new(fake));
    render(&mut renderer, |painter| {
        background(painter);
        painter.draw_glass(&rect_group(Rect::new(16.0, 16.0, 48.0, 48.0)));
        // Its region meets the first composite, so it needs a second phase.
        painter.draw_glass(&rect_group(Rect::new(48.0, 48.0, 80.0, 80.0)));
    });
    let calls = calls.lock().unwrap();
    assert_eq!(calls.begin_frames, 1);
    assert_eq!(calls.frames.len(), 1, "prepare runs once per frame");
    let (_, phases) = calls.frames[0];
    assert_eq!(phases, 2);
    assert_eq!(calls.records, vec![1, 2]);
}

#[test]
fn image_renderer_with_a_backend_composites_its_glass() {
    let Some(mut renderer) = image_renderer() else {
        return;
    };
    let stock = render(&mut renderer, |painter| background(painter));
    let (mut fake, _) = Fake::new();
    fake.magenta = true;
    renderer.set_glass_backend(Box::new(fake));
    // A composite on whole tiles, so Vello's snapping leaves it as is.
    let composite = (16, 16, 48, 48);
    let glass = render(&mut renderer, |painter| {
        background(painter);
        painter.draw_glass(&rect_group(Rect::new(16.0, 16.0, 48.0, 48.0)));
    });
    for y in 0..SIZE {
        for x in 0..SIZE {
            let inside =
                (composite.0..composite.2).contains(&x) && (composite.1..composite.3).contains(&y);
            let expected = if inside {
                [255, 0, 255, 255]
            } else {
                pixel(&stock, x, y)
            };
            assert_eq!(pixel(&glass, x, y), expected, "pixel ({x}, {y})");
        }
    }
}
