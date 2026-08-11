//! One-shot CSS box construction, numeric layout, and owned paint projection.
//!
//! The crate knows Stylo and Taffy but never knows Moli's live DOM or V8
//! runtime. Renderer-owned adapters lend a canonical source view and resolve
//! styles; all source/style borrows and per-pass caches are gone before the
//! returned [`LayoutPassOutput`] or [`PaintSnapshot`] crosses into a consumer.

mod builder;
mod capture;
mod containment;
mod error;
mod form;
mod gradient;
mod inline;
mod intrinsic;
mod list;
mod normalize;
mod normalize_source;
mod output;
mod paint;
mod pass;
mod positioned;
mod projection;
mod replaced;
mod snapshot;
mod source;
mod stacking;
mod style;
mod stylo_to_parley;
mod system_fonts;
mod table;
mod taffy_tree;
mod text;
mod world;

pub use builder::build_layout_world;
pub use capture::{PaintCaptureRegion, PaintCaptureRequest, PaintCaptureSurface};
pub use error::LayoutError;
pub use normalize::{NormalizedBoxNode, NormalizedBoxTree, NormalizedFormattingContext};
pub use normalize_source::{
    NormalizedLayoutSourceNode, NormalizedLayoutSourceTree, normalize_layout_source,
};
pub use output::{
    GeometryProvider, LayoutAnswers, LayoutBoxGeometry, LayoutBoxModel, LayoutCaretPosition,
    LayoutClipChainId, LayoutClipNode, LayoutCoordinateSpace, LayoutCoordinateSpaceId,
    LayoutDocumentMetrics, LayoutElementMetrics, LayoutFlushReason, LayoutFragment,
    LayoutFragmentBoxModel, LayoutFragmentId, LayoutFragmentKind, LayoutHit, LayoutHitTestEntry,
    LayoutHitTestIndex, LayoutIntersectionGeometry, LayoutNodeOutput, LayoutOutputBoxId,
    LayoutOutputRetentionMetrics, LayoutPassMetrics, LayoutPassOutput, LayoutPoint, LayoutQuad,
    LayoutQuery, LayoutQueryAnswer, LayoutQueryBatch, LayoutRect, LayoutScrollContainerMetrics,
    LayoutScrollExtent, LayoutScrollExtentId, LayoutScrollIntoViewGeometry, LayoutSize,
    LayoutTransform2D, LayoutViewport, MAX_RETAINED_LAYOUT_BOXES, MAX_RETAINED_LAYOUT_FRAGMENTS,
    MAX_RETAINED_LAYOUT_OUTPUT_BYTES,
};
pub use pass::{
    EmbeddedFrameRenderer, LayoutPassRequest, ScreenshotLayoutRequest, build_layout_pass_output,
    build_layout_pass_output_with_embedded_frames, build_screenshot_snapshot,
};
pub use snapshot::{
    PaintBlendMode, PaintBorderColors, PaintBorderStyle, PaintBorderStyles, PaintBoxShadow,
    PaintBrush, PaintColor, PaintCompositeMode, PaintConicGradient, PaintCornerRadii,
    PaintCornerRadius, PaintDiagnostic, PaintDiagnosticSeverity, PaintEdgeSizes, PaintFilter,
    PaintFontId, PaintFontResource, PaintFragment, PaintGlyph, PaintGlyphRun,
    PaintGradientColorSpace, PaintGradientExtend, PaintGradientHueDirection,
    PaintGradientInterpolation, PaintGradientStop, PaintImage, PaintImageId, PaintImageResource,
    PaintImageSampling, PaintLineCap, PaintLineJoin, PaintLinearGradient, PaintPath,
    PaintPathElement, PaintPoint, PaintRadialGradient, PaintRect, PaintShape, PaintSize,
    PaintSnapshot, PaintStroke, PaintSvgImage, PaintSvgImageId, PaintSvgImageResource,
    PaintTextDecoration, PaintTextDecorationStyle, PaintTextShadow, PaintTransform2D,
    PaintViewport,
};
pub use source::{
    LayoutElementCategory, LayoutElementMetadata, LayoutElementSemantics, LayoutFormControlData,
    LayoutFormControlKind, LayoutImageResource, LayoutInputControlKind, LayoutListData,
    LayoutListRole, LayoutNamespace, LayoutPseudo, LayoutReplacedKind, LayoutSource,
    LayoutSourceKind, LayoutStyleResolver, LayoutTableData, LayoutTableRole, LayoutTextSelection,
    ReplacedMetrics,
};
pub use style::{
    LayoutDisplay, LayoutInlineAlignment, LayoutListMarkerPosition, LayoutListMarkerType,
    LayoutPosition, ResolvedLayoutStyle,
};
pub use text::{
    DocumentLayoutServices, SystemFontPolicy, WebFontFace, WebFontRegistration,
    WebFontRegistrationError, WebFontRegistrationOutcome, WebFontStyle, WebFontUnicodeRange,
};
pub use world::{
    LayoutAnonymousReason, LayoutBox, LayoutBoxId, LayoutBoxKind, LayoutCapabilityDiagnostic,
    LayoutWorld,
};
