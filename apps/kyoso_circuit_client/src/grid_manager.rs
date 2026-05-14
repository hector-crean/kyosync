//! Per-layer grid manager + snap-to-cell behaviour.
//!
//! Each [`CircuitLayer`] owns a [`GridLayout3d`] — a uniform rectangular
//! grid lying on that layer's xz-plane. Drag operations on circuit
//! nodes snap their x/z translation to the nearest cell centre; the
//! y component is left alone so [`crate::scene::sync_layer_y`] (and
//! the layer's own `y_offset`) remain authoritative.
//!
//! Grids are configured at startup from the layer's default cell size
//! and a hardcoded extent; runtime tweaks happen through the
//! [`GridManager`] resource. The visual representation (gizmo grid) is
//! drawn by [`draw_grid_planes`] using the same `GridLayout3d`, so the
//! visual cells and the snap target always agree.
//!
//! Snap is on by default and can be toggled with the `Q` hotkey.

use std::collections::HashMap;

use bevy::picking::backend::ray::RayMap;
use bevy::picking::events::{DragEnd, Pointer};
use bevy::picking::pointer::PointerId;
use bevy::prelude::*;
use kyoso_camera::markers::MainCamera;
use kyoso_camera::raycast::RayMapExt;
use kyoso_circuit::{CircuitLayer, CircuitNode, OnLayer};

/// Uniform rectangular grid on the xz-plane at a given y.
///
/// `origin` is the min corner of cell `(0, 0)`; cells extend in +X and
/// +Z. `extent_cells` is how many cells wide / deep the grid is —
/// outside that range, snapping still rounds to a grid line but
/// queries that bound the index (e.g. visualization) clamp to it.
#[derive(Clone, Copy, Debug)]
pub struct GridLayout3d {
    pub origin_xz: Vec2,
    pub cell_size_xz: Vec2,
    pub extent_cells: UVec2,
    pub y: f32,
}

impl GridLayout3d {
    /// Build a grid centred at the world origin (in xz) with a given
    /// per-side extent in cells.
    #[must_use]
    pub fn centered(extent_cells: UVec2, cell_size_xz: Vec2, y: f32) -> Self {
        let half = Vec2::new(
            extent_cells.x as f32 * cell_size_xz.x * 0.5,
            extent_cells.y as f32 * cell_size_xz.y * 0.5,
        );
        Self {
            origin_xz: -half,
            cell_size_xz,
            extent_cells,
            y,
        }
    }

    /// World-space centre of cell `(cx, cz)`. Negative indices and
    /// indices beyond `extent_cells` are allowed (used by snapping
    /// when the cursor strays off the visualised area).
    #[must_use]
    pub fn cell_center(&self, cx: i32, cz: i32) -> Vec3 {
        let xz = self.origin_xz
            + Vec2::new(
                (cx as f32 + 0.5) * self.cell_size_xz.x,
                (cz as f32 + 0.5) * self.cell_size_xz.y,
            );
        Vec3::new(xz.x, self.y, xz.y)
    }

    /// Snap a world-space point to the nearest cell centre on this
    /// grid. The returned point's y matches the grid's y; the input
    /// y is ignored.
    #[must_use]
    pub fn snap(&self, world: Vec3) -> Vec3 {
        let rel = Vec2::new(world.x, world.z) - self.origin_xz;
        let cx = (rel.x / self.cell_size_xz.x).floor() as i32;
        let cz = (rel.y / self.cell_size_xz.y).floor() as i32;
        self.cell_center(cx, cz)
    }

    /// World-space extent of the grid (cells × cell_size), useful for
    /// drawing the visual plane.
    #[must_use]
    pub fn world_size(&self) -> Vec2 {
        Vec2::new(
            self.extent_cells.x as f32 * self.cell_size_xz.x,
            self.extent_cells.y as f32 * self.cell_size_xz.y,
        )
    }
}

/// Per-layer grid configuration plus a global snap-enabled toggle.
#[derive(Resource, Debug)]
pub struct GridManager {
    layouts: HashMap<CircuitLayer, GridLayout3d>,
    snap_enabled: bool,
}

