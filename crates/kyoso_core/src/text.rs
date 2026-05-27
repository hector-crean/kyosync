//! `Text`: text node with collaborative content + nested typography.
//!
//! - `content` is a `Sequence<char>` so two peers typing into the same
//!   text node interleave deterministically (RGA convergence; see
//!   `kyoso_crdt::types::sequence`).
//! - `style` is a nested `TypeStyle` whose fields each merge
//!   independently (font_family + font_size etc. don't clobber).
//! - `fills` mirror Frame/Rectangle's whole-list LWW.
//!
//! This module is the single home for the Text variant: marker
//! [`Text`] component, owned [`TextData`] bundle, borrowed
//! [`TextQueryData`] projection, and [`NodeVariant`] impl.

use bevy::ecs::query::{QueryData, ROQueryItem};
use bevy::prelude::*;
use kyoso_graph::NodeVariant;
use kyoso_sync::SchemaSync;
use serde::{Deserialize, Serialize};

use crate::node::Node;
use crate::paint::Paint;
use crate::size::Size;
use crate::typestyle::TypeStyle;
use crate::{NodeKind, SceneNode};

#[derive(
    Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync, Serialize, Deserialize,
    schemars::JsonSchema,
)]
#[reflect(Component, Default)]
#[require(NodeKind = NodeKind::Text)]
#[schema(name = "Text")]
pub struct Text {
    #[crdt(sequence)]
    pub content: String,

    #[crdt(nested)]
    pub style: TypeStyle,

    pub fills: Vec<Paint>,
}

/// Owned Text variant payload + Bevy `Bundle`.
#[derive(Bundle, Default, Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
pub struct TextData {
    pub text: Text,
    pub size: Size,
}

/// Borrowed projection for typed queries.
#[derive(QueryData)]
pub struct TextQueryData {
    pub entity: Entity,
    pub text: &'static Text,
    pub size: &'static Size,
    pub kind: &'static NodeKind,
}

impl NodeVariant for Text {
    type Graph = SceneNode;
    type Data = TextData;
    type Query = TextQueryData;
    const KIND: NodeKind = NodeKind::Text;

    fn wrap(data: TextData) -> Node {
        Node::Text(data)
    }

    fn materialize(item: ROQueryItem<'_, '_, TextQueryData>) -> TextData {
        TextData {
            text: item.text.clone(),
            size: item.size.clone(),
        }
    }
}
