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
    /// Returns the pixels `group` reads and writes, at a device scale of `scale`.
    fn regions(&self, group: &GlassGroup, scale: f64) -> GlassRegions;

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
}

impl FrameInputs {
    /// Creates frame inputs for time `time` at a device scale of `scale`.
    pub fn new(time: f64, scale: f64) -> Self {
        Self { time, scale }
    }
}

/// The inputs a renderer uses until its caller sets some: time 0 at scale 1.
const DEFAULT_INPUTS: FrameInputs = FrameInputs {
    time: 0.0,
    scale: 1.0,
};

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
}
