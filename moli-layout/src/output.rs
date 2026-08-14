use std::{
    collections::HashSet,
    fmt::Debug,
    hash::Hash,
    ops::{Deref, Range},
    time::Duration,
};

use crate::{LayoutError, LayoutPosition, PaintDiagnostic, PaintSnapshot};

/// Viewport inputs shared by layout, geometry queries, and paint projection.
///
/// Dimensions are CSS pixels. Device-pixel conversion belongs to the paint
/// backend and never changes the geometry stored in a frozen layout tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutViewport {
    pub css_width: u32,
    pub css_height: u32,
    pub device_pixel_ratio: f32,
}

impl LayoutViewport {
    pub const fn new(css_width: u32, css_height: u32, device_pixel_ratio: f32) -> Self {
        Self {
            css_width,
            css_height,
            device_pixel_ratio,
        }
    }
}

/// A two-dimensional point in CSS pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LayoutPoint {
    pub x: f32,
    pub y: f32,
}

impl LayoutPoint {
    pub const ZERO: Self = Self::new(0.0, 0.0);

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A two-dimensional extent in CSS pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LayoutSize {
    pub width: f32,
    pub height: f32,
}

impl LayoutSize {
    pub const ZERO: Self = Self::new(0.0, 0.0);

    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// An axis-aligned rectangle in one explicit layout coordinate space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LayoutRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl LayoutRect {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0, 0.0);

    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    pub fn contains(self, point: LayoutPoint) -> bool {
        self.width >= 0.0
            && self.height >= 0.0
            && point.x >= self.x
            && point.x < self.right()
            && point.y >= self.y
            && point.y < self.bottom()
    }

    pub fn union(self, other: Self) -> Self {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::new(left, top, (right - left).max(0.0), (bottom - top).max(0.0))
    }
}

/// Four corners of a transformed CSS box in top-left, top-right,
/// bottom-right, bottom-left order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutQuad {
    pub points: [LayoutPoint; 4],
}

impl LayoutQuad {
    pub fn bounding_rect(self) -> LayoutRect {
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for point in self.points {
            min_x = min_x.min(point.x);
            min_y = min_y.min(point.y);
            max_x = max_x.max(point.x);
            max_y = max_y.max(point.y);
        }
        if ![min_x, min_y, max_x, max_y].into_iter().all(f32::is_finite) {
            return LayoutRect::ZERO;
        }
        LayoutRect::new(
            min_x,
            min_y,
            (max_x - min_x).max(0.0),
            (max_y - min_y).max(0.0),
        )
    }
}

/// A CSS-pixel 2D affine transform.
///
/// Coefficients use the CSS matrix order `[a, b, c, d, e, f]`, where
/// `x' = a*x + c*y + e` and `y' = b*x + d*y + f`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutTransform2D {
    pub coefficients: [f64; 6],
}

impl LayoutTransform2D {
    pub const IDENTITY: Self = Self::new([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub const fn new(coefficients: [f64; 6]) -> Self {
        Self { coefficients }
    }

    pub fn translation(x: f32, y: f32) -> Self {
        Self::new([1.0, 0.0, 0.0, 1.0, f64::from(x), f64::from(y)])
    }

    pub fn scale(x: f64, y: f64) -> Self {
        Self::new([x, 0.0, 0.0, y, 0.0, 0.0])
    }

    pub fn rotation(radians: f64) -> Self {
        let (sin, cos) = radians.sin_cos();
        Self::new([cos, sin, -sin, cos, 0.0, 0.0])
    }

    /// Concatenates a child transform after this parent transform.
    ///
    /// The returned matrix maps a child-local point by `child` first, then by
    /// `self`. This is the operation used while walking coordinate spaces.
    pub fn concatenate(self, child: Self) -> Self {
        let [a, b, c, d, e, f] = self.coefficients;
        let [g, h, i, j, k, l] = child.coefficients;
        Self::new([
            a * g + c * h,
            b * g + d * h,
            a * i + c * j,
            b * i + d * j,
            a * k + c * l + e,
            b * k + d * l + f,
        ])
    }

    pub fn inverse(self) -> Option<Self> {
        let [a, b, c, d, e, f] = self.coefficients;
        let determinant = a * d - b * c;
        if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
            return None;
        }
        let inverse = 1.0 / determinant;
        Some(Self::new([
            d * inverse,
            -b * inverse,
            -c * inverse,
            a * inverse,
            (c * f - d * e) * inverse,
            (b * e - a * f) * inverse,
        ]))
    }

    pub fn map_point(self, point: LayoutPoint) -> LayoutPoint {
        let [a, b, c, d, e, f] = self.coefficients;
        let x = f64::from(point.x);
        let y = f64::from(point.y);
        LayoutPoint::new((a * x + c * y + e) as f32, (b * x + d * y + f) as f32)
    }

    pub fn map_rect(self, rect: LayoutRect) -> LayoutQuad {
        LayoutQuad {
            points: [
                self.map_point(LayoutPoint::new(rect.x, rect.y)),
                self.map_point(LayoutPoint::new(rect.right(), rect.y)),
                self.map_point(LayoutPoint::new(rect.right(), rect.bottom())),
                self.map_point(LayoutPoint::new(rect.x, rect.bottom())),
            ],
        }
    }

    pub fn is_finite(self) -> bool {
        self.coefficients.into_iter().all(f64::is_finite)
    }
}

macro_rules! dense_output_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u32);

        impl $name {
            pub(crate) fn from_index(index: usize) -> Self {
                Self(
                    u32::try_from(index).expect("one frozen layout tree exceeded the u32 id limit"),
                )
            }

            pub const fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

dense_output_id!(LayoutOutputBoxId);
dense_output_id!(LayoutFragmentId);
dense_output_id!(LayoutCoordinateSpaceId);
dense_output_id!(LayoutClipChainId);

/// One explicit local coordinate system in a frozen layout tree.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayoutCoordinateSpace {
    pub(crate) id: LayoutCoordinateSpaceId,
    pub(crate) owner: Option<LayoutOutputBoxId>,
    pub(crate) parent: Option<LayoutCoordinateSpaceId>,
    pub(crate) local_to_parent: LayoutTransform2D,
    /// Maps local coordinates to the visual document coordinate system. This
    /// includes element scrolling but excludes the viewport scroll offset.
    pub(crate) local_to_document: LayoutTransform2D,
    /// Maps local coordinates directly to viewport CSS pixels.
    pub(crate) local_to_viewport: LayoutTransform2D,
}

/// Query-facing coordinate data retained for one frozen box-tree node.
#[derive(Clone, Debug, PartialEq)]
pub struct FrozenCoordinateSpace {
    pub owner: Option<LayoutOutputBoxId>,
    pub local_to_viewport: LayoutTransform2D,
}

impl From<LayoutCoordinateSpace> for FrozenCoordinateSpace {
    fn from(space: LayoutCoordinateSpace) -> Self {
        Self {
            owner: space.owner,
            local_to_viewport: space.local_to_viewport,
        }
    }
}

/// One rectangular clip linked to its ancestor clip.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutClipNode {
    pub parent: Option<LayoutClipChainId>,
    pub owner: Option<LayoutOutputBoxId>,
    pub coordinate_space: LayoutCoordinateSpaceId,
    pub rect: LayoutRect,
}

/// Complete physical box model for one tree-local CSS box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutBoxModel {
    pub content: LayoutQuad,
    pub padding: LayoutQuad,
    pub border: LayoutQuad,
    pub margin: LayoutQuad,
}

/// Per-box scroll geometry in the box's own coordinate space.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutScrollExtent {
    pub scrollport: LayoutRect,
    pub scrollable_overflow: LayoutRect,
    pub scroll_size: LayoutSize,
    pub applied_offset: LayoutPoint,
    pub minimum_offset: LayoutPoint,
    pub maximum_offset: LayoutPoint,
    pub is_scroll_container: bool,
    pub clips_overflow: bool,
}