impl Default for GridManager {
    fn default() -> Self {
        let mut layouts = HashMap::new();
        for layer in CircuitLayer::all() {
            // Same cell size on every layer for now — keeps cross-layer
            // wires visually aligned. Future: per-layer overrides for
            // mechanical vs. signal pitch.
            layouts.insert(
                layer,
                GridLayout3d::centered(UVec2::splat(16), Vec2::splat(1.0), layer.y_offset()),
            );
        }
        Self {
            layouts,
            snap_enabled: true,
        }
    }
}

impl GridManager {
    #[must_use]
    pub fn layout(&self, layer: CircuitLayer) -> &GridLayout3d {
        self.layouts
            .get(&layer)
            .expect("GridManager initializes a layout for every CircuitLayer variant")
    }

    pub fn set_layout(&mut self, layer: CircuitLayer, layout: GridLayout3d) {
        self.layouts.insert(layer, layout);
    }

    #[must_use]
    pub fn snap_enabled(&self) -> bool {
        self.snap_enabled
    }

    pub fn toggle_snap(&mut self) -> bool {
        self.snap_enabled = !self.snap_enabled;
        self.snap_enabled
    }

    /// Convenience: snap a world point to a specific layer's grid.
    /// Used by spawn-time snapping so newly-placed components land on
    /// a cell centre instead of an arbitrary cursor-raycast point.
    #[must_use]
    pub fn snap_world(&self, layer: CircuitLayer, world: Vec3) -> Vec3 {
        self.layout(layer).snap(world)
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct GridManagerPlugin;

impl Plugin for GridManagerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GridManager>();
        // Ensure `Pointer<DragEnd>` is registered as a Message even
        // in headless / test setups that don't load Bevy's picking
        // plugin — otherwise `snap_on_drag_end`'s `MessageReader`
        // panics on first run with "Message not initialized". The
        // picking plugin's own registration is idempotent, so this
        // is safe to call alongside it in the visual app.
        app.add_message::<Pointer<DragEnd>>();
        app.add_systems(Update, (snap_hotkey, snap_on_drag_end));
    }
}

/// `Q` toggles `GridManager::snap_enabled`.
fn snap_hotkey(keys: Option<Res<ButtonInput<KeyCode>>>, mut grid: ResMut<GridManager>) {
    let Some(keys) = keys else { return };
    if keys.just_pressed(KeyCode::KeyQ) {
        grid.toggle_snap();
    }
}

/// On every [`Pointer<DragEnd>`] event, snap the just-dragged circuit
/// node's xz to its layer's nearest cell centre. The drag itself is
/// freeform — the user sees the component glide under the cursor —
/// then it commits to a cell on release.
///
/// `y` is owned by [`crate::scene::sync_layer_y`] (matched to the
/// layer's offset) and not touched here.
fn snap_on_drag_end(
    grid: Res<GridManager>,
    mut drag_ends: MessageReader<Pointer<DragEnd>>,
    mut nodes: Query<(&mut Transform, &OnLayer), With<CircuitNode>>,
) {
    if !grid.snap_enabled() {
        return;
    }
    for event in drag_ends.read() {
        let Ok((mut transform, on_layer)) = nodes.get_mut(event.entity) else {
            continue;
        };
        let Some(layer) = on_layer.layer() else {
            continue;
        };
        let snapped = grid.layout(layer).snap(transform.translation);
        transform.translation.x = snapped.x;
        transform.translation.z = snapped.z;
    }
}

// ---------------------------------------------------------------------------
// Visual: draw each layer's grid as a gizmo grid sized + positioned
// from the configured `GridLayout3d`. Replaces `scene::draw_layer_planes`.
// ---------------------------------------------------------------------------

