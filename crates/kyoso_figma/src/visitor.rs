//! Per-variant visitor over the live ECS scene tree.
//!
//! ECS-side analogue of [`crate::walker::NodeVisitor`] (the Figma-API
//! *import* visitor). Both share the same shape — typed per-variant
//! `visit_*` methods with a context object — so the mental model
//! carries over from import to runtime.
//!
//! Named `SceneVisitor` (not `NodeVisitor`) to avoid colliding with
//! the import-side trait at the crate-root re-export.
//!
//! Where the import walker walks `figma_api::SubcanvasNode` trees, this
//! walker walks the live Bevy ECS via
//! [`FigmaNodeQuery::walk_visit`](crate::node::FigmaNodeQuery::walk_visit),
//! materializing each entity through [`NodeVariant::materialize`]
//! before dispatching.

use bevy::prelude::Entity;

use crate::node::{FrameData, RectangleData, TextData};

/// Context passed to each `visit_*` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisitContext {
    /// Entity being visited.
    pub entity: Entity,
    /// Depth relative to the walk root (0 = the root).
    pub depth: usize,
    /// Parent entity, or `None` for the walk root.
    pub parent: Option<Entity>,
}

/// Visitor return value — what should the walker do next?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Traverse {
    /// Descend into this node's children.
    Continue,
    /// Skip this node's subtree; continue with siblings.
    SkipChildren,
    /// Halt the walk entirely.
    Stop,
}

/// Per-variant visit hooks. Default impls do nothing and return
/// [`Traverse::Continue`], so implementors only override the variants
/// they care about.
pub trait SceneVisitor {
    fn visit_frame(&mut self, _data: &FrameData, _ctx: &VisitContext) -> Traverse {
        Traverse::Continue
    }
    fn visit_rectangle(&mut self, _data: &RectangleData, _ctx: &VisitContext) -> Traverse {
        Traverse::Continue
    }
    fn visit_text(&mut self, _data: &TextData, _ctx: &VisitContext) -> Traverse {
        Traverse::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{FigmaNodeQuery, FrameData, RectangleData, TextData};
    use crate::{FigmaNode, NodeKind};
    use bevy::ecs::system::RunSystemOnce;
    use bevy::prelude::*;
    use kyoso_graph::components::{EdgeFrom, EdgeTo};
    use kyoso_graph::tree::{OrderKey, TreeEdge, TreeParent};

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app
    }

    fn spawn_tree_fixture(commands: &mut Commands) -> (Entity, Entity, Entity) {
        let root = commands
            .spawn((FrameData::default(), Transform::IDENTITY, FigmaNode, TreeParent(None)))
            .id();
        let rect = commands
            .spawn((
                RectangleData::default(),
                Transform::IDENTITY,
                FigmaNode,
                TreeParent(Some(root)),
                OrderKey("a".into()),
            ))
            .id();
        let text = commands
            .spawn((
                TextData::default(),
                Transform::IDENTITY,
                FigmaNode,
                TreeParent(Some(root)),
                OrderKey("b".into()),
            ))
            .id();
        commands.spawn((EdgeFrom(root), EdgeTo(rect), TreeEdge));
        commands.spawn((EdgeFrom(root), EdgeTo(text), TreeEdge));
        (root, rect, text)
    }

    #[derive(Default)]
    struct Counter {
        frames: usize,
        rectangles: usize,
        texts: usize,
        max_depth: usize,
    }

    impl SceneVisitor for Counter {
        fn visit_frame(&mut self, _data: &FrameData, ctx: &VisitContext) -> Traverse {
            self.frames += 1;
            self.max_depth = self.max_depth.max(ctx.depth);
            Traverse::Continue
        }
        fn visit_rectangle(&mut self, _data: &RectangleData, ctx: &VisitContext) -> Traverse {
            self.rectangles += 1;
            self.max_depth = self.max_depth.max(ctx.depth);
            Traverse::Continue
        }
        fn visit_text(&mut self, _data: &TextData, ctx: &VisitContext) -> Traverse {
            self.texts += 1;
            self.max_depth = self.max_depth.max(ctx.depth);
            Traverse::Continue
        }
    }

    #[test]
    fn scene_visitor_counts_each_variant_with_depth() {
        let mut app = test_app();
        let (root, _, _) = app
            .world_mut()
            .run_system_once(|mut commands: Commands| spawn_tree_fixture(&mut commands))
            .expect("spawn fixture");

        let counter = app
            .world_mut()
            .run_system_once(move |q: FigmaNodeQuery| {
                let mut c = Counter::default();
                q.walk_visit(root, &mut c);
                c
            })
            .expect("walk system runs");

        assert_eq!(counter.frames, 1);
        assert_eq!(counter.rectangles, 1);
        assert_eq!(counter.texts, 1);
        assert_eq!(counter.max_depth, 1);
    }

    /// Visitor that returns `SkipChildren` on the root Frame — should
    /// see the Frame but not its descendants.
    #[derive(Default)]
    struct Pruner {
        frames: usize,
        rectangles: usize,
        texts: usize,
    }

    impl SceneVisitor for Pruner {
        fn visit_frame(&mut self, _data: &FrameData, _ctx: &VisitContext) -> Traverse {
            self.frames += 1;
            Traverse::SkipChildren
        }
        fn visit_rectangle(&mut self, _: &RectangleData, _: &VisitContext) -> Traverse {
            self.rectangles += 1;
            Traverse::Continue
        }
        fn visit_text(&mut self, _: &TextData, _: &VisitContext) -> Traverse {
            self.texts += 1;
            Traverse::Continue
        }
    }

    #[test]
    fn skip_children_prunes_subtree() {
        let mut app = test_app();
        let (root, _, _) = app
            .world_mut()
            .run_system_once(|mut commands: Commands| spawn_tree_fixture(&mut commands))
            .expect("spawn fixture");

        let pruner = app
            .world_mut()
            .run_system_once(move |q: FigmaNodeQuery| {
                let mut p = Pruner::default();
                q.walk_visit(root, &mut p);
                p
            })
            .expect("walk system runs");

        assert_eq!(pruner.frames, 1);
        assert_eq!(pruner.rectangles, 0, "SkipChildren should prune Rectangle");
        assert_eq!(pruner.texts, 0, "SkipChildren should prune Text");
    }

    // Silence unused-import warning when NodeKind isn't referenced in tests.
    #[allow(dead_code)]
    fn _ensure_node_kind_referenced(k: NodeKind) -> NodeKind { k }
}
