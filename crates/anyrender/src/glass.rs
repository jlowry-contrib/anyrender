//! Liquid glass groups, drawn with [`PaintScene::draw_glass`].
//!
//! A [`GlassGroup`] is a set of glass shapes that refract the same backdrop and may merge
//! into one another. Backends that render glass report it through
//! [`PaintScene::supports_glass`]; the others fall back to
//! [`fill_tint`], which fills each shape with its tint.

use std::sync::Arc;

use kurbo::{Affine, BezPath, Ellipse, Rect, RoundedRect, RoundedRectRadii};
use peniko::{Color, Fill};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::PaintScene;

/// Alpha of the white that [`fill_tint`] uses for a shape without a tint.
///
/// A light veil keeps an untinted glass shape visible over any backdrop without hiding it.
const FALLBACK_TINT_ALPHA: f32 = 0.3;

/// A set of glass shapes that share one backdrop and can merge into each other.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub struct GlassGroup {
    /// The shapes, drawn in order.
    pub shapes: Vec<GlassShape>,
    /// The distance, in user units, within which neighbouring shapes merge.
    pub merge_spacing: f64,
    /// Pairs of shape ids that merge whatever the distance between them.
    pub unions: Vec<(u32, u32)>,
    /// The opacity of the whole group.
    pub alpha: f32,
    /// Where the group's backdrop comes from.
    pub source: BackdropSource,
}

impl GlassGroup {
    /// Creates an opaque group of `shapes` over the content behind it, with no merging.
    pub fn new(shapes: Vec<GlassShape>) -> Self {
        Self {
            shapes,
            merge_spacing: 0.0,
            unions: Vec::new(),
            alpha: 1.0,
            source: BackdropSource::Behind,
        }
    }

    /// Composes `transform` before every transform the group holds.
    pub(crate) fn apply_transform(&mut self, transform: Affine) {
        for shape in &mut self.shapes {
            shape.transform = transform * shape.transform;
        }
        if let BackdropSource::Region {
            transform: region_transform,
            ..
        } = &mut self.source
        {
            *region_transform = transform * *region_transform;
        }
    }
}

/// One glass shape of a [`GlassGroup`].
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub struct GlassShape {
    /// An id, unique within its group, that [`GlassGroup::unions`] refers to.
    pub id: u32,
    /// The transform from the shape's coordinates to the scene's.
    pub transform: Affine,
    /// The outline of the shape.
    pub kind: ShapeKind,
    /// How the shape bends and tints its backdrop.
    pub material: GlassMaterial,
}

impl GlassShape {
    /// Creates a glass shape.
    pub fn new(id: u32, transform: Affine, kind: ShapeKind, material: GlassMaterial) -> Self {
        Self {
            id,
            transform,
            kind,
            material,
        }
    }
}

/// The outline of a [`GlassShape`].
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum ShapeKind {
    /// A rectangle with rounded corners.
    RoundedRect {
        /// The bounds.
        rect: Rect,
        /// The corner radii.
        radii: RoundedRectRadii,
        /// The curve of each corner.
        corner: CornerStyle,
    },
    /// A rectangle whose shorter sides are half circles.
    Capsule(Rect),
    /// The ellipse inscribed in a rectangle.
    Ellipse(Rect),
    /// Any closed path.
    Path(Arc<BezPath>),
}

/// The curve of a rounded rectangle's corners.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum CornerStyle {
    /// Quarter circles.
    Circular,
    /// Corners whose curvature changes continuously into the straight edges.
    Continuous,
    /// Superellipse corners, with one exponent per corner in the order top-left, top-right,
    /// bottom-right, bottom-left.
    Superellipse([f32; 4]),
}

/// How a [`GlassShape`] bends and tints its backdrop.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub struct GlassMaterial {
    /// The kind of glass.
    pub variant: GlassVariant,
    /// A colour laid over the backdrop, if any.
    pub tint: Option<Color>,
    /// Whether the shape reacts to touches.
    pub interactive: bool,
}

impl GlassMaterial {
    /// Creates an untinted, non-interactive material.
    pub fn new(variant: GlassVariant) -> Self {
        Self {
            variant,
            tint: None,
            interactive: false,
        }
    }
}

/// The kind of glass a [`GlassMaterial`] is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum GlassVariant {
    /// Frosted glass, for controls over busy content.
    Regular,
    /// Clearer glass, for controls over media.
    Clear,
    /// Glass that leaves its backdrop unchanged.
    Identity,
}

/// Where a [`GlassGroup`]'s backdrop comes from.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum BackdropSource {
    /// The scene content drawn before the group.
    Behind,
    /// The scene content drawn before the group, inside `rect` under `transform`.
    Region {
        /// The region, in its own coordinates.
        rect: Rect,
        /// The transform from the region's coordinates to the scene's.
        transform: Affine,
    },
}

/// Fills each shape of `group` with its tint, under the shape's transform.
///
/// A shape without a tint gets a translucent white. The group's alpha scales every fill.
/// This is the default [`PaintScene::draw_glass`], for backends that can't render glass.
pub fn fill_tint(painter: &mut (impl PaintScene + ?Sized), group: &GlassGroup) {
    for shape in &group.shapes {
        let color = shape
            .material
            .tint
            .unwrap_or(Color::WHITE.with_alpha(FALLBACK_TINT_ALPHA))
            .multiply_alpha(group.alpha);
        let transform = shape.transform;
        match &shape.kind {
            ShapeKind::RoundedRect { rect, radii, .. } => painter.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(*rect, *radii),
            ),
            ShapeKind::Capsule(rect) => painter.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(*rect, rect.width().min(rect.height()) / 2.0),
            ),
            ShapeKind::Ellipse(rect) => painter.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &Ellipse::from_rect(*rect),
            ),
            ShapeKind::Path(path) => {
                painter.fill(Fill::NonZero, transform, color, None, path.as_ref())
            }
        }
    }
}
