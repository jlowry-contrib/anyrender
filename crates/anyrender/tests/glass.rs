//! Integration tests for glass groups and their tint fallback.

use std::sync::Arc;

use anyrender::glass::{
    BackdropSource, GlassGroup, GlassMaterial, GlassShape, GlassVariant, ShapeKind, VibrantMatrix,
};
use anyrender::recording::{RenderCommand, Scene};
use anyrender::{Filter, Glyph, NormalizedCoord, Paint, PaintRef, PaintScene, RenderContext};
use kurbo::{Affine, Rect, Shape, Stroke};
use peniko::{BlendMode, Color, Fill, FontData, Mix, StyleRef};

/// A painter that keeps only its fills, and takes the default `draw_glass`.
#[derive(Default)]
struct FillLog {
    fills: Vec<(Affine, Color)>,
    /// The address of each group `draw_shared_glass` received.
    shared: Vec<*const GlassGroup>,
    /// The blend, alpha and transform of each pushed layer.
    layers: Vec<(BlendMode, f32, Affine)>,
}

impl RenderContext for FillLog {}

impl PaintScene for FillLog {
    fn reset(&mut self) {
        self.fills.clear();
    }

    fn push_layer(
        &mut self,
        _fill: Fill,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        _clip: &impl Shape,
        _filter: Option<Arc<Filter>>,
        _backdrop_filter: Option<Arc<Filter>>,
    ) {
        self.layers.push((blend.into(), alpha, transform));
    }

    fn push_clip_layer(&mut self, _fill: Fill, _transform: Affine, _clip: &impl Shape) {}

    fn pop_layer(&mut self) {}

    fn stroke<'a>(
        &mut self,
        _style: &Stroke,
        _transform: Affine,
        _brush: impl Into<PaintRef<'a>>,
        _brush_transform: Option<Affine>,
        _shape: &impl Shape,
    ) {
    }

    fn fill<'a>(
        &mut self,
        _style: Fill,
        transform: Affine,
        brush: impl Into<PaintRef<'a>>,
        _brush_transform: Option<Affine>,
        _shape: &impl Shape,
    ) {
        let Paint::Solid(color) = brush.into() else {
            panic!("the tint fallback fills with solid colors");
        };
        self.fills.push((transform, color));
    }

    fn draw_glyphs<'a, 's: 'a>(
        &'s mut self,
        _font: &'a FontData,
        _font_size: f32,
        _hint: bool,
        _normalized_coords: &'a [NormalizedCoord],
        _embolden: kurbo::Vec2,
        _style: impl Into<StyleRef<'a>>,
        _brush: impl Into<PaintRef<'a>>,
        _brush_alpha: f32,
        _transform: Affine,
        _glyph_transform: Option<Affine>,
        _glyphs: impl Iterator<Item = Glyph> + Clone,
    ) {
    }

    fn draw_box_shadow(
        &mut self,
        _transform: Affine,
        _rect: Rect,
        _brush: Color,
        _radius: f64,
        _std_dev: f64,
    ) {
    }

    fn draw_shared_glass(&mut self, group: Arc<GlassGroup>) {
        self.shared.push(Arc::as_ptr(&group));
        self.draw_glass(&group);
    }
}

const RED: Color = Color::from_rgba8(255, 0, 0, 128);
const BLUE: Color = Color::from_rgba8(0, 0, 255, 64);

fn tinted(variant: GlassVariant, tint: Color) -> GlassMaterial {
    let mut material = GlassMaterial::new(variant);
    material.tint = Some(tint);
    material
}

fn two_shapes() -> GlassGroup {
    GlassGroup::new(vec![
        GlassShape::new(
            1,
            Affine::translate((5.0, 6.0)),
            ShapeKind::Capsule(Rect::new(0.0, 0.0, 80.0, 30.0)),
            tinted(GlassVariant::Regular, RED),
        ),
        GlassShape::new(
            2,
            Affine::scale(2.0),
            ShapeKind::Ellipse(Rect::new(0.0, 0.0, 40.0, 40.0)),
            tinted(GlassVariant::Clear, BLUE),
        ),
    ])
}

