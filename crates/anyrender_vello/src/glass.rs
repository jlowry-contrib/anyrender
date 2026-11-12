//! The slot through which a glass backend renders [`GlassGroup`]s.

use std::sync::Arc;

use anyrender::glass::GlassGroup;
use kurbo::Rect;
use vello::glass::{GlassMarker, GlassPasses, IRect};

/// A renderer of glass groups, installed into a Vello renderer.
///
/// The painter asks it where each group reads and writes, marks those pixels in the scene,
/// and hands it the frame's groups before Vello calls its [`GlassPasses`].
///
/// The backend is a trait object (M-DI-HIERARCHY) because anyrender_vello can't name the
/// types of the crate that renders the glass.
pub trait GlassBackend: GlassPasses {
    /// Returns the pixels `group` reads and writes under the frame's `inputs`.
    fn regions(&self, group: &GlassGroup, inputs: &FrameInputs) -> GlassRegions;

    /// Sees the frame's groups, indexed by marker id, before Vello renders the frame.
    fn begin_frame(&mut self, groups: &[Arc<GlassGroup>], inputs: &FrameInputs);

    /// Returns `true` while the glass needs another frame, for example while it animates.
    fn wants_redraw(&self) -> bool {
        false
    }
}

/// The pixels a glass group reads and writes, in fractional device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassRegions {
    /// The pixels whose backdrop the glass reads.
    pub region: Rect,
    /// The pixels the glass writes; must lie inside `region`.
    pub composite: Rect,
}

/// Per-frame values a glass backend renders with.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct FrameInputs {
    /// The frame's time, in seconds.
    pub time: f64,
    /// Device pixels per scene unit.
    pub scale: f64,
    /// The appearance the glass takes.
    pub appearance: Appearance,
    /// The accessibility settings the glass follows.
    pub accessibility: Accessibility,
}

impl FrameInputs {
    /// Creates frame inputs for time `time` at a device scale of `scale`, in the light
    /// appearance with no accessibility setting on.
    pub const fn new(time: f64, scale: f64) -> Self {
        Self {
            time,
            scale,
            appearance: Appearance::Light,
            accessibility: Accessibility::new(),
        }
    }

    /// Returns the inputs with `appearance`.
    pub fn with_appearance(mut self, appearance: Appearance) -> Self {
        self.appearance = appearance;
        self
    }

    /// Returns the inputs with `accessibility`.
    pub fn with_accessibility(mut self, accessibility: Accessibility) -> Self {
        self.accessibility = accessibility;
        self
    }
}

/// Whether glass renders for a light or a dark interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    /// The light appearance.
    #[default]
    Light,
    /// The dark appearance.
    Dark,
}

/// The accessibility settings that change how glass renders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Accessibility {
    /// Whether glass is drawn as a more opaque material.
    pub reduce_transparency: bool,
    /// Whether glass draws with stronger contrast.
    pub increase_contrast: bool,
}

impl Accessibility {
    /// Creates settings with every option off.
    pub const fn new() -> Self {
        Self {
            reduce_transparency: false,
            increase_contrast: false,
        }
    }
}

/// The inputs a renderer uses until its caller sets some: time 0 at scale 1, light, with no
/// accessibility setting on.
const DEFAULT_INPUTS: FrameInputs = FrameInputs::new(0.0, 1.0);

/// A renderer's glass backend and the state of the frame being painted.
#[derive(Default)]
pub(crate) struct GlassSlot {
    pub(crate) backend: Option<Box<dyn GlassBackend + Send>>,
    pub(crate) inputs: Option<FrameInputs>,
    /// The groups drawn this frame, indexed by marker id.
    pub(crate) frame_groups: Vec<Arc<GlassGroup>>,
    /// Whether this frame already logged a composite outside its region.
    pub(crate) composite_outside_logged: bool,
    /// Whether this frame already logged regions it couldn't mark.
    pub(crate) invalid_logged: bool,
    /// Whether the last glass render failed, so a run of failures logs once.
    pub(crate) render_failed: bool,
}

impl GlassSlot {
    /// Returns the frame inputs, or the defaults when none were set.
    pub(crate) fn inputs(&self) -> FrameInputs {
        self.inputs.unwrap_or(DEFAULT_INPUTS)
    }

    /// Forgets the frame's groups, so the next marker gets id 0.
    pub(crate) fn reset_frame(&mut self) {
        self.frame_groups.clear();
        self.composite_outside_logged = false;
        self.invalid_logged = false;
    }

    /// Calls the backend's `begin_frame` if it is installed and `has_markers`, and returns
    /// it for the render.
    pub(crate) fn begin_frame(&mut self, has_markers: bool) -> Option<&mut dyn GlassPasses> {
        let inputs = self.inputs();
        let backend = self.backend.as_deref_mut().filter(|_| has_markers)?;
        backend.begin_frame(&self.frame_groups, &inputs);
        Some(backend)
    }