/// Geometry retained for one tree-local box.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutBoxGeometry {
    pub id: LayoutOutputBoxId,
    pub parent: Option<LayoutOutputBoxId>,
    pub layout_parent: Option<LayoutOutputBoxId>,
    pub position: LayoutPosition,
    pub coordinate_space: LayoutCoordinateSpaceId,
    pub clip_chain: Option<LayoutClipChainId>,
    pub content_box: LayoutRect,
    pub padding_box: LayoutRect,
    pub border_box: LayoutRect,
    pub margin_box: LayoutRect,
    pub fragments: Vec<LayoutFragmentId>,
    /// Untransformed border-box origin in document layout coordinates.
    pub layout_origin_in_document: LayoutPoint,
    pub is_body_element: bool,
    pub is_table_offset_parent: bool,
    pub establishes_positioned_containing_block: bool,
    pub establishes_fixed_containing_block: bool,
    pub visible: bool,
    pub pointer_events: bool,
}

/// One node in the immutable layout tree retained after a full pass.
///
/// `geometry_source` associates ordinary CSSOM geometry with its source. A
/// split inline continuation can therefore share its originating source while
/// remaining a distinct box-tree node. `hit_source` is separate because a
/// generated pseudo box participates in hit testing as its originating DOM
/// element without manufacturing CSSOM rects for that element.
#[derive(Clone, Debug, PartialEq)]
pub struct FrozenLayoutBox<N> {
    pub geometry: LayoutBoxGeometry,
    pub scroll_extent: LayoutScrollExtent,
    pub coordinate_space: FrozenCoordinateSpace,
    pub geometry_source: Option<N>,
    pub principal_source: Option<N>,
    pub hit_source: Option<N>,
}

impl<N> Deref for FrozenLayoutBox<N> {
    type Target = LayoutBoxGeometry;

    fn deref(&self) -> &Self::Target {
        &self.geometry
    }
}

/// Physical boxes retained for one box fragment in that fragment's local
/// coordinate space. Inline elements can own several of these across lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutFragmentBoxModel {
    pub content: LayoutRect,
    pub padding: LayoutRect,
    pub border: LayoutRect,
    pub margin: LayoutRect,
}

/// A geometry fragment kind. IDs contained here are valid only in the same
/// [`FrozenLayoutTree`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutFragmentKind {
    Box {
        box_id: LayoutOutputBoxId,
    },
    Line {
        owner: LayoutOutputBoxId,
        line_index: usize,
    },
    InlineBox {
        box_id: LayoutOutputBoxId,
        line_index: usize,
        has_start_edge: bool,
        has_end_edge: bool,
    },
    Text {
        box_id: LayoutOutputBoxId,
        line_index: usize,
        source_utf16_range: Range<usize>,
        rtl: bool,
    },
}

/// One box/line/inline/text fragment in an explicit coordinate space.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutFragment {
    pub id: LayoutFragmentId,
    pub kind: LayoutFragmentKind,
    pub rect: LayoutRect,
    pub box_model: Option<LayoutFragmentBoxModel>,
    pub coordinate_space: LayoutCoordinateSpaceId,
    pub clip_chain: Option<LayoutClipChainId>,
    pub paint_order: Option<u32>,
}

/// A short-lived source view derived from frozen box provenance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutNodeOutput {
    pub principal_box: Option<LayoutOutputBoxId>,
    pub fragments: Vec<LayoutFragmentId>,
    /// Direct generated boxes used only to resolve operations whose target is
    /// a `display: contents` source. They do not manufacture CSSOM rects for
    /// the box-suppressed element itself.
    pub scroll_proxy_boxes: Vec<LayoutOutputBoxId>,
}

/// One front-to-back hit-test candidate.
#[derive(Clone, Debug, PartialEq)]
struct LayoutHitTestEntry<N> {
    source: N,
    fragment: LayoutFragmentId,
    coordinate_space: LayoutCoordinateSpaceId,
    clip_chain: Option<LayoutClipChainId>,
    local_rect: LayoutRect,
    paint_order: u32,
    is_text: bool,
    pointer_events: bool,
}

/// Result of resolving a point against hit candidates derived from the tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutHit<N> {
    pub source: N,
    /// The real provider identifies the exact fragment. Explicit mock
    /// providers can return a source-only hit without manufacturing a
    /// tree-local fragment identity.
    pub fragment: Option<LayoutFragmentId>,
    pub local_point: LayoutPoint,
    pub is_text: bool,
    /// Box geometry copied from the same frozen tree when the hit source owns a
    /// CSS box. Consumers use it for source-dependent follow-up work such as
    /// descending through a transformed child-frame content box without
    /// forcing a second parent-document pass.
    pub box_model: Option<LayoutBoxModel>,
}

/// Caret geometry resolved from the same text fragments and coordinate spaces
/// as Range geometry and hit testing.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutCaretPosition<N> {
    pub source: N,
    /// Present when `source` owns a rendered text fragment. The offset uses
    /// the source node's UTF-16 code-unit coordinate space.
    pub utf16_offset: Option<usize>,
    pub rect: LayoutQuad,
    /// Source boxes from the selected fragment towards the construction root.
    /// This lets tree-scope retargeting use the same pass without retaining
    /// output-local box identifiers or forcing a follow-up layout.
    pub ancestor_boxes: Vec<(N, LayoutBoxModel)>,
}

/// Why a full, synchronous layout pass was forced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutFlushReason {
    Screenshot,
    Screencast,
    SynchronousGeometry,
    CdpGeometry,
    ObserverDelivery,
    HitTest,
    Paint,
    Test,
}

/// Diagnostics and cost counters for exactly one full pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutPassMetrics {
    pub reason: LayoutFlushReason,
    pub elapsed: Duration,
    pub box_count: usize,
    pub fragment_count: usize,
    pub paint_operation_count: usize,
    pub fallback_count: usize,
}

/// Document-level dimensions returned from the unified output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutDocumentMetrics {
    pub viewport: LayoutViewport,
    pub viewport_scroll: LayoutPoint,
    pub content_size: LayoutSize,
}

/// CSSOM View and observer metrics for one source element.
///
/// Transformed quads use viewport CSS pixels. Offset and size fields retain
/// the untransformed layout values required by offset/client/scroll APIs.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutElementMetrics<N> {
    pub offset_parent: Option<N>,
    pub offset_position: LayoutPoint,
    pub offset_size: LayoutSize,
    pub content_size: LayoutSize,
    pub client_size: LayoutSize,
    pub client_border: LayoutPoint,
    pub scroll_size: LayoutSize,
    pub scroll_offset: LayoutPoint,
    pub minimum_scroll_offset: LayoutPoint,
    pub maximum_scroll_offset: LayoutPoint,
    pub scrollport: LayoutQuad,
    pub scrollable_overflow: LayoutQuad,
    pub is_scroll_container: bool,
    pub clips_overflow: bool,
    pub visible: bool,
    pub pointer_events: bool,
}

/// One scroll container on a target's layout ancestor chain.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutScrollContainerMetrics<N> {
    pub source: N,
    pub metrics: LayoutElementMetrics<N>,
}

/// Geometry needed to run one `scrollIntoView` operation without retaining
/// layout state or forcing source-dependent follow-up passes.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutScrollIntoViewGeometry<N> {
    pub target_rects: Vec<LayoutQuad>,
    /// Innermost to outermost, including the root scrolling element.
    pub scroll_containers: Vec<LayoutScrollContainerMetrics<N>>,
}

/// Owned inputs for one IntersectionObserver target/root pair.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutIntersectionGeometry {
    pub target_rects: Vec<LayoutQuad>,
    pub root_rect: LayoutQuad,
    pub ancestor_clips: Vec<LayoutQuad>,
    pub target_has_layout: bool,
    pub target_visible: bool,
    pub root_clips_overflow: bool,
    pub root_is_layout_ancestor: bool,
}

/// One request in an explicit same-pass geometry batch.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutQuery<N> {
    DocumentMetrics,
    BoxModel {
        source: N,
    },
    ClientRects {
        source: N,
    },
    ContentQuads {
        source: N,
    },
    TextRangeRects {
        source: N,
        utf16_range: Range<usize>,
    },
    ElementMetrics {
        source: N,
    },
    ScrollIntoViewGeometry {
        source: N,
    },
    IntersectionGeometry {
        target: N,
        root: Option<N>,
    },
    HitTest {
        point: LayoutPoint,
        ignore_pointer_events_none: bool,
    },
    HitTestAll {
        point: LayoutPoint,
        ignore_pointer_events_none: bool,
    },
    CaretPosition {
        point: LayoutPoint,
    },
    EventOffset {
        source: N,
        point: LayoutPoint,
    },
}