#[test]
fn default_draw_glass_fills_each_shape_with_its_tint() {
    let mut painter = FillLog::default();
    painter.draw_glass(&two_shapes());

    assert_eq!(
        painter.fills,
        vec![
            (Affine::translate((5.0, 6.0)), RED),
            (Affine::scale(2.0), BLUE),
        ]
    );
    assert!(!painter.supports_glass());
}

#[test]
fn default_draw_glass_fills_untinted_shapes_with_translucent_white() {
    let group = GlassGroup::new(vec![GlassShape::new(
        7,
        Affine::IDENTITY,
        ShapeKind::Ellipse(Rect::new(0.0, 0.0, 10.0, 10.0)),
        GlassMaterial::new(GlassVariant::Regular),
    )]);
    let mut painter = FillLog::default();
    painter.draw_glass(&group);

    assert_eq!(
        painter.fills,
        vec![(Affine::IDENTITY, Color::WHITE.with_alpha(0.3))]
    );
}

#[test]
fn recording_keeps_glass_commands_in_order() {
    let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
    let group = two_shapes();
    let mut scene = Scene::new();
    scene.fill(Fill::NonZero, Affine::IDENTITY, RED, None, &rect);
    scene.draw_glass(&group);
    scene.fill(Fill::NonZero, Affine::IDENTITY, BLUE, None, &rect);

    assert!(scene.supports_glass());
    assert_eq!(scene.commands.len(), 3);
    assert!(matches!(scene.commands[0], RenderCommand::Fill(_)));
    assert_eq!(scene.commands[1], RenderCommand::Glass(Arc::new(group)));
    assert!(matches!(scene.commands[2], RenderCommand::Fill(_)));
}

#[test]
fn append_scene_replays_glass_under_the_scene_transform() {
    let offset = Affine::translate((10.0, 20.0));
    let mut fragment = Scene::new();
    fragment.draw_glass(&two_shapes());

    let mut painter = FillLog::default();
    painter.append_scene(fragment.clone(), offset);
    assert_eq!(
        painter.fills,
        vec![
            (offset * Affine::translate((5.0, 6.0)), RED),
            (offset * Affine::scale(2.0), BLUE),
        ]
    );

    let mut scene = Scene::new();
    scene.append_scene(fragment, offset);
    let [RenderCommand::Glass(group)] = scene.commands.as_slice() else {
        panic!("expected one glass command, got {:?}", scene.commands);
    };
    let transforms: Vec<_> = group.shapes.iter().map(|shape| shape.transform).collect();
    assert_eq!(
        transforms,
        vec![
            offset * Affine::translate((5.0, 6.0)),
            offset * Affine::scale(2.0),
        ]
    );
}

#[test]
fn append_scene_composes_the_backdrop_region_transform() {
    let offset = Affine::translate((10.0, 20.0));
    let region = Rect::new(0.0, 0.0, 50.0, 50.0);
    let mut group = two_shapes();
    group.source = BackdropSource::Region {
        rect: region,
        transform: Affine::scale(3.0),
    };
    let mut fragment = Scene::new();
    fragment.draw_glass(&group);

    let mut scene = Scene::new();
    scene.append_scene(fragment, offset);
    let [RenderCommand::Glass(group)] = scene.commands.as_slice() else {
        panic!("expected one glass command, got {:?}", scene.commands);
    };
    assert_eq!(
        group.source,
        BackdropSource::Region {
            rect: region,
            transform: offset * Affine::scale(3.0),
        }
    );
}

#[test]
fn default_draw_glass_skips_identity_shapes() {
    let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
    let group = GlassGroup::new(vec![
        GlassShape::new(
            1,
            Affine::translate((1.0, 0.0)),
            ShapeKind::Capsule(rect),
            tinted(GlassVariant::Identity, RED),
        ),
        GlassShape::new(
            2,
            Affine::translate((2.0, 0.0)),
            ShapeKind::Capsule(rect),
            tinted(GlassVariant::Regular, BLUE),
        ),
        GlassShape::new(
            3,
            Affine::translate((3.0, 0.0)),
            ShapeKind::Ellipse(rect),
            GlassMaterial::new(GlassVariant::Identity),
        ),
    ]);
    let mut painter = FillLog::default();
    painter.draw_glass(&group);

    assert_eq!(painter.fills, vec![(Affine::translate((2.0, 0.0)), BLUE)]);
}