    /// Renders a frame into `renderer` with `glass` when [`begin_frame`](Self::begin_frame)
    /// returns the backend, and with `stock` otherwise or when `glass` fails.
    ///
    /// A glass failure is logged as `glass.render.failed` when it follows a frame that
    /// didn't fail, and the frame renders without glass rather than panicking.
    ///
    /// # Errors
    ///
    /// Returns the error of `stock`.
    pub(crate) fn render<R: ?Sized>(
        &mut self,
        has_markers: bool,
        renderer: &mut R,
        glass: impl FnOnce(&mut R, &mut dyn GlassPasses) -> Result<(), vello::Error>,
        stock: impl FnOnce(&mut R) -> Result<(), vello::Error>,
    ) -> Result<(), vello::Error> {
        let Some(passes) = self.begin_frame(has_markers) else {
            return stock(renderer);
        };
        match glass(renderer, passes) {
            Ok(()) => {
                self.render_failed = false;
                Ok(())
            }
            Err(error) => {
                if !self.render_failed {
                    log::warn!("glass.render.failed error={error}");
                }
                self.render_failed = true;
                stock(renderer)
            }
        }
    }
}

/// Rounds `rect` outward to whole pixels.
fn round_out(rect: Rect) -> IRect {
    IRect {
        x0: rect.x0.floor() as i32,
        y0: rect.y0.floor() as i32,
        x1: rect.x1.ceil() as i32,
        y1: rect.y1.ceil() as i32,
    }
}

