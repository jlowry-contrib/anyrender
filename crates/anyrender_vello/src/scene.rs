use std::sync::Arc;

use anyrender::glass::{GlassGroup, fill_tint};
use anyrender::{Filter, NormalizedCoord, Paint, PaintRef, PaintScene, RenderContext, ResourceId};
use kurbo::{Affine, Rect, Shape, Stroke};
use peniko::{BlendMode, BrushRef, Color, Fill, FontData, ImageBrush, ImageData, StyleRef};
use rustc_hash::FxHashMap;
use vello::Renderer as VelloRenderer;
use wgpu::Texture;
use wgpu_context::DeviceHandle;

use crate::glass::{self, GlassSlot};

pub struct VelloScenePainter<'r, 's> {
    pub(crate) renderer: Option<&'r mut VelloRenderer>,
    pub(crate) device_handle: Option<&'r DeviceHandle>,
    pub(crate) texture_handles: Option<&'r mut FxHashMap<ResourceId, ImageData>>,
    pub(crate) inner: &'s mut vello::Scene,
    pub(crate) glass: Option<&'r mut GlassSlot>,
}

impl RenderContext for VelloScenePainter<'_, '_> {
    fn try_register_custom_resource(
        &mut self,
        resource: Box<dyn std::any::Any>,
    ) -> Result<ResourceId, anyrender::RegisterResourceError> {
        if let Some(renderer) = &mut self.renderer
            && let Some(texture_handles) = &mut self.texture_handles
        {
            if let Ok(texture) = resource.downcast::<Texture>() {
                let id = ResourceId::new();
                texture_handles.insert(id, renderer.register_texture(*texture));
                Ok(id)
            } else {
                Err(anyrender::RegisterResourceErrorKind::UnsupportedResourceKind.into())
            }
        } else {
            Err(anyrender::RegisterResourceErrorKind::Unimplemented.into())
        }
    }

    fn unregister_resource(&mut self, resource_id: ResourceId) {
        if let Some(renderer) = &mut self.renderer
            && let Some(texture_handles) = &mut self.texture_handles
            && let Some(handle) = texture_handles.remove(&resource_id)
        {
            renderer.unregister_texture(handle);
        }
    }

    fn renderer_specific_context(&self) -> Option<Box<dyn std::any::Any>> {
        self.device_handle
            .map(|device_handle| Box::new(device_handle.clone()) as _)
    }
}

impl VelloScenePainter<'_, '_> {
    pub fn new<'s>(scene: &'s mut vello::Scene) -> VelloScenePainter<'static, 's> {
        VelloScenePainter {
            renderer: None,
            device_handle: None,
            texture_handles: None,
            inner: scene,
            glass: None,
        }
    }

    /// Pushes the marker of `group` and returns `true` if the frame should keep the group.
    ///
    /// Without a backend, it fills the tint instead and returns `false`. A group with no
    /// shapes, or whose regions aren't finite or cover no pixels, pushes nothing and returns
    /// `false`.
    fn push_glass_marker(&mut self, group: &GlassGroup) -> bool {
        let Some(slot) = self.glass.as_deref_mut() else {
            fill_tint(self, group);
            return false;
        };
        let Some(backend) = &slot.backend else {
            fill_tint(self, group);
            return false;
        };
        if group.shapes.is_empty() {
            return false;
        }
        let regions = backend.regions(group, slot.inputs().scale);
        let id = u32::try_from(slot.frame_groups.len()).expect("fewer than 2^32 glass groups");
        let Some((marker, clamped)) = glass::marker(id, regions) else {
            if !slot.invalid_logged {
                slot.invalid_logged = true;
                log::warn!(
                    "glass.regions.invalid region={:?} composite={:?}",
                    regions.region,
                    regions.composite
                );
            }
            return false;
        };
        if clamped && !slot.composite_outside_logged {
            slot.composite_outside_logged = true;
            log::warn!(
                "glass.regions.composite_outside id={id} region={:?} composite={:?} requested={:?}",
                marker.region,
                marker.composite,
                regions.composite
            );
        }
        self.inner.push_glass(marker);
        true
    }

    /// Keeps `group` as the frame's next glass group.
    fn keep_glass_group(&mut self, group: Arc<GlassGroup>) {
        if let Some(slot) = self.glass.as_deref_mut() {
            slot.frame_groups.push(group);
        }
    }
}