/// Cursor-driven ghost cell on the active layer. Draws a filled-ish
/// rectangle outlining the cell the cursor's snap target would land
/// in if you clicked or finished a drag right now. Hidden when snap
/// is off or the active layer is hidden.
///
/// Lives in `VisualPlugin` (not `GridManagerPlugin`) because it needs
/// the picking pipeline's [`RayMap`] — only present alongside Bevy's
/// [`MeshPickingPlugin`].
pub fn draw_snap_preview(
    grid: Res<GridManager>,
    layer_manager: Res<crate::layer_manager::LayerManager>,
    ray_map: Option<Res<RayMap>>,
    cameras: Query<Entity, With<MainCamera>>,
    mut gizmos: Gizmos,
) {
    if !grid.snap_enabled() {
        return;
    }
    let active = layer_manager.active();
    if !layer_manager.is_visible(active) {
        return;
    }
    let Some(ray_map) = ray_map.as_deref() else {
        return;
    };
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    let layout = grid.layout(active);
    let Some(world) = ray_map.pointer_plane_intersection(
        camera,
        PointerId::Mouse,
        Vec3::new(0.0, layout.y, 0.0),
        Vec3::Y,
    ) else {
        return;
    };
    let snapped = layout.snap(world);
    // Lift the ghost a hair above the grid plane so the outline reads
    // clearly even when the grid gizmo overlaps it.
    let center = Vec3::new(snapped.x, snapped.y + 0.01, snapped.z);
    let c = active.color_srgb();
    let outline = Color::srgba(c[0], c[1], c[2], 0.9);
    let fill = Color::srgba(c[0], c[1], c[2], 0.25);
    // Filled cross-hair inside the cell, plus the cell outline. Bevy's
    // gizmo API doesn't have a filled-rect primitive on a 3D plane, so
    // we approximate with two crossed lines that read as a "target".
    gizmos.rect(
        Isometry3d::new(center, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        layout.cell_size_xz,
        outline,
    );
    let half = layout.cell_size_xz * 0.5;
    gizmos.line(
        Vec3::new(center.x - half.x, center.y, center.z),
        Vec3::new(center.x + half.x, center.y, center.z),
        fill,
    );
    gizmos.line(
        Vec3::new(center.x, center.y, center.z - half.y),
        Vec3::new(center.x, center.y, center.z + half.y),
        fill,
    );
}

/// Draw each layer's grid as a gizmo grid on the xz-plane at its
/// y_offset. The active layer is drawn opaque; inactive layers fade
/// to a faint outline so the user knows which plane they're working
/// on without losing the layered-board context.
pub fn draw_grid_planes(
    grid: Res<GridManager>,
    active: Res<crate::layer_manager::LayerManager>,
    mut gizmos: Gizmos,
) {
    for layer in CircuitLayer::all() {
        if !active.is_visible(layer) {
            continue;
        }
        let layout = grid.layout(layer);
        let c = layer.color_srgb();
        let alpha = if layer == active.active() { 0.55 } else { 0.15 };
        let color = Color::srgba(c[0], c[1], c[2], alpha);
        let world_size = layout.world_size();
        let center = Vec3::new(
            layout.origin_xz.x + world_size.x * 0.5,
            layout.y,
            layout.origin_xz.y + world_size.y * 0.5,
        );
        gizmos.grid(
            Isometry3d::new(center, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
            layout.extent_cells,
            layout.cell_size_xz,
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_to_nearest_cell_center() {
        let layout = GridLayout3d::centered(UVec2::splat(4), Vec2::splat(1.0), 0.0);
        // origin is (-2, -2); cell (2, 2) centre = (0.5, 0, 0.5)
        let s = layout.snap(Vec3::new(0.3, 0.0, 0.6));
        assert!((s.x - 0.5).abs() < 1e-6, "x snap, got {}", s.x);
        assert!((s.z - 0.5).abs() < 1e-6, "z snap, got {}", s.z);
        assert!(s.y.abs() < 1e-6, "y unchanged from layout, got {}", s.y);
    }

    #[test]
    fn snap_is_idempotent() {
        let layout = GridLayout3d::centered(UVec2::splat(4), Vec2::splat(1.0), 0.0);
        let once = layout.snap(Vec3::new(2.3, 0.0, -1.7));
        let twice = layout.snap(once);
        assert_eq!(once, twice);
    }

    #[test]
    fn manager_has_layout_per_layer() {
        let manager = GridManager::default();
        for layer in CircuitLayer::all() {
            let layout = manager.layout(layer);
            assert!((layout.y - layer.y_offset()).abs() < 1e-6);
        }
    }
}