/// Builds marker `id` from `regions`, rounded outward to whole pixels, with the composite
/// clamped into the region. Returns the marker and whether the composite needed clamping.
///
/// Returns `None` when an edge of either rectangle isn't finite or the rounded region covers
/// no pixels: a faulty backend's regions can't be marked.
pub(crate) fn marker(id: u32, regions: GlassRegions) -> Option<(GlassMarker, bool)> {
    let finite = |r: Rect| [r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite());
    if !finite(regions.region) || !finite(regions.composite) {
        return None;
    }
    let region = round_out(regions.region);
    if region.is_empty() {
        return None;
    }
    let composite = round_out(regions.composite);
    let clamped = IRect {
        x0: composite.x0.clamp(region.x0, region.x1),
        y0: composite.y0.clamp(region.y0, region.y1),
        x1: composite.x1.clamp(region.x0, region.x1),
        y1: composite.y1.clamp(region.y0, region.y1),
    };
    let marker = GlassMarker {
        id,
        region,
        composite: clamped,
    };
    Some((marker, clamped != composite))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Affine;

    fn irect(x0: i32, y0: i32, x1: i32, y1: i32) -> IRect {
        IRect { x0, y0, x1, y1 }
    }

    /// The test backend's regions: the shape box, and that box grown by 10.4 px.
    fn grown(shape: Rect) -> GlassRegions {
        GlassRegions {
            region: shape.inflate(10.4, 10.4),
            composite: shape,
        }
    }

    #[test]
    fn draw_glass_rounds_fractional_rects_outward() {
        let (marker, clamped) = marker(3, grown(Rect::new(10.3, 20.6, 50.2, 40.1))).unwrap();
        assert_eq!(marker.id, 3);
        assert_eq!(marker.region, irect(-1, 10, 61, 51));
        assert_eq!(marker.composite, irect(10, 20, 51, 41));
        assert!(!clamped);
    }

    #[test]
    fn draw_glass_clamps_a_composite_outside_its_region() {
        let regions = GlassRegions {
            region: Rect::new(10.0, 10.0, 50.0, 50.0),
            composite: Rect::new(5.5, 20.0, 70.0, 40.0),
        };
        let (marker, clamped) = marker(0, regions).unwrap();
        assert!(clamped);
        assert_eq!(marker.region, irect(10, 10, 50, 50));
        assert_eq!(marker.composite, irect(10, 20, 50, 40));
    }

    #[test]
    fn a_composite_beside_its_region_clamps_to_empty() {
        let regions = GlassRegions {
            region: Rect::new(0.0, 0.0, 10.0, 10.0),
            composite: Rect::new(20.0, 0.0, 30.0, 10.0),
        };
        let (marker, clamped) = marker(0, regions).unwrap();
        assert!(clamped);
        assert!(marker.composite.is_empty());
    }

    #[test]
    fn draw_glass_under_a_fractional_transform_covers_the_shape() {
        let transform = Affine::translate((0.5, 0.25)).then_scale(3.0);
        let shape = transform.transform_rect_bbox(Rect::new(1.1, 2.3, 7.7, 9.9));
        let (marker, _) = marker(0, grown(shape)).unwrap();
        let covers = |r: IRect, s: Rect| {
            f64::from(r.x0) <= s.x0
                && f64::from(r.y0) <= s.y0
                && f64::from(r.x1) >= s.x1
                && f64::from(r.y1) >= s.y1
        };
        assert!(covers(marker.composite, shape), "{marker:?} vs {shape:?}");
        assert!(covers(marker.region, shape.inflate(10.4, 10.4)));
        let (r, c) = (marker.region, marker.composite);
        assert!(r.x0 <= c.x0 && r.y0 <= c.y0 && c.x1 <= r.x1 && c.y1 <= r.y1);
    }

    fn region(x0: f64, y0: f64, x1: f64, y1: f64) -> GlassRegions {
        GlassRegions {
            region: Rect { x0, y0, x1, y1 },
            composite: Rect::new(10.0, 10.0, 20.0, 20.0),
        }
    }

    #[test]
    fn an_inverted_region_is_not_marked() {
        assert_eq!(marker(0, region(50.0, 0.0, 10.0, 40.0)), None);
        assert_eq!(marker(0, region(0.0, 40.0, 40.0, 0.0)), None);
    }

    #[test]
    fn a_region_with_a_nan_edge_is_not_marked() {
        assert_eq!(marker(0, region(f64::NAN, 0.0, 40.0, 40.0)), None);
        assert_eq!(marker(0, region(-20.0, 0.0, f64::NAN, 40.0)), None);
        let nan_composite = GlassRegions {
            composite: Rect::new(10.0, f64::NAN, 20.0, 20.0),
            ..region(0.0, 0.0, 40.0, 40.0)
        };
        assert_eq!(marker(0, nan_composite), None);
    }

    #[test]
    fn a_region_with_an_infinite_edge_is_not_marked() {
        assert_eq!(marker(0, region(f64::INFINITY, 0.0, 40.0, 40.0)), None);
        assert_eq!(marker(0, region(0.0, 0.0, f64::INFINITY, 40.0)), None);
        assert_eq!(marker(0, region(f64::NEG_INFINITY, 0.0, 40.0, 40.0)), None);
        assert_eq!(marker(0, region(0.0, f64::NEG_INFINITY, 40.0, 40.0)), None);
    }

    #[test]
    fn a_region_past_the_i32_range_saturates_to_empty() {
        // Both x edges saturate to i32::MAX, leaving no pixels.
        assert_eq!(marker(0, region(1e12, 0.0, 2e12, 40.0)), None);
    }

    /// A backend that renders nothing; the test's glass closure stands in for Vello.
    struct Inert;

    impl GlassPasses for Inert {
        fn prepare(&mut self, _: &wgpu::Device, _: &wgpu::Queue, _: &vello::glass::GlassFrame<'_>) {
        }

        fn record(
            &mut self,
            _: &wgpu::Device,
            _: &mut wgpu::CommandEncoder,
            _: &vello::glass::GlassPhase<'_>,
        ) {
        }
    }

    impl GlassBackend for Inert {
        fn regions(&self, _: &GlassGroup, _: &FrameInputs) -> GlassRegions {
            grown(Rect::ZERO)
        }

        fn begin_frame(&mut self, _: &[Arc<GlassGroup>], _: &FrameInputs) {}
    }

    /// Captures warnings, for the only test in this binary that logs.
    fn warnings() -> &'static std::sync::Mutex<Vec<String>> {
        use std::sync::{Mutex, OnceLock};
        static LOGGED: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
        static LOGGER: Capture = Capture;
        struct Capture;
        impl log::Log for Capture {
            fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
                metadata.level() <= log::Level::Warn
            }
            fn log(&self, record: &log::Record<'_>) {
                warnings().lock().unwrap().push(record.args().to_string());
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
    fn a_failed_glass_render_falls_back_to_stock_and_logs_once() {
        let logged = warnings();
        let mut slot = GlassSlot {
            backend: Some(Box::new(Inert)),
            ..GlassSlot::default()
        };
        let failing = |calls: &mut Vec<&str>, _: &mut dyn GlassPasses| {
            calls.push("glass");
            Err(vello::Error::GlassAtlasTooLarge(512, 16384, 8192))
        };
        let stock = |calls: &mut Vec<&str>| {
            calls.push("stock");
            Ok(())
        };
        let mut calls = Vec::new();
        for _ in 0..2 {
            assert!(slot.render(true, &mut calls, failing, stock).is_ok());
        }
        assert_eq!(calls, ["glass", "stock", "glass", "stock"]);
        let failed: Vec<_> = logged
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.starts_with("glass.render.failed"))
            .cloned()
            .collect();
        assert_eq!(
            failed,
            [
                "glass.render.failed error=Glass atlas of 512x16384 exceeds the device's 8192 px \
              texture limit"
            ]
        );

        // A frame that renders its glass ends the run, and a frame without markers is stock.
        calls.clear();
        let rendered = |c: &mut Vec<&str>, _: &mut dyn GlassPasses| {
            c.push("glass");
            Ok(())
        };
        slot.render(true, &mut calls, rendered, stock).unwrap();
        slot.render(false, &mut calls, failing, stock).unwrap();
        assert_eq!(calls, ["glass", "stock"]);
        assert!(!slot.render_failed);
    }
}