/// A high-level batch that must be answered from one full layout pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayoutQueryBatch<N> {
    pub queries: Vec<LayoutQuery<N>>,
}

impl<N> LayoutQueryBatch<N> {
    pub fn new(queries: Vec<LayoutQuery<N>>) -> Self {
        Self { queries }
    }

    pub fn push(&mut self, query: LayoutQuery<N>) {
        self.queries.push(query);
    }
}

/// One answer corresponding to the same-index query in a batch.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutQueryAnswer<N> {
    DocumentMetrics(LayoutDocumentMetrics),
    BoxModel(Option<LayoutBoxModel>),
    ClientRects(Vec<LayoutQuad>),
    ContentQuads(Vec<LayoutQuad>),
    TextRangeRects(Vec<LayoutQuad>),
    ElementMetrics(Option<LayoutElementMetrics<N>>),
    ScrollIntoViewGeometry(Option<LayoutScrollIntoViewGeometry<N>>),
    IntersectionGeometry(Option<LayoutIntersectionGeometry>),
    HitTest(Option<LayoutHit<N>>),
    HitTestAll(Vec<LayoutHit<N>>),
    CaretPosition(Option<LayoutCaretPosition<N>>),
    EventOffset(Option<LayoutPoint>),
}

/// Minimal owned results derived from one frozen layout tree.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutAnswers<N> {
    pub answers: Vec<LayoutQueryAnswer<N>>,
    pub metrics: LayoutPassMetrics,
}

/// Common real/mock geometry boundary used by renderer consumers.
pub trait GeometryProvider {
    type NodeId: Copy + Debug + Eq + Hash;

    /// Answers a batch from the provider's latest layout state.
    ///
    /// The provider decides whether this requires a fresh pass or can reuse an
    /// already-owned tree; callers must not assume that one call equals one
    /// layout computation.
    fn answer(
        &mut self,
        reason: LayoutFlushReason,
        viewport: LayoutViewport,
        queries: &LayoutQueryBatch<Self::NodeId>,
    ) -> Result<LayoutAnswers<Self::NodeId>, LayoutError>;
}

/// Retained footprint for one frozen layout tree.
///
/// The byte count is an allocation-capacity estimate for the tree's box,
/// fragment, source-provenance, scroll, transform, and clip storage. It
/// excludes allocator metadata and every pass-only diagnostic or paint value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutTreeRetentionMetrics {
    pub box_count: usize,
    pub fragment_count: usize,
    pub estimated_geometry_bytes: usize,
}

/// Maximum geometry boxes retained in the single latest-layout snapshot.
pub const MAX_RETAINED_LAYOUT_BOXES: usize = 1_000_000;
/// Maximum fragments retained in the single latest-layout snapshot.
pub const MAX_RETAINED_LAYOUT_FRAGMENTS: usize = 4_000_000;
/// Maximum estimated allocation capacity retained by one frozen layout tree.
pub const MAX_RETAINED_LAYOUT_TREE_BYTES: usize = 256 * 1024 * 1024;

/// Immutable, DOM-independent layout tree produced by one complete pass.
///
/// The box tree is stored densely through parent IDs. Text/inline fragments,
/// coordinate spaces, clips, and scroll extents are canonical layout data:
/// they preserve results that cannot be reconstructed after the working tree,
/// Taffy caches, Parley state, and computed styles are dropped. Source and
/// hit-test indexes are derived from the box provenance and fragments.
pub struct FrozenLayoutTree<N>
where
    N: Copy + Debug + Eq + Hash,
{
    pub viewport: LayoutViewport,
    pub viewport_scroll: LayoutPoint,
    pub content_size: LayoutSize,
    pub root_box: LayoutOutputBoxId,
    pub boxes: Vec<FrozenLayoutBox<N>>,
    pub fragments: Vec<LayoutFragment>,
    /// Source/box relationships for `display: contents` nodes, which own no
    /// principal box but can still nominate rendered descendants for scroll.
    pub scroll_proxy_links: Vec<(N, LayoutOutputBoxId)>,
    viewport_coordinate_space: FrozenCoordinateSpace,
    pub clip_chain: Vec<LayoutClipNode>,
}

/// Transient products of exactly one complete layout demand.
///
/// Consumers may inspect the tree and take an optional paint snapshot while
/// handling the demand. Only [`FrozenLayoutTree`] crosses the latest-layout
/// retention boundary; diagnostics, metrics, and paint remain pass-owned.
pub struct LayoutPassResult<N>
where
    N: Copy + Debug + Eq + Hash,
{
    pub tree: FrozenLayoutTree<N>,
    pub diagnostics: Vec<PaintDiagnostic>,
    pub metrics: LayoutPassMetrics,
    paint_snapshot: Option<PaintSnapshot>,
}

impl<N> Deref for LayoutPassResult<N>
where
    N: Copy + Debug + Eq + Hash,
{
    type Target = FrozenLayoutTree<N>;

    fn deref(&self) -> &Self::Target {
        &self.tree
    }
}

impl<N> LayoutPassResult<N>
where
    N: Copy + Debug + Eq + Hash,
{
    pub fn paint_snapshot(&self) -> Option<&PaintSnapshot> {
        self.paint_snapshot.as_ref()
    }

    pub fn take_paint_snapshot(&mut self) -> Result<PaintSnapshot, LayoutError> {
        self.paint_snapshot
            .take()
            .ok_or(LayoutError::PaintProjectionNotRequested)
    }

    pub fn into_paint_snapshot(self) -> Result<PaintSnapshot, LayoutError> {
        self.paint_snapshot
            .ok_or(LayoutError::PaintProjectionNotRequested)
    }

    /// Consumes every pass-only product and returns the sole retainable tree.
    pub fn into_tree(self) -> FrozenLayoutTree<N> {
        self.tree
    }

    pub fn retention_metrics(&self) -> LayoutTreeRetentionMetrics {
        self.tree.retention_metrics()
    }

    pub fn validate_retention_budget(&self) -> Result<(), LayoutError> {
        self.tree.validate_retention_budget()
    }

    pub fn answer_queries(&self, batch: &LayoutQueryBatch<N>) -> LayoutAnswers<N> {
        self.tree.answer_queries(batch, self.metrics)
    }
}

