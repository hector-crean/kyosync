
The pivot to replicate Figma's data model crosses the threshold where hand-writing `SchemaSync` impls stops being viable. Figma's API exposes ~50 distinct node/style/effect types with ~20–40 fields each (see [figma-api docs.rs](https://docs.rs/figma-api/latest/figma_api/models/index.html)). A naive translation to typed schemas would be:

  

- 50 schema structs × ~25 fields × 2 LOC per field (changes_against + write_back) = ~2500 LOC of mechanical boilerplate.

- Every one of those 2500 lines must follow the `unwrap_or(&Self::default().<field>)` echo-guard convention or risk silent data loss.

- Field-CRDT type choice (LWW vs OrSet vs Sequence vs CausalMap) needs to be deliberate per field — but most fields want the same default (LWW for scalars).

  

The right answer is a `derive(SchemaSync)` proc-macro that:

1. **Generates the parallel schema struct** (sibling type with field-CRDT replacements).

2. **Generates the `SchemaSync` impl** with the echo-guard convention baked in.

3. **Generates the `derive(Crdt)` on the schema** transitively.

4. **Lets users opt into non-default CRDT semantics** per field via attributes (the AsBindGroup pattern).

  

Reference: Bevy's `AsBindGroup` ([source](https://docs.rs/bevy/latest/bevy/render/render_resource/derive.AsBindGroup.html)) shows the right vocabulary — container attrs for the type-level config, field attrs that drive code generation per field.

  

## IX.1 · Attribute surface

  

### Container attributes

  

```rust

#[derive(Component, Clone, Default, PartialEq, SchemaSync)]

#[schema(name = "Frame")] // → SCHEMA_NAME (default = type name)

pub struct Frame { ... }

```

  

- `name = "..."` — wire-format discriminator. Defaults to the type name. Required to be unique per app.

  

### Field attributes — CRDT type selection

  

Choose at most one CRDT-kind attribute per field. Defaults are listed in §IX.3.

  

```rust

#[crdt(lww)] // LwwRegister<T>; default for scalars and most types

#[crdt(or_set)] // OrSet<T>; for Vec<T>, HashSet<T>, BTreeSet<T>

#[crdt(sequence)] // Sequence<T>; for String, Vec<T> when ordered + collaborative

#[crdt(map)] // CausalMap<K, V>; for HashMap<K, V>, BTreeMap<K, V>

#[crdt(counter)] // PnCounter; for i64 / i32 / u32 / u64

#[crdt(nested)] // recurse into another SchemaSync component

#[crdt(skip)] // exclude from sync entirely

```

  

### Field attributes — refinement

  

```rust

#[crdt(rename = "x")] // wire path uses "x" instead of the Rust field name

#[crdt(default = "expr")] // override default for echo-guard fallback (rare)

#[crdt(with = "Type")] // custom schema-side type (escape hatch for Handle<T>, etc.)

```

  

### Combined example

  

```rust

#[derive(Component, Clone, Default, PartialEq, SchemaSync)]
#[schema(name = "Frame")]
pub struct Frame {
	pub name: String, // implicit lww
	pub absolute_bounding_box: Rectangle, // implicit lww (Rectangle: Default + PartialEq)
	pub visible: bool, // implicit lww
	#[crdt(or_set)]
	pub export_settings: Vec<ExportSetting>, // OrSet<ExportSetting>
	#[crdt(sequence)]
	pub characters: String, // Sequence<char>
	#[crdt(map)]
	pub component_property_definitions: HashMap<String, ComponentPropertyDef>, 
	// CausalMap<String, LwwRegister<...>>
	#[crdt(counter)]
	pub edit_count: i64, // PnCounter
	#[crdt(skip)]
	pub local_hover_state: HoverState, // not synced
	#[crdt(rename = "fillsGeometry")]
	pub fills: Vec<Paint>, // wire path = "fillsGeometry"
	#[crdt(with = "AssetHandleSchema")]
	pub thumbnail: Handle<Image>, // custom schema field
}

```

  

## IX.2 · Code generation contract

  

For the `Frame` example above, the macro generates:

  

```rust

// 1. Schema struct (sibling type)

#[derive(Clone, Debug, Default, PartialEq, ::kyoso_crdt::DeriveCrdt)]

pub struct FrameSchema {
	pub name: ::kyoso_crdt::types::LwwRegister<String>,
	pub absolute_bounding_box: ::kyoso_crdt::types::LwwRegister<Rectangle>,
	pub visible: ::kyoso_crdt::types::LwwRegister<bool>,
	pub export_settings: ::kyoso_crdt::types::OrSet<ExportSetting>,
	pub characters: ::kyoso_crdt::types::Sequence<char>,
	pub component_property_definitions:
::kyoso_crdt::types::CausalMap<String, ::kyoso_crdt::types::LwwRegister<ComponentPropertyDef>>,
	pub edit_count: ::kyoso_crdt::types::PnCounter,
// skipped fields are absent
	pub fills: ::kyoso_crdt::types::OrSet<Paint>, // implicit or_set for Vec — see §IX.3
	pub thumbnail: AssetHandleSchema, // user-provided schema type
}

  

// 2. SchemaSync impl

impl ::kyoso_sync::SchemaSync for Frame {

type Schema = FrameSchema;

const SCHEMA_NAME: &'static str = "Frame";

  

fn changes_against(

&self,

current: &Self::Schema,

) -> Vec<<Self::Schema as ::kyoso_crdt::Crdt>::Mutation> {

let default = <Self as ::core::default::Default>::default();

let mut out = Vec::new();

  

// LWW field — echo-guard convention applied

if *current.name.get().unwrap_or(&default.name) != self.name {

out.push(FrameSchemaMut::Name(::kyoso_crdt::types::LwwMut::Set(self.name.clone())));

}

  

// OrSet field — emit Add for elements in self but not in current

for elem in &self.export_settings {

if !current.export_settings.contains(elem) {

out.push(FrameSchemaMut::ExportSettings(

::kyoso_crdt::types::OrSetMut::Add(elem.clone())));

}

}

// (and Remove for elements in current but not in self — see §IX.5)

  

// Sequence field — diff via LCS or naive replace; see §IX.6 for trade-off

// ...

  

out

}

  

fn write_back(&mut self, schema: &Self::Schema) {

if let Some(v) = schema.name.get() { self.name = v.clone(); }

if let Some(v) = schema.absolute_bounding_box.get() { self.absolute_bounding_box = *v; }

if let Some(v) = schema.visible.get() { self.visible = *v; }

self.export_settings = schema.export_settings.iter().cloned().collect();

self.characters = schema.characters.iter().collect();

self.component_property_definitions = schema

.component_property_definitions

.iter()

.filter_map(|(k, v)| v.get().map(|val| (k.clone(), val.clone())))

.collect();

self.edit_count = schema.edit_count.value();

// skipped fields are not touched

self.fills = schema.fills.iter().cloned().collect();

// `with`-fields delegate to the user's projection

self.thumbnail = AssetHandleSchema::project_to(&schema.thumbnail);

}

}

```

  

The generated code is straight-line, debuggable via `cargo expand`, and uses fully-qualified paths so it works from any consumer crate.

  

## IX.3 · Default CRDT-type rules

  

When a field has no explicit `#[crdt(...)]` attribute, the macro infers based on the Rust type:

  

| Rust type | Default CRDT | Override with |

|---|---|---|

| `T: Serialize + Default + Clone + PartialEq` (scalars, structs, enums) | `LwwRegister<T>` | `#[crdt(...)]` for non-LWW |

| `String` | `LwwRegister<String>` | `#[crdt(sequence)]` for collab text |

| `Vec<T>` | `LwwRegister<Vec<T>>` | `#[crdt(or_set)]`, `#[crdt(sequence)]` |

| `HashSet<T>`, `BTreeSet<T>` | `OrSet<T>` (since LWW-replace on a set is rarely intent) | `#[crdt(lww)]` to force replace |

| `HashMap<K, V>`, `BTreeMap<K, V>` | `LwwRegister<HashMap<K, V>>` | `#[crdt(map)]` for per-key sync |

| `Option<T>` | `LwwRegister<Option<T>>` | (no special handling) |

| Numeric types `i32/i64/u32/u64/f32/f64` | `LwwRegister<T>` | `#[crdt(counter)]` for PN-counter (integers only) |

| `Handle<T>`, `Entity`, function pointers, types missing required bounds | **error**: must use `#[crdt(skip)]` or `#[crdt(with = ...)]` |

  

The principle: defaults match the most common intent, explicit attrs handle the rest. The compiler refuses to silently sync types that don't satisfy the bounds.