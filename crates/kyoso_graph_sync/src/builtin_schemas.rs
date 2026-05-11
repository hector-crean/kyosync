//! Built-in [`SchemaSync`] impls for common Bevy components.
//!
//! Currently included:
//! - [`TransformSchema`] — LWW per `translation` / `rotation` / `scale`,
//!   plumbed for [`bevy::prelude::Transform`].

use bevy::prelude::{Quat, Transform, Vec3};
use kyoso_crdt::types::{LwwMut, LwwRegister};
use kyoso_crdt::{Crdt, DeriveCrdt};

use crate::schema_sync::SchemaSync;

/// Typed schema mirroring [`bevy::prelude::Transform`].
#[derive(Clone, Debug, Default, PartialEq, DeriveCrdt)]
pub struct TransformSchema {
    pub translation: LwwRegister<Vec3>,
    pub rotation: LwwRegister<Quat>,
    pub scale: LwwRegister<Vec3>,
}

impl SchemaSync for Transform {
    type Schema = TransformSchema;
    const SCHEMA_NAME: &'static str = "Transform";

    fn changes_against(
        &self,
        current: &Self::Schema,
    ) -> Vec<<Self::Schema as Crdt>::Mutation> {
        let default = Self::default();
        let mut out = Vec::new();
        if *current.translation.get().unwrap_or(&default.translation) != self.translation {
            out.push(TransformSchemaMut::Translation(LwwMut::Set(self.translation)));
        }
        if *current.rotation.get().unwrap_or(&default.rotation) != self.rotation {
            out.push(TransformSchemaMut::Rotation(LwwMut::Set(self.rotation)));
        }
        if *current.scale.get().unwrap_or(&default.scale) != self.scale {
            out.push(TransformSchemaMut::Scale(LwwMut::Set(self.scale)));
        }
        out
    }

    fn write_back(&mut self, schema: &Self::Schema) {
        if let Some(&t) = schema.translation.get() {
            self.translation = t;
        }
        if let Some(&r) = schema.rotation.get() {
            self.rotation = r;
        }
        if let Some(&s) = schema.scale.get() {
            self.scale = s;
        }
    }
}