#[test]
fn default_draw_glass_scales_fills_by_the_group_alpha() {
    let mut group = two_shapes();
    group.alpha = 0.5;
    let mut painter = FillLog::default();
    painter.draw_glass(&group);

    assert_eq!(
        painter.fills,
        vec![
            (
                Affine::translate((5.0, 6.0)),
                Color::from_rgba8(255, 0, 0, 64)
            ),
            (Affine::scale(2.0), Color::from_rgba8(0, 0, 255, 32)),
        ]
    );
}

#[test]
fn append_scene_passes_the_recorded_group_through() {
    let mut fragment = Scene::new();
    fragment.draw_glass(&two_shapes());
    let RenderCommand::Glass(recorded) = &fragment.commands[0] else {
        panic!("expected a glass command, got {:?}", fragment.commands);
    };
    let recorded = Arc::as_ptr(recorded);

    let mut painter = FillLog::default();
    painter.append_scene(fragment, Affine::translate((1.0, 2.0)));
    assert_eq!(painter.shared, vec![recorded]);
    assert_eq!(painter.fills.len(), 2);
}

#[test]
fn recording_keeps_a_shared_group_without_cloning_it() {
    let group = Arc::new(two_shapes());
    let mut scene = Scene::new();
    scene.draw_shared_glass(Arc::clone(&group));
    let [RenderCommand::Glass(kept)] = scene.commands.as_slice() else {
        panic!("expected one glass command, got {:?}", scene.commands);
    };
    assert!(Arc::ptr_eq(kept, &group));
}

#[test]
fn group_ids_survive_recording_and_replay() {
    let group = two_shapes();
    assert_eq!(group.id, None);
    let group = group.with_id(42);
    assert_eq!(group.id, Some(42));

    let mut fragment = Scene::new();
    fragment.draw_glass(&group);
    let mut scene = Scene::new();
    scene.append_scene(fragment, Affine::translate((3.0, 4.0)));
    let [RenderCommand::Glass(replayed)] = scene.commands.as_slice() else {
        panic!("expected one glass command, got {:?}", scene.commands);
    };
    assert_eq!(replayed.id, Some(42));
}

/// A matrix that swaps the red and blue channels.
fn red_blue_swap() -> VibrantMatrix {
    VibrantMatrix {
        rows: [
            [0.0, 0.0, 1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0, 0.0],
        ],
        clamp: (0.0, 1.0),
    }
}

#[test]
fn push_vibrant_layer_records_and_replays_under_the_scene_transform() {
    let clip = Rect::new(0.0, 0.0, 10.0, 10.0);
    let local = Affine::translate((1.0, 2.0));
    let offset = Affine::translate((10.0, 20.0));
    let mut fragment = Scene::new();
    fragment.push_vibrant_layer(&red_blue_swap(), 0.75, local, &clip);
    fragment.pop_layer();

    let mut scene = Scene::new();
    scene.append_scene(fragment.clone(), offset);
    let [
        RenderCommand::PushVibrantLayer(layer),
        RenderCommand::PopLayer,
    ] = scene.commands.as_slice()
    else {
        panic!(
            "expected a vibrant layer and a pop, got {:?}",
            scene.commands
        );
    };
    assert_eq!(layer.matrix, red_blue_swap());
    assert_eq!(layer.alpha, 0.75);
    assert_eq!(layer.transform, offset * local);
    assert_eq!(layer.clip, clip.into_path(0.1));

    // A painter without vibrant layers replays it as a normal layer, under the same transform.
    let mut painter = FillLog::default();
    painter.append_scene(fragment, offset);
    assert_eq!(
        painter.layers,
        vec![(BlendMode::from(Mix::Normal), 0.75, offset * local)]
    );
}

#[test]
fn default_push_vibrant_layer_is_a_normal_layer() {
    let mut painter = FillLog::default();
    let transform = Affine::scale(2.0);
    painter.push_vibrant_layer(
        &red_blue_swap(),
        0.5,
        transform,
        &Rect::new(0.0, 0.0, 4.0, 4.0),
    );
    assert_eq!(
        painter.layers,
        vec![(BlendMode::from(Mix::Normal), 0.5, transform)]
    );
}