impl<N> FrozenLayoutTree<N>
where
    N: Copy + Debug + Eq + Hash,
{
    pub fn retention_metrics(&self) -> LayoutTreeRetentionMetrics {
        fn allocation<T>(capacity: usize) -> usize {
            capacity.saturating_mul(std::mem::size_of::<T>())
        }

        let box_allocations = self.boxes.iter().fold(0usize, |bytes, layout_box| {
            bytes.saturating_add(allocation::<LayoutFragmentId>(
                layout_box.fragments.capacity(),
            ))
        });
        let estimated_geometry_bytes = std::mem::size_of::<Self>()
            .saturating_add(allocation::<FrozenLayoutBox<N>>(self.boxes.capacity()))
            .saturating_add(allocation::<LayoutFragment>(self.fragments.capacity()))
            .saturating_add(allocation::<(N, LayoutOutputBoxId)>(
                self.scroll_proxy_links.capacity(),
            ))
            .saturating_add(allocation::<LayoutClipNode>(self.clip_chain.capacity()))
            .saturating_add(box_allocations);
        LayoutTreeRetentionMetrics {
            box_count: self.boxes.len(),
            fragment_count: self.fragments.len(),
            estimated_geometry_bytes,
        }
    }

    /// Rejects a tree that would make the single latest-layout slot an
    /// unbounded retained allocation.
    pub fn validate_retention_budget(&self) -> Result<(), LayoutError> {
        validate_retention_metrics(self.retention_metrics())
    }

    pub fn box_geometry(&self, id: LayoutOutputBoxId) -> Option<&LayoutBoxGeometry> {
        self.boxes
            .get(id.index())
            .map(|layout_box| &layout_box.geometry)
    }

    pub fn fragment(&self, id: LayoutFragmentId) -> Option<&LayoutFragment> {
        self.fragments.get(id.index())
    }

    pub fn coordinate_space(&self, id: LayoutCoordinateSpaceId) -> Option<&FrozenCoordinateSpace> {
        match id.index() {
            0 => Some(&self.viewport_coordinate_space),
            index => self
                .boxes
                .get(index - 1)
                .map(|layout_box| &layout_box.coordinate_space),
        }
    }

    pub fn scroll_extent(&self, id: LayoutOutputBoxId) -> Option<&LayoutScrollExtent> {
        self.boxes
            .get(id.index())
            .map(|layout_box| &layout_box.scroll_extent)
    }

    /// Derives the source view from canonical box provenance.
    ///
    /// No source hash table survives the pass. The returned IDs are copied
    /// into one short-lived query value.
    pub fn source_output(&self, source: N) -> Option<LayoutNodeOutput> {
        let mut found = false;
        let mut output = LayoutNodeOutput::default();
        for layout_box in &self.boxes {
            if layout_box.principal_source == Some(source) {
                output.principal_box = Some(layout_box.id);
                found = true;
            }
            if layout_box.geometry_source == Some(source) {
                output
                    .fragments
                    .extend(layout_box.fragments.iter().copied().filter(|id| {
                        self.fragment(*id).is_some_and(|fragment| {
                            !matches!(fragment.kind, LayoutFragmentKind::Line { .. })
                        })
                    }));
                found = true;
            }
        }
        for (proxy_source, box_id) in &self.scroll_proxy_links {
            if *proxy_source == source {
                output.scroll_proxy_boxes.push(*box_id);
                found = true;
            }
        }
        found.then_some(output)
    }

    pub fn element_metrics_for_source(&self, source: N) -> Option<LayoutElementMetrics<N>> {
        self.element_metrics_for_source_with_offset_parent_filter(source, |_| true)
    }

    /// Resolves CSSOM View element metrics while allowing the renderer to hide
    /// flat-tree ancestors that do not belong to the queried element's
    /// ancestor tree scopes.
    ///
    /// Shadow DOM tree-scope visibility is an HTML/DOM concern, not a CSS box
    /// tree concern. The frozen tree therefore retains the complete box chain
    /// and lets its short-lived consumer supply that one predicate. Geometry
    /// is still derived wholly from this pass; no live layout state is read or
    /// retained here.
    pub fn element_metrics_for_source_with_offset_parent_filter(
        &self,
        source: N,
        mut offset_parent_is_exposed: impl FnMut(N) -> bool,
    ) -> Option<LayoutElementMetrics<N>> {
        let output = self.source_output(source)?;
        let box_id = output.principal_box?;
        let geometry = self.box_geometry(box_id)?;
        let extent = self.scroll_extent(box_id)?;
        let coordinate_space = self.coordinate_space(geometry.coordinate_space)?;
        let is_root = box_id == self.root_box;
        let offset_parent_id = self.offset_parent_box(box_id, &mut offset_parent_is_exposed);
        let offset_parent = offset_parent_id.and_then(|id| {
            self.boxes
                .get(id.index())
                .and_then(|layout_box| layout_box.geometry_source)
        });
        let offset_parent_origin = offset_parent_id
            .and_then(|id| self.box_geometry(id))
            .map(|parent| {
                if parent.is_body_element {
                    if parent.position == LayoutPosition::Static {
                        LayoutPoint::ZERO
                    } else {
                        parent.layout_origin_in_document
                    }
                } else {
                    LayoutPoint::new(
                        parent.layout_origin_in_document.x + parent.padding_box.x,
                        parent.layout_origin_in_document.y + parent.padding_box.y,
                    )
                }
            })
            .unwrap_or(LayoutPoint::ZERO);
        let layout_origin = output
            .fragments
            .iter()
            .find_map(|id| {
                let fragment = self.fragment(*id)?;
                let LayoutFragmentKind::InlineBox {
                    box_id: fragment_box,
                    ..
                } = fragment.kind
                else {
                    return None;
                };
                if fragment_box != box_id {
                    return None;
                }
                let border = fragment.box_model?.border;
                let owner = self.coordinate_space(fragment.coordinate_space)?.owner?;
                let owner_origin = self.box_geometry(owner)?.layout_origin_in_document;
                Some(LayoutPoint::new(
                    owner_origin.x + border.x,
                    owner_origin.y + border.y,
                ))
            })
            .unwrap_or(geometry.layout_origin_in_document);
        let client_size = if is_root {
            LayoutSize::new(
                self.viewport.css_width as f32,
                self.viewport.css_height as f32,
            )
        } else {
            LayoutSize::new(geometry.padding_box.width, geometry.padding_box.height)
        };
        let scroll_size = if is_root {
            LayoutSize::new(
                extent.scroll_size.width.max(self.content_size.width),
                extent.scroll_size.height.max(self.content_size.height),
            )
        } else {
            extent.scroll_size
        };
        Some(LayoutElementMetrics {
            offset_parent,
            offset_position: LayoutPoint::new(
                layout_origin.x - offset_parent_origin.x,
                layout_origin.y - offset_parent_origin.y,
            ),
            offset_size: LayoutSize::new(geometry.border_box.width, geometry.border_box.height),
            content_size: LayoutSize::new(geometry.content_box.width, geometry.content_box.height),
            client_size,
            client_border: LayoutPoint::new(
                geometry.padding_box.x - geometry.border_box.x,
                geometry.padding_box.y - geometry.border_box.y,
            ),
            scroll_size,
            scroll_offset: extent.applied_offset,
            minimum_scroll_offset: extent.minimum_offset,
            maximum_scroll_offset: extent.maximum_offset,
            scrollport: coordinate_space
                .local_to_viewport
                .map_rect(extent.scrollport),
            scrollable_overflow: coordinate_space
                .local_to_viewport
                .map_rect(extent.scrollable_overflow),
            is_scroll_container: extent.is_scroll_container,
            clips_overflow: extent.clips_overflow,
            visible: geometry.visible,
            pointer_events: geometry.pointer_events,
        })
    }

    /// Resolves a viewport point into the coordinate system Blink uses for
    /// `MouseEvent.offsetX/Y`: a box target's padding edge, or the shared IFC
    /// coordinate space for a flattened inline layout object.
    pub fn event_offset_for_source(
        &self,
        source: N,
        viewport_point: LayoutPoint,
    ) -> Option<LayoutPoint> {
        let output = self.source_output(source)?;
        let box_id = output.principal_box?;
        if let Some(inline_fragment) = output.fragments.iter().find_map(|id| {
            let fragment = self.fragment(*id)?;
            matches!(
                fragment.kind,
                LayoutFragmentKind::InlineBox {
                    box_id: fragment_box,
                    ..
                } if fragment_box == box_id
            )
            .then_some(fragment)
        }) {
            let inverse = self
                .coordinate_space(inline_fragment.coordinate_space)?
                .local_to_viewport
                .inverse()?;
            return Some(inverse.map_point(viewport_point));
        }

        let geometry = self.box_geometry(box_id)?;
        let inverse = self
            .coordinate_space(geometry.coordinate_space)?
            .local_to_viewport
            .inverse()?;
        let mut local = inverse.map_point(viewport_point);
        local.x -= geometry.padding_box.x - geometry.border_box.x;
        local.y -= geometry.padding_box.y - geometry.border_box.y;
        Some(local)
    }

    pub fn box_model_for_source(&self, source: N) -> Option<LayoutBoxModel> {
        let output = self.source_output(source)?;
        let fragment_models = output
            .fragments
            .iter()
            .filter_map(|id| self.fragment(*id))
            .filter_map(|fragment| {
                fragment
                    .box_model
                    .map(|model| (fragment.coordinate_space, model))
            })
            .collect::<Vec<_>>();
        if !fragment_models.is_empty() {
            return self.project_fragment_box_models(&fragment_models);
        }

        let box_id = output.principal_box?;
        let geometry = self.box_geometry(box_id)?;
        self.project_local_box_model(
            geometry.coordinate_space,
            LayoutFragmentBoxModel {
                content: geometry.content_box,
                padding: geometry.padding_box,
                border: geometry.border_box,
                margin: geometry.margin_box,
            },
        )
    }

    pub fn scroll_into_view_geometry_for_source(
        &self,
        source: N,
    ) -> Option<LayoutScrollIntoViewGeometry<N>> {
        let output = self.source_output(source)?;
        let (target_box, target_rects) = if let Some(box_id) = output.principal_box {
            let mut rects = self.client_rects_for_source(source);
            if rects.is_empty() {
                rects = self.content_quads_for_source(source);
            }
            (box_id, rects)
        } else if let Some(box_id) = output
            .fragments
            .iter()
            .find_map(|fragment| self.fragment_box(*fragment))
        {
            (box_id, self.content_quads_for_source(source))
        } else {
            output.scroll_proxy_boxes.iter().find_map(|box_id| {
                let rects = self.scroll_target_rects_for_box(*box_id);
                (!rects.is_empty()).then_some((*box_id, rects))
            })?
        };
        if target_rects.is_empty() {
            return None;
        }
        let mut candidate = self
            .box_geometry(target_box)
            .and_then(|geometry| geometry.layout_parent.or(geometry.parent));
        let mut seen = HashSet::new();
        let mut scroll_containers = Vec::new();
        while let Some(box_id) = candidate {
            let geometry = self.box_geometry(box_id)?;
            let extent = self.scroll_extent(box_id)?;
            if (extent.is_scroll_container || box_id == self.root_box)
                && let Some(container_source) = self
                    .boxes
                    .get(box_id.index())
                    .and_then(|layout_box| layout_box.geometry_source)
                && seen.insert(container_source)
                && let Some(metrics) = self.element_metrics_for_source(container_source)
            {
                scroll_containers.push(LayoutScrollContainerMetrics {
                    source: container_source,
                    metrics,
                });
            }
            candidate = geometry.layout_parent.or(geometry.parent);
        }
        Some(LayoutScrollIntoViewGeometry {
            target_rects,
            scroll_containers,
        })
    }

    fn fragment_box(&self, fragment: LayoutFragmentId) -> Option<LayoutOutputBoxId> {
        match self.fragment(fragment)?.kind {
            LayoutFragmentKind::Box { box_id }
            | LayoutFragmentKind::InlineBox { box_id, .. }
            | LayoutFragmentKind::Text { box_id, .. } => Some(box_id),
            LayoutFragmentKind::Line { .. } => None,
        }
    }

    fn scroll_target_rects_for_box(&self, box_id: LayoutOutputBoxId) -> Vec<LayoutQuad> {
        let Some(geometry) = self.box_geometry(box_id) else {
            return Vec::new();
        };
        let rects = geometry
            .fragments
            .iter()
            .filter_map(|fragment| self.fragment(*fragment))
            .filter(|fragment| match fragment.kind {
                LayoutFragmentKind::Box {
                    box_id: fragment_box,
                }
                | LayoutFragmentKind::InlineBox {
                    box_id: fragment_box,
                    ..
                }
                | LayoutFragmentKind::Text {
                    box_id: fragment_box,
                    ..
                } => fragment_box == box_id,
                LayoutFragmentKind::Line { .. } => false,
            })
            .filter_map(|fragment| {
                self.coordinate_space(fragment.coordinate_space)
                    .map(|space| space.local_to_viewport.map_rect(fragment.rect))
            })
            .collect::<Vec<_>>();
        if !rects.is_empty() {
            return rects;
        }
        self.coordinate_space(geometry.coordinate_space)
            .map(|space| vec![space.local_to_viewport.map_rect(geometry.border_box)])
            .unwrap_or_default()
    }

    pub fn intersection_geometry(
        &self,
        target: N,
        root: Option<N>,
    ) -> Option<LayoutIntersectionGeometry> {
        let target_output = self.source_output(target);
        let target_box = target_output
            .as_ref()
            .and_then(|output| output.principal_box);
        let root_box = root.and_then(|source| self.source_output(source)?.principal_box);
        let root_is_layout_ancestor = match (target_box, root_box, root) {
            (_, _, None) => true,
            (Some(target_box), Some(root_box), Some(_)) => {
                self.box_is_layout_descendant_of(target_box, root_box)
            }
            _ => false,
        };
        let root_clips_overflow = root_box
            .and_then(|root_box| self.scroll_extent(root_box))
            .is_some_and(|extent| extent.clips_overflow);
        let root_rect = if root.is_none() {
            LayoutTransform2D::IDENTITY.map_rect(LayoutRect::new(
                0.0,
                0.0,
                self.viewport.css_width as f32,
                self.viewport.css_height as f32,
            ))
        } else if let Some((geometry, extent, space)) = root_box.and_then(|root_box| {
            let geometry = self.box_geometry(root_box)?;
            Some((
                geometry,
                self.scroll_extent(root_box)?,
                self.coordinate_space(geometry.coordinate_space)?,
            ))
        }) {
            let local_rect = if extent.clips_overflow {
                extent.scrollport
            } else {
                geometry.border_box
            };
            space.local_to_viewport.map_rect(local_rect)
        } else {
            LayoutTransform2D::IDENTITY.map_rect(LayoutRect::ZERO)
        };

        let mut clip_ids = HashSet::new();
        let mut ancestor_clips = Vec::new();
        let mut add_clip_chain = |mut clip: Option<LayoutClipChainId>| {
            while let Some(id) = clip {
                let Some(node) = self.clip_chain.get(id.index()) else {
                    break;
                };
                if node.owner == root_box {
                    break;
                }
                if node.owner.is_some()
                    && clip_ids.insert(id)
                    && let Some(space) = self.coordinate_space(node.coordinate_space)
                {
                    ancestor_clips.push(space.local_to_viewport.map_rect(node.rect));
                }
                clip = node.parent;
            }
        };
        if let Some(target_output) = target_output.as_ref() {
            for fragment in target_output
                .fragments
                .iter()
                .filter_map(|id| self.fragment(*id))
            {
                add_clip_chain(fragment.clip_chain);
            }
        }
        if target_output
            .as_ref()
            .is_none_or(|output| output.fragments.is_empty())
            && let Some(target_box) = target_box
            && let Some(geometry) = self.box_geometry(target_box)
        {
            add_clip_chain(geometry.clip_chain);
        }
        let target_visible = target_box
            .and_then(|id| self.box_geometry(id))
            .is_some_and(|geometry| geometry.visible);
        Some(LayoutIntersectionGeometry {
            target_rects: self.client_rects_for_source(target),
            root_rect,
            ancestor_clips,
            target_has_layout: target_box.is_some(),
            target_visible,
            root_clips_overflow,
            root_is_layout_ancestor,
        })
    }

    fn box_is_layout_descendant_of(
        &self,
        mut candidate: LayoutOutputBoxId,
        ancestor: LayoutOutputBoxId,
    ) -> bool {
        loop {
            if candidate == ancestor {
                return true;
            }
            let Some(parent) = self
                .box_geometry(candidate)
                .and_then(|geometry| geometry.layout_parent.or(geometry.parent))
            else {
                return false;
            };
            candidate = parent;
        }
    }

    pub fn client_rects_for_source(&self, source: N) -> Vec<LayoutQuad> {
        let Some(output) = self.source_output(source) else {
            return Vec::new();
        };
        output
            .fragments
            .iter()
            .filter_map(|id| self.fragment(*id))
            .filter(|fragment| {
                matches!(
                    fragment.kind,
                    LayoutFragmentKind::Box { .. } | LayoutFragmentKind::InlineBox { .. }
                )
            })
            .filter_map(|fragment| {
                self.coordinate_space(fragment.coordinate_space)
                    .map(|space| space.local_to_viewport.map_rect(fragment.rect))
            })
            .collect()
    }

    pub fn content_quads_for_source(&self, source: N) -> Vec<LayoutQuad> {
        let Some(output) = self.source_output(source) else {
            return Vec::new();
        };
        output
            .fragments
            .iter()
            .filter_map(|id| self.fragment(*id))
            .filter(|fragment| {
                matches!(
                    fragment.kind,
                    LayoutFragmentKind::Box { .. }
                        | LayoutFragmentKind::InlineBox { .. }
                        | LayoutFragmentKind::Text { .. }
                )
            })
            .filter_map(|fragment| {
                let rect = fragment
                    .box_model
                    .map(|model| model.content)
                    .unwrap_or(fragment.rect);
                self.coordinate_space(fragment.coordinate_space)
                    .map(|space| space.local_to_viewport.map_rect(rect))
            })
            .collect()
    }

    pub fn text_range_rects(&self, source: N, utf16_range: Range<usize>) -> Vec<LayoutQuad> {
        let Some(output) = self.source_output(source) else {
            return Vec::new();
        };
        #[derive(Clone, Copy)]
        struct SelectedTextRect {
            box_id: LayoutOutputBoxId,
            line_index: usize,
            rtl: bool,
            coordinate_space: LayoutCoordinateSpaceId,
            rect: LayoutRect,
        }

        let mut selected = output
            .fragments
            .iter()
            .filter_map(|id| self.fragment(*id))
            .filter_map(|fragment| {
                let LayoutFragmentKind::Text {
                    box_id,
                    line_index,
                    source_utf16_range,
                    rtl,
                    ..
                } = &fragment.kind
                else {
                    return None;
                };
                let source_len = source_utf16_range
                    .end
                    .saturating_sub(source_utf16_range.start);
                let (selected_start, selected_end) = if utf16_range.is_empty() {
                    if utf16_range.start < source_utf16_range.start
                        || utf16_range.start > source_utf16_range.end
                    {
                        return None;
                    }
                    let point = utf16_range
                        .start
                        .saturating_sub(source_utf16_range.start)
                        .min(source_len);
                    (point, point)
                } else {
                    let start = source_utf16_range.start.max(utf16_range.start);
                    let end = source_utf16_range.end.min(utf16_range.end);
                    if start >= end {
                        return None;
                    }
                    (
                        start.saturating_sub(source_utf16_range.start),
                        end.saturating_sub(source_utf16_range.start),
                    )
                };
                let denominator = source_len.max(1) as f32;
                let start_ratio = selected_start as f32 / denominator;
                let end_ratio = selected_end as f32 / denominator;
                let visual_start_ratio = if *rtl { 1.0 - end_ratio } else { start_ratio };
                let rect = LayoutRect::new(
                    fragment.rect.x + fragment.rect.width * visual_start_ratio,
                    fragment.rect.y,
                    fragment.rect.width * (end_ratio - start_ratio),
                    fragment.rect.height,
                );
                Some(SelectedTextRect {
                    box_id: *box_id,
                    line_index: *line_index,
                    rtl: *rtl,
                    coordinate_space: fragment.coordinate_space,
                    rect,
                })
            })
            .collect::<Vec<_>>();

        // Parley exposes cluster-level source fragments, while CSSOM View
        // exposes one Range rect per contiguous directional run on a line.
        // Keep opposite bidi runs separate, but merge adjacent clusters from
        // the same source box before mapping through transforms.
        selected.sort_by(|left, right| {
            left.coordinate_space
                .index()
                .cmp(&right.coordinate_space.index())
                .then_with(|| left.box_id.index().cmp(&right.box_id.index()))
                .then_with(|| left.line_index.cmp(&right.line_index))
                .then_with(|| left.rtl.cmp(&right.rtl))
                .then_with(|| left.rect.y.total_cmp(&right.rect.y))
                .then_with(|| left.rect.x.total_cmp(&right.rect.x))
        });
        let mut merged: Vec<SelectedTextRect> = Vec::with_capacity(selected.len());
        for fragment in selected {
            let can_merge = merged.last().is_some_and(|previous| {
                let tolerance = previous
                    .rect
                    .width
                    .abs()
                    .max(fragment.rect.width.abs())
                    .max(1.0)
                    * f32::EPSILON
                    * 16.0;
                previous.box_id == fragment.box_id
                    && previous.line_index == fragment.line_index
                    && previous.rtl == fragment.rtl
                    && previous.coordinate_space == fragment.coordinate_space
                    && (previous.rect.y - fragment.rect.y).abs() <= tolerance
                    && (previous.rect.height - fragment.rect.height).abs() <= tolerance
                    && fragment.rect.x <= previous.rect.right() + tolerance
            });
            if can_merge {
                let previous = merged.last_mut().expect("checked above");
                previous.rect = previous.rect.union(fragment.rect);
            } else {
                merged.push(fragment);
            }
        }

        let mut quads = merged
            .into_iter()
            .filter_map(|fragment| {
                self.coordinate_space(fragment.coordinate_space)
                    .map(|space| space.local_to_viewport.map_rect(fragment.rect))
            })
            .collect::<Vec<_>>();
        quads.sort_by(|left, right| {
            let left = left.bounding_rect();
            let right = right.bounding_rect();
            left.y
                .total_cmp(&right.y)
                .then_with(|| left.x.total_cmp(&right.x))
        });
        quads
    }

    pub fn hit_test(
        &self,
        viewport_point: LayoutPoint,
        ignore_pointer_events_none: bool,
    ) -> Option<LayoutHit<N>> {
        self.hit_test_entries().into_iter().find_map(|entry| {
            self.hit_for_entry(&entry, viewport_point, ignore_pointer_events_none)
        })
    }

    pub fn hit_test_all(
        &self,
        viewport_point: LayoutPoint,
        ignore_pointer_events_none: bool,
    ) -> Vec<LayoutHit<N>> {
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for entry in self.hit_test_entries() {
            let Some(hit) = self.hit_for_entry(&entry, viewport_point, ignore_pointer_events_none)
            else {
                continue;
            };
            if seen.insert(hit.source) {
                hits.push(hit);
            }
        }
        hits
    }

    pub fn caret_position(&self, viewport_point: LayoutPoint) -> Option<LayoutCaretPosition<N>> {
        let entries = self.hit_test_entries();
        let top_entry = entries
            .iter()
            .find(|entry| self.hit_for_entry(entry, viewport_point, true).is_some())?;
        let top_box = self.fragment_box_id(top_entry.fragment)?;
        let text_entry = entries
            .iter()
            .filter(|entry| entry.is_text)
            .filter(|entry| {
                self.fragment_box_id(entry.fragment)
                    .is_some_and(|box_id| self.box_is_construction_descendant_of(box_id, top_box))
            })
            .filter_map(|entry| {
                self.hit_entry_distance_to_point(entry, viewport_point)
                    .map(|distance| (entry, distance))
            })
            .min_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(entry, _)| entry);
        if let Some(entry) = text_entry {
            return self.caret_position_for_text_entry(entry, viewport_point);
        }

        let fragment = self.fragment(top_entry.fragment)?;
        let space = self.coordinate_space(top_entry.coordinate_space)?;
        let inverse = space.local_to_viewport.inverse()?;
        let local_point = inverse.map_point(viewport_point);
        let caret_x = if local_point.x <= fragment.rect.x + fragment.rect.width / 2.0 {
            fragment.rect.x
        } else {
            fragment.rect.right()
        };
        let rect = space.local_to_viewport.map_rect(LayoutRect::new(
            caret_x,
            fragment.rect.y,
            0.0,
            fragment.rect.height,
        ));
        Some(LayoutCaretPosition {
            source: top_entry.source,
            utf16_offset: None,
            rect,
            ancestor_boxes: self.ancestor_box_models(top_box),
        })
    }

    /// Builds the front-to-back hit candidates for one query.
    ///
    /// Paint order, source provenance, transforms, and clips are canonical
    /// tree data. The duplicated candidate vector is deliberately temporary.
    fn hit_test_entries(&self) -> Vec<LayoutHitTestEntry<N>> {
        let mut entries = self
            .fragments
            .iter()
            .filter_map(|fragment| {
                let paint_order = fragment.paint_order?;
                let (box_id, is_text) = match fragment.kind {
                    LayoutFragmentKind::Box { box_id }
                    | LayoutFragmentKind::InlineBox { box_id, .. } => (box_id, false),
                    LayoutFragmentKind::Text { box_id, .. } => (box_id, true),
                    LayoutFragmentKind::Line { .. } => return None,
                };
                let layout_box = self.boxes.get(box_id.index())?;
                if !layout_box.visible {
                    return None;
                }
                Some(LayoutHitTestEntry {
                    source: layout_box.hit_source?,
                    fragment: fragment.id,
                    coordinate_space: fragment.coordinate_space,
                    clip_chain: fragment.clip_chain,
                    local_rect: fragment.rect,
                    paint_order,
                    is_text,
                    pointer_events: layout_box.pointer_events,
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.paint_order));
        entries
    }

    fn caret_position_for_text_entry(
        &self,
        entry: &LayoutHitTestEntry<N>,
        viewport_point: LayoutPoint,
    ) -> Option<LayoutCaretPosition<N>> {
        let fragment = self.fragment(entry.fragment)?;
        let LayoutFragmentKind::Text {
            box_id,
            source_utf16_range,
            rtl,
            ..
        } = &fragment.kind
        else {
            return None;
        };
        let space = self.coordinate_space(entry.coordinate_space)?;
        let local_point = space.local_to_viewport.inverse()?.map_point(viewport_point);
        let source_len = source_utf16_range
            .end
            .saturating_sub(source_utf16_range.start);
        let on_left_half = local_point.x <= fragment.rect.x + fragment.rect.width * 0.5;
        let at_source_start = if *rtl { !on_left_half } else { on_left_half };
        let fragment_offset = if at_source_start { 0 } else { source_len };
        let caret_x = if at_source_start == *rtl {
            fragment.rect.right()
        } else {
            fragment.rect.x
        };
        Some(LayoutCaretPosition {
            source: entry.source,
            utf16_offset: Some(source_utf16_range.start + fragment_offset),
            rect: space.local_to_viewport.map_rect(LayoutRect::new(
                caret_x,
                fragment.rect.y,
                0.0,
                fragment.rect.height,
            )),
            ancestor_boxes: self.ancestor_box_models(*box_id),
        })
    }

    fn hit_entry_distance_to_point(
        &self,
        entry: &LayoutHitTestEntry<N>,
        viewport_point: LayoutPoint,
    ) -> Option<f64> {
        let space = self.coordinate_space(entry.coordinate_space)?;
        let local_point = space.local_to_viewport.inverse()?.map_point(viewport_point);
        let nearest_local = LayoutPoint::new(
            local_point
                .x
                .clamp(entry.local_rect.x, entry.local_rect.right()),
            local_point
                .y
                .clamp(entry.local_rect.y, entry.local_rect.bottom()),
        );
        let nearest_viewport = space.local_to_viewport.map_point(nearest_local);
        if !self.point_passes_clip_chain(nearest_viewport, entry.clip_chain) {
            return None;
        }
        let dx = f64::from(nearest_viewport.x - viewport_point.x);
        let dy = f64::from(nearest_viewport.y - viewport_point.y);
        Some(dx * dx + dy * dy)
    }

    fn fragment_box_id(&self, fragment: LayoutFragmentId) -> Option<LayoutOutputBoxId> {
        match self.fragment(fragment)?.kind {
            LayoutFragmentKind::Box { box_id }
            | LayoutFragmentKind::InlineBox { box_id, .. }
            | LayoutFragmentKind::Text { box_id, .. } => Some(box_id),
            LayoutFragmentKind::Line { owner, .. } => Some(owner),
        }
    }

    fn box_is_construction_descendant_of(
        &self,
        mut candidate: LayoutOutputBoxId,
        ancestor: LayoutOutputBoxId,
    ) -> bool {
        loop {
            if candidate == ancestor {
                return true;
            }
            let Some(parent) = self.box_geometry(candidate).and_then(|box_| box_.parent) else {
                return false;
            };
            candidate = parent;
        }
    }

    fn ancestor_box_models(&self, mut box_id: LayoutOutputBoxId) -> Vec<(N, LayoutBoxModel)> {
        let mut seen = HashSet::new();
        let mut ancestors = Vec::new();
        loop {
            if let Some(source) = self
                .boxes
                .get(box_id.index())
                .and_then(|layout_box| layout_box.geometry_source)
                && seen.insert(source)
                && let Some(model) = self.box_model_for_source(source)
            {
                ancestors.push((source, model));
            }
            let Some(parent) = self.box_geometry(box_id).and_then(|box_| box_.parent) else {
                break;
            };
            box_id = parent;
        }
        ancestors
    }

    fn hit_for_entry(
        &self,
        entry: &LayoutHitTestEntry<N>,
        viewport_point: LayoutPoint,
        ignore_pointer_events_none: bool,
    ) -> Option<LayoutHit<N>> {
        if !ignore_pointer_events_none && !entry.pointer_events {
            return None;
        }
        if !self.point_passes_clip_chain(viewport_point, entry.clip_chain) {
            return None;
        }
        let inverse = self
            .coordinate_space(entry.coordinate_space)?
            .local_to_viewport
            .inverse()?;
        let local_point = inverse.map_point(viewport_point);
        entry.local_rect.contains(local_point).then_some(LayoutHit {
            source: entry.source,
            fragment: Some(entry.fragment),
            local_point,
            is_text: entry.is_text,
            box_model: self.box_model_for_source(entry.source),
        })
    }

    pub fn answer_queries(
        &self,
        batch: &LayoutQueryBatch<N>,
        metrics: LayoutPassMetrics,
    ) -> LayoutAnswers<N> {
        let answers = batch
            .queries
            .iter()
            .map(|query| match query {
                LayoutQuery::DocumentMetrics => {
                    LayoutQueryAnswer::DocumentMetrics(LayoutDocumentMetrics {
                        viewport: self.viewport,
                        viewport_scroll: self.viewport_scroll,
                        content_size: self.content_size,
                    })
                }
                LayoutQuery::BoxModel { source } => {
                    LayoutQueryAnswer::BoxModel(self.box_model_for_source(*source))
                }
                LayoutQuery::ClientRects { source } => {
                    LayoutQueryAnswer::ClientRects(self.client_rects_for_source(*source))
                }
                LayoutQuery::ContentQuads { source } => {
                    LayoutQueryAnswer::ContentQuads(self.content_quads_for_source(*source))
                }
                LayoutQuery::TextRangeRects {
                    source,
                    utf16_range,
                } => LayoutQueryAnswer::TextRangeRects(
                    self.text_range_rects(*source, utf16_range.clone()),
                ),
                LayoutQuery::ElementMetrics { source } => {
                    LayoutQueryAnswer::ElementMetrics(self.element_metrics_for_source(*source))
                }
                LayoutQuery::ScrollIntoViewGeometry { source } => {
                    LayoutQueryAnswer::ScrollIntoViewGeometry(
                        self.scroll_into_view_geometry_for_source(*source),
                    )
                }
                LayoutQuery::IntersectionGeometry { target, root } => {
                    LayoutQueryAnswer::IntersectionGeometry(
                        self.intersection_geometry(*target, *root),
                    )
                }
                LayoutQuery::HitTest {
                    point,
                    ignore_pointer_events_none,
                } => LayoutQueryAnswer::HitTest(self.hit_test(*point, *ignore_pointer_events_none)),
                LayoutQuery::HitTestAll {
                    point,
                    ignore_pointer_events_none,
                } => LayoutQueryAnswer::HitTestAll(
                    self.hit_test_all(*point, *ignore_pointer_events_none),
                ),
                LayoutQuery::CaretPosition { point } => {
                    LayoutQueryAnswer::CaretPosition(self.caret_position(*point))
                }
                LayoutQuery::EventOffset { source, point } => {
                    LayoutQueryAnswer::EventOffset(self.event_offset_for_source(*source, *point))
                }
            })
            .collect();
        LayoutAnswers { answers, metrics }
    }

    fn point_passes_clip_chain(
        &self,
        viewport_point: LayoutPoint,
        mut clip: Option<LayoutClipChainId>,
    ) -> bool {
        while let Some(id) = clip {
            let Some(node) = self.clip_chain.get(id.index()) else {
                return false;
            };
            let Some(inverse) = self
                .coordinate_space(node.coordinate_space)
                .and_then(|space| space.local_to_viewport.inverse())
            else {
                return false;
            };
            if !node.rect.contains(inverse.map_point(viewport_point)) {
                return false;
            }
            clip = node.parent;
        }
        true
    }

    fn project_fragment_box_models(
        &self,
        models: &[(LayoutCoordinateSpaceId, LayoutFragmentBoxModel)],
    ) -> Option<LayoutBoxModel> {
        let first_space = models.first()?.0;
        if models.iter().all(|(space, _)| *space == first_space) {
            let mut combined = models.first()?.1;
            for (_, model) in &models[1..] {
                combined.content = combined.content.union(model.content);
                combined.padding = combined.padding.union(model.padding);
                combined.border = combined.border.union(model.border);
                combined.margin = combined.margin.union(model.margin);
            }
            return self.project_local_box_model(first_space, combined);
        }

        let mut projected = models
            .iter()
            .filter_map(|(space, model)| self.project_local_box_model(*space, *model));
        let first = projected.next()?;
        let combined = projected.fold(first, |mut combined, model| {
            combined.content = axis_aligned_union(combined.content, model.content);
            combined.padding = axis_aligned_union(combined.padding, model.padding);
            combined.border = axis_aligned_union(combined.border, model.border);
            combined.margin = axis_aligned_union(combined.margin, model.margin);
            combined
        });
        Some(combined)
    }

    fn offset_parent_box(
        &self,
        box_id: LayoutOutputBoxId,
        offset_parent_is_exposed: &mut impl FnMut(N) -> bool,
    ) -> Option<LayoutOutputBoxId> {
        let geometry = self.box_geometry(box_id)?;
        if box_id == self.root_box || geometry.is_body_element {
            return None;
        }
        let base_is_positioned = geometry.position != LayoutPosition::Static;
        let mut in_fixed_position_chain = geometry.position == LayoutPosition::Fixed;
        let mut candidate = geometry.parent;
        while let Some(id) = candidate {
            let parent = self.box_geometry(id)?;
            let source = self
                .boxes
                .get(id.index())
                .and_then(|layout_box| layout_box.geometry_source);
            let Some(source) = source else {
                candidate = parent.parent;
                continue;
            };

            if !offset_parent_is_exposed(source) {
                if parent.establishes_fixed_containing_block {
                    in_fixed_position_chain = false;
                } else if parent.position == LayoutPosition::Fixed {
                    in_fixed_position_chain = true;
                }
                candidate = parent.parent;
                continue;
            }

            if in_fixed_position_chain {
                if parent.establishes_fixed_containing_block {
                    return Some(id);
                }
            } else if parent.establishes_positioned_containing_block
                || parent.is_body_element
                || (!base_is_positioned && parent.is_table_offset_parent)
            {
                return Some(id);
            }
            in_fixed_position_chain |= parent.position == LayoutPosition::Fixed;
            candidate = parent.parent;
        }
        None
    }

    fn project_local_box_model(
        &self,
        coordinate_space: LayoutCoordinateSpaceId,
        model: LayoutFragmentBoxModel,
    ) -> Option<LayoutBoxModel> {
        let transform = self.coordinate_space(coordinate_space)?.local_to_viewport;
        Some(LayoutBoxModel {
            content: transform.map_rect(model.content),
            padding: transform.map_rect(model.padding),
            border: transform.map_rect(model.border),
            margin: transform.map_rect(model.margin),
        })
    }

    pub(crate) fn new(
        viewport: LayoutViewport,
        viewport_scroll: LayoutPoint,
        content_size: LayoutSize,
        root_box: LayoutOutputBoxId,
        boxes: Vec<FrozenLayoutBox<N>>,
        fragments: Vec<LayoutFragment>,
        scroll_proxy_links: Vec<(N, LayoutOutputBoxId)>,
        viewport_coordinate_space: FrozenCoordinateSpace,
        clip_chain: Vec<LayoutClipNode>,
    ) -> Self {
        Self {
            viewport,
            viewport_scroll,
            content_size,
            root_box,
            boxes,
            fragments,
            scroll_proxy_links,
            viewport_coordinate_space,
            clip_chain,
        }
    }
}

impl<N> LayoutPassResult<N>
where
    N: Copy + Debug + Eq + Hash,
{
    pub(crate) fn new(
        tree: FrozenLayoutTree<N>,
        diagnostics: Vec<PaintDiagnostic>,
        metrics: LayoutPassMetrics,
        paint_snapshot: Option<PaintSnapshot>,
    ) -> Self {
        Self {
            tree,
            diagnostics,
            metrics,
            paint_snapshot,
        }
    }
}

fn axis_aligned_union(left: LayoutQuad, right: LayoutQuad) -> LayoutQuad {
    LayoutTransform2D::IDENTITY.map_rect(left.bounding_rect().union(right.bounding_rect()))
}

fn validate_retention_metrics(metrics: LayoutTreeRetentionMetrics) -> Result<(), LayoutError> {
    if metrics.box_count > MAX_RETAINED_LAYOUT_BOXES
        || metrics.fragment_count > MAX_RETAINED_LAYOUT_FRAGMENTS
        || metrics.estimated_geometry_bytes > MAX_RETAINED_LAYOUT_TREE_BYTES
    {
        return Err(LayoutError::TreeRetentionBudgetExceeded {
            boxes: metrics.box_count,
            fragments: metrics.fragment_count,
            estimated_bytes: metrics.estimated_geometry_bytes,
            max_boxes: MAX_RETAINED_LAYOUT_BOXES,
            max_fragments: MAX_RETAINED_LAYOUT_FRAGMENTS,
            max_bytes: MAX_RETAINED_LAYOUT_TREE_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_concatenation_and_inverse_round_trip() {
        let transform = LayoutTransform2D::translation(20.0, 30.0)
            .concatenate(LayoutTransform2D::rotation(std::f64::consts::FRAC_PI_2))
            .concatenate(LayoutTransform2D::scale(2.0, 3.0));
        let point = LayoutPoint::new(4.0, 5.0);
        let mapped = transform.map_point(point);
        let restored = transform.inverse().expect("invertible").map_point(mapped);
        assert!((restored.x - point.x).abs() <= 0.0001);
        assert!((restored.y - point.y).abs() <= 0.0001);
    }

    #[test]
    fn rectangle_contains_uses_half_open_right_and_bottom_edges() {
        let rect = LayoutRect::new(10.0, 20.0, 30.0, 40.0);
        assert!(rect.contains(LayoutPoint::new(10.0, 20.0)));
        assert!(rect.contains(LayoutPoint::new(39.999, 59.999)));
        assert!(!rect.contains(LayoutPoint::new(40.0, 30.0)));
        assert!(!rect.contains(LayoutPoint::new(20.0, 60.0)));
        assert!(!LayoutRect::new(0.0, 0.0, 0.0, 10.0).contains(LayoutPoint::ZERO));
    }

    #[test]
    fn retained_tree_budget_reports_each_bounded_dimension() {
        for metrics in [
            LayoutTreeRetentionMetrics {
                box_count: MAX_RETAINED_LAYOUT_BOXES + 1,
                ..Default::default()
            },
            LayoutTreeRetentionMetrics {
                fragment_count: MAX_RETAINED_LAYOUT_FRAGMENTS + 1,
                ..Default::default()
            },
            LayoutTreeRetentionMetrics {
                estimated_geometry_bytes: MAX_RETAINED_LAYOUT_TREE_BYTES + 1,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                validate_retention_metrics(metrics),
                Err(LayoutError::TreeRetentionBudgetExceeded { .. })
            ));
        }
        validate_retention_metrics(LayoutTreeRetentionMetrics {
            box_count: MAX_RETAINED_LAYOUT_BOXES,
            fragment_count: MAX_RETAINED_LAYOUT_FRAGMENTS,
            estimated_geometry_bytes: MAX_RETAINED_LAYOUT_TREE_BYTES,
        })
        .expect("each exact retention limit should be accepted");
    }
}
