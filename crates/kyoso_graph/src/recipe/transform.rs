//! Recipe transforms: what to do when a pattern matches.
//!
//! Three built-in transform kinds:
//!
//! - **Annotate** -- attach a marker component to matched entities.
//! - **Collapse** -- replace the matched subgraph with a single composite node.
//! - **Upgrade** -- mutate edge types in the matched subgraph (e.g. mark as
//!   aromatic).

use std::collections::HashMap;
use std::fmt::Debug;

use bevy::prelude::*;
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use petgraph::Directed;

use crate::GraphMessage;
use crate::queries::GraphQuery;

use super::matcher::find_embeddings;
use super::pattern::Pattern;

// ---------------------------------------------------------------------------
// FunctionalGroup marker
// ---------------------------------------------------------------------------

/// Marker component added by the Annotate transform.
#[derive(Component, Clone, Debug, Reflect)]
pub struct FunctionalGroup {
    pub name: String,
}

// ---------------------------------------------------------------------------
// RecipeTransform
// ---------------------------------------------------------------------------

/// What to do when a recipe's pattern matches.
pub enum RecipeTransform {
    /// Tag every entity in the embedding with a [`FunctionalGroup`].
    Annotate { group_name: String },
    /// Replace the matched subgraph with a single composite node.
    Collapse { composite_label: String },
    /// (Reserved for future use) Upgrade edges in the match, e.g. mark as
    /// aromatic.
    Upgrade,
}

// ---------------------------------------------------------------------------
// Recipe
// ---------------------------------------------------------------------------

/// A pattern + transform pair.
pub struct Recipe {
    pub pattern: Pattern,
    pub transform: RecipeTransform,
}

impl Recipe {
    pub fn annotate(pattern: Pattern, group_name: impl Into<String>) -> Self {
        Self {
            pattern,
            transform: RecipeTransform::Annotate {
                group_name: group_name.into(),
            },
        }
    }

    pub fn collapse(pattern: Pattern, composite_label: impl Into<String>) -> Self {
        Self {
            pattern,
            transform: RecipeTransform::Collapse {
                composite_label: composite_label.into(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// RecipeBook resource
// ---------------------------------------------------------------------------

/// Collection of recipes to evaluate each time the graph changes.
#[derive(Resource, Default)]
pub struct RecipeBook {
    pub recipes: Vec<Recipe>,
}

impl RecipeBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, recipe: Recipe) {
        self.recipes.push(recipe);
    }
}

// ---------------------------------------------------------------------------
// Bevy systems
// ---------------------------------------------------------------------------

/// System that evaluates all recipes against the current ECS topology and
/// applies transforms.
pub fn evaluate_recipes<Node, Edge>(
    mut commands: Commands,
    q: GraphQuery<'_, '_, &Node, &Edge>,
    recipe_book: Option<Res<RecipeBook>>,
    node_symbol_q: Query<&Name, With<Node>>,
    existing_groups: Query<Entity, With<FunctionalGroup>>,
    mut reader: MessageReader<GraphMessage>,
) where
    Node: Component + Debug,
    Edge: Component + Debug,
{
    // Only re-evaluate when topology changes.
    let mut changed = false;
    for msg in reader.read() {
        match msg {
            GraphMessage::NodeAdded { .. }
            | GraphMessage::NodeRemoved { .. }
            | GraphMessage::EdgeAdded { .. }
            | GraphMessage::EdgeRemoved { .. } => {
                changed = true;
            }
            _ => {}
        }
    }
    if !changed {
        return;
    }

    let Some(recipe_book) = recipe_book else {
        return;
    };

    // Strip previous annotations.
    for e in existing_groups.iter() {
        commands.entity(e).remove::<FunctionalGroup>();
    }

    // Build a lightweight host graph for the matcher, sourced directly
    // from ECS. The host graph drives subgraph isomorphism in
    // `find_embeddings`; the matcher itself is unchanged.
    let mut host: StableGraph<(), (), Directed> = StableGraph::new();
    let mut entity_to_host: HashMap<Entity, NodeIndex> = HashMap::new();
    let mut host_to_entity: HashMap<NodeIndex, Entity> = HashMap::new();
    for (entity, _, _, _) in q.nodes_iter() {
        let hi = host.add_node(());
        entity_to_host.insert(entity, hi);
        host_to_entity.insert(hi, entity);
    }
    for (_, edge_from, edge_to, _) in q.edges_iter() {
        if let (Some(&a), Some(&b)) = (
            entity_to_host.get(&edge_from.0),
            entity_to_host.get(&edge_to.0),
        ) {
            host.add_edge(a, b, ());
        }
    }

    let node_symbol = |hi: NodeIndex| -> String {
        let Some(&entity) = host_to_entity.get(&hi) else {
            return String::new();
        };
        node_symbol_q
            .get(entity)
            .map(|n| n.as_str().to_string())
            .unwrap_or_default()
    };

    let edge_order = |from_hi: NodeIndex, to_hi: NodeIndex| -> Option<u8> {
        // Simplified: all edges have order 1 in the mirror (the mirror
        // stores phantom states). Real order would come from an ECS
        // component query.  For now we return 1 so single-bond patterns
        // match.
        if host.find_edge(from_hi, to_hi).is_some() {
            Some(1)
        } else {
            None
        }
    };

    for recipe in &recipe_book.recipes {
        let embeddings = find_embeddings(&recipe.pattern, &host, &node_symbol, &edge_order);

        for embedding in &embeddings {
            match &recipe.transform {
                RecipeTransform::Annotate { group_name } => {
                    for (_, &host_ni) in embedding {
                        let Some(&entity) = host_to_entity.get(&host_ni) else {
                            continue;
                        };
                        commands.entity(entity).insert(FunctionalGroup {
                            name: group_name.clone(),
                        });
                    }
                }
                RecipeTransform::Collapse { .. } | RecipeTransform::Upgrade => {
                    // Collapse / upgrade are structurally complex transforms;
                    // left as stubs for now.  A collapse would:
                    //  1. Spawn a new composite node.
                    //  2. Re-wire external edges to the composite.
                    //  3. Despawn matched internal nodes + edges.
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct RecipePlugin<Node, Edge>
where
    Node: Component + Debug,
    Edge: Component + Debug,
{
    _phantom: std::marker::PhantomData<(Node, Edge)>,
}

impl<Node, Edge> Default for RecipePlugin<Node, Edge>
where
    Node: Component + Debug,
    Edge: Component + Debug,
{
    fn default() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<Node, Edge> Plugin for RecipePlugin<Node, Edge>
where
    Node: Component + Debug,
    Edge: Component + Debug,
{
    fn build(&self, app: &mut App) {
        app.register_type::<FunctionalGroup>()
            .init_resource::<RecipeBook>()
            .add_systems(
                Update,
                evaluate_recipes::<Node, Edge>
                    .in_set(crate::GraphSystemSet::Consumption),
            );
    }
}