impl PaintScene for VelloScenePainter<'_, '_> {
    fn reset(&mut self) {
        self.inner.reset();
        if let Some(slot) = self.glass.as_deref_mut() {
            slot.reset_frame();
        }
    }

    fn draw_glass(&mut self, group: &GlassGroup) {
        if self.push_glass_marker(group) {
            self.keep_glass_group(Arc::new(group.clone()));
        }
    }

    fn draw_shared_glass(&mut self, group: Arc<GlassGroup>) {
        if self.push_glass_marker(&group) {
            self.keep_glass_group(group);
        }
    }

    fn supports_glass(&self) -> bool {
        self.glass
            .as_deref()
            .is_some_and(|slot| slot.backend.is_some())
    }

    fn push_layer(
        &mut self,
        fill: Fill,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        clip: &impl Shape,
        _filter: Option<Arc<Filter>>,
        _backdrop_filter: Option<Arc<Filter>>,
    ) {
        self.inner.push_layer(fill, blend, alpha, transform, clip);
    }

    fn push_clip_layer(&mut self, fill: Fill, transform: Affine, clip: &impl Shape) {
        self.inner.push_clip_layer(fill, transform, clip);
    }

    fn pop_layer(&mut self) {
        self.inner.pop_layer();
    }

    fn stroke<'a>(
        &mut self,
        style: &Stroke,
        transform: Affine,
        paint_ref: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        let paint_ref: PaintRef<'_> = paint_ref.into();
        let brush_ref: BrushRef<'_> = paint_ref.into();
        self.inner
            .stroke(style, transform, brush_ref, brush_transform, shape);
    }

    fn fill<'a>(
        &mut self,
        style: Fill,
        transform: Affine,
        paint: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        let paint: PaintRef<'_> = paint.into();
        let brush_ref: BrushRef<'_> = match paint {
            Paint::Solid(color) => BrushRef::Solid(color),
            Paint::Gradient(gradient) => BrushRef::Gradient(gradient),
            Paint::Image(image) => BrushRef::Image(image),
            Paint::Resource(brush) => {
                let resource_id = brush.image;
                if let Some(texture_handle) = self
                    .texture_handles
                    .as_ref()
                    .and_then(|texture_handles| texture_handles.get(&resource_id))
                {
                    peniko::Brush::Image(ImageBrush {
                        image: texture_handle,
                        sampler: brush.sampler,
                    })
                } else {
                    BrushRef::Solid(Color::TRANSPARENT)
                }
            }
            Paint::Custom(_) => BrushRef::Solid(Color::TRANSPARENT),
        };

        self.inner
            .fill(style, transform, brush_ref, brush_transform, shape);
    }

    fn draw_glyphs<'a, 's: 'a>(
        &'a mut self,
        font: &'a FontData,
        font_size: f32,
        hint: bool,
        normalized_coords: &'a [NormalizedCoord],
        embolden: kurbo::Vec2,
        style: impl Into<StyleRef<'a>>,
        paint: impl Into<PaintRef<'a>>,
        brush_alpha: f32,
        transform: Affine,
        glyph_transform: Option<Affine>,
        glyphs: impl Iterator<Item = anyrender::Glyph>,
    ) {
        self.inner
            .draw_glyphs(font)
            .font_size(font_size)
            .hint(hint)
            .normalized_coords(normalized_coords)
            .font_embolden(vello::FontEmbolden::new(kurbo::Diagonal2::new(
                embolden.x, embolden.y,
            )))
            .brush(paint.into())
            .brush_alpha(brush_alpha)
            .transform(transform)
            .glyph_transform(glyph_transform)
            .draw(
                style,
                glyphs.map(|g: anyrender::Glyph| vello::Glyph {
                    id: g.id,
                    x: g.x,
                    y: g.y,
                }),
            );
    }

    fn draw_box_shadow(
        &mut self,
        transform: Affine,
        rect: Rect,
        brush: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.inner
            .draw_blurred_rounded_rect(transform, rect, brush, radius, std_dev);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyrender::PaintScene;
    use anyrender::glass::{
        CornerStyle, GlassGroup, GlassMaterial, GlassShape, GlassVariant, ShapeKind,
    };
    use kurbo::{Affine, Rect, RoundedRectRadii};
    use vello::glass::{GlassFrame, GlassPasses, GlassPhase, IRect};
    use wgpu::{CommandEncoder, Device, Queue};

    use super::VelloScenePainter;
    use crate::glass::{FrameInputs, GlassBackend, GlassRegions, GlassSlot};

    /// The shape id for which [`Boxes`] returns a NaN region.
    const INVALID: u32 = 99;

    /// A backend whose composite is the group's box under each shape's transform, and whose
    /// region is that box grown by 10.4 px, or NaN for a group whose first shape is
    /// [`INVALID`].
    struct Boxes;

    impl GlassPasses for Boxes {
        fn prepare(&mut self, _: &Device, _: &Queue, _: &GlassFrame<'_>) {}
        fn record(&mut self, _: &Device, _: &mut CommandEncoder, _: &GlassPhase<'_>) {}
    }

    impl GlassBackend for Boxes {
        fn regions(&self, group: &GlassGroup, scale: f64) -> GlassRegions {
            let composite = group
                .shapes
                .iter()
                .map(|shape| {
                    let ShapeKind::RoundedRect { rect, .. } = &shape.kind else {
                        unreachable!("the tests draw rounded rectangles only");
                    };
                    (Affine::scale(scale) * shape.transform).transform_rect_bbox(*rect)
                })
                .reduce(|a, b| a.union(b))
                .unwrap();
            let region = if group.shapes[0].id == INVALID {
                Rect::new(f64::NAN, 0.0, 10.0, 10.0)
            } else {
                composite.inflate(10.4, 10.4)
            };
            GlassRegions { region, composite }
        }

        fn begin_frame(&mut self, _: &[Arc<GlassGroup>], _: &FrameInputs) {}
    }

    fn group(transform: Affine, rect: Rect) -> GlassGroup {
        shape_group(0, transform, rect)
    }

    fn shape_group(id: u32, transform: Affine, rect: Rect) -> GlassGroup {
        GlassGroup::new(vec![GlassShape::new(
            id,
            transform,
            ShapeKind::RoundedRect {
                rect,
                radii: RoundedRectRadii::from_single_radius(1.0),
                corner: CornerStyle::Circular,
            },
            GlassMaterial::new(GlassVariant::Regular),
        )])
    }

    fn slot() -> GlassSlot {
        GlassSlot {
            backend: Some(Box::new(Boxes)),
            ..GlassSlot::default()
        }
    }

    fn painter<'r, 's>(
        scene: &'s mut vello::Scene,
        slot: &'r mut GlassSlot,
    ) -> VelloScenePainter<'r, 's> {
        VelloScenePainter {
            renderer: None,
            device_handle: None,
            texture_handles: None,
            inner: scene,
            glass: Some(slot),
        }
    }

    fn irect(x0: i32, y0: i32, x1: i32, y1: i32) -> IRect {
        IRect { x0, y0, x1, y1 }
    }

    #[test]
    fn draw_glass_under_a_fractional_transform_marks_the_shape_tiles() {
        let (mut scene, mut slot) = (vello::Scene::new(), slot());
        let transform = Affine::translate((0.5, 0.25)).then_scale(3.0);
        let mut painter = painter(&mut scene, &mut slot);
        painter.draw_glass(&group(transform, Rect::new(5.1, 5.0, 10.2, 10.0)));
        // The shape's device box is (16.8, 15.75, 32.1, 30.75): composite (16, 15, 33, 31)
        // and region (6, 5, 43, 42), which Vello snaps out to 16 px tiles.
        let markers = &scene.encoding().glass.markers;
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].id, 0);
        assert_eq!(markers[0].composite, irect(16, 0, 48, 32));
        assert_eq!(markers[0].region, irect(0, 0, 48, 48));
        assert_eq!(slot.frame_groups.len(), 1);
        assert_eq!(slot.frame_groups[0].shapes[0].transform, transform);
    }

    #[test]
    fn draw_glass_with_invalid_regions_marks_nothing_and_keeps_ids_dense() {
        let (mut scene, mut slot) = (vello::Scene::new(), slot());
        let rect = Rect::new(16.0, 16.0, 32.0, 32.0);
        let mut painter = painter(&mut scene, &mut slot);
        painter.draw_glass(&shape_group(INVALID, Affine::IDENTITY, rect));
        painter.draw_glass(&group(Affine::IDENTITY, rect));
        let markers = &scene.encoding().glass.markers;
        assert_eq!(markers.len(), 1);
        assert_eq!(
            markers[0].id, 0,
            "the valid group after an invalid one is marker 0"
        );
        assert_eq!(slot.frame_groups.len(), 1);
        assert_eq!(slot.frame_groups[0].shapes[0].id, 0);
        assert!(slot.invalid_logged);
    }
}
