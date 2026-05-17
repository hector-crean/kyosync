//! Built-in [`SchemaSync`] impls for common Bevy components.
//!
//! Currently included:
//! - [`TransformSchema`] — LWW per `translation` / `rotation` / `scale`,
//!   plumbed for [`bevy::prelude::Transform`].

use bevy::prelude::{Quat, Transform, Vec3};
use kyoso_crdt::DeriveCrdt;
use kyoso_crdt::types::LwwRegister;

use crate::schema_sync::{SchemaField, SchemaMutations, SchemaSync};

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

    fn diff(&self, doc: &Self::Schema) -> SchemaMutations<Self> {
        // One uniform `SchemaField` delegation per field — same shape
        // the `derive(SchemaSync)` macro emits. `scale`'s baseline is
        // `Vec3::ONE` (Transform's default), not `Vec3::ZERO`, which is
        // why the component default is threaded through explicitly.
        let default = Self::default();
        let mut out = Vec::new();
        out.extend(
            doc
                .translation
                .diff(&self.translation, &default.translation)
                .into_iter()
                .map(TransformSchemaMut::Translation),
        );
        out.extend(
            doc
                .rotation
                .diff(&self.rotation, &default.rotation)
                .into_iter()
                .map(TransformSchemaMut::Rotation),
        );
        out.extend(
            doc
                .scale
                .diff(&self.scale, &default.scale)
                .into_iter()
                .map(TransformSchemaMut::Scale),
        );
        out
    }

    fn write_back(&mut self, schema: &Self::Schema) {
        schema.translation.project_to(&mut self.translation);
        schema.rotation.project_to(&mut self.rotation);
        schema.scale.project_to(&mut self.scale);
    }
}
