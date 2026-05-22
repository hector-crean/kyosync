//! `Text`: text node with collaborative content + nested typography.
//!
//! - `content` is a `Sequence<char>` so two peers typing into the same
//!   text node interleave deterministically (RGA convergence; see
//!   `kyoso_crdt::types::sequence`).
//! - `style` is a nested `TypeStyle` whose fields each merge
//!   independently (font_family + font_size etc. don't clobber).
//! - `fills` mirror Frame/Rectangle's whole-list LWW.

use bevy::prelude::*;
use kyoso_sync::SchemaSync;
use serde::{Deserialize, Serialize};

use crate::paint::Paint;
use crate::typestyle::TypeStyle;
use crate::NodeKind;

#[derive(
    Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync, Serialize, Deserialize,
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
