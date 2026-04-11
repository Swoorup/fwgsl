use std::collections::BTreeMap;

use naga::{ArraySize, Handle, Module, Scalar, Type, TypeInner, VectorSize};
use shadml_bundler::{CompiledProfile, ShaderBundleManifest};

pub type ReflectedManifest = BTreeMap<String, ReflectedProfile>;

/// Binding name → (size in bytes, min_binding_size), plus optional push-constant size.
pub type BindingSizes = (BTreeMap<String, Option<u64>>, Option<u32>);

#[derive(Debug, Clone)]
pub struct ReflectedProfile {
    pub structs: BTreeMap<String, StructLayout>,
    pub entries: BTreeMap<String, ReflectedEntry>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // render_block used by codegen, other fields may be used by consumers
pub struct ReflectedEntry {
    pub binding_sizes: BTreeMap<String, Option<u64>>,
    pub push_constant_size: Option<u32>,
    /// Name of the render block this entry belongs to, if any.
    /// Entries in the same render block share a pipeline layout.
    pub render_block: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLayout {
    pub size: u32,
    pub alignment: u32,
    pub fields: Vec<FieldLayout>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLayout {
    pub name: String,
    pub offset: u32,
    /// Size of this field's type in bytes (excluding padding).
    pub size: u32,
}

#[derive(Debug, Clone, Copy)]
struct TypeLayout {
    size: Option<u32>,
    alignment: u32,
}

pub fn reflect_profile(profile: &CompiledProfile) -> Result<ReflectedProfile, String> {
    // Build a multimap from bare struct name to its origin module paths.
    // This lets us key reflected layouts by origin module when the name is
    // unambiguous, matching the deduplicated ExportedType.rust_mod_path.
    let mut struct_origins: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    for exported in &profile.exported_types {
        struct_origins
            .entry(exported.name.clone())
            .or_default()
            .push(exported.rust_mod_path.clone());
    }

    let mut structs = BTreeMap::new();
    let mut entries = BTreeMap::new();

    for entry in &profile.entries {
        let module = naga::front::wgsl::parse_str(&entry.wgsl_source).map_err(|error| {
            format!(
                "failed to parse WGSL for '{}': {}",
                entry.shader_name, error
            )
        })?;

        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .map_err(|error| {
            format!(
                "failed to validate WGSL for '{}': {}",
                entry.shader_name, error
            )
        })?;

        let (binding_sizes, push_constant_size) = reflect_binding_sizes(&module)?;
        entries.insert(
            entry.shader_name.clone(),
            ReflectedEntry {
                binding_sizes,
                push_constant_size,
                render_block: entry.render_block.clone(),
            },
        );

        let entry_structs = reflect_structs(&module)?;
        let module_prefix = entry.rust_mod_path.join(".");
        for (name, layout) in entry_structs {
            // Always store under entry path for fallback lookup.
            let entry_key = if module_prefix.is_empty() {
                name.clone()
            } else {
                format!("{}.{}", module_prefix, name)
            };
            structs.insert(entry_key.clone(), layout.clone());

            // Also store under origin path when the name is unambiguous.
            if let Some(origins) = struct_origins.get(&name) {
                if origins.len() == 1 {
                    let origin_prefix = origins[0].join(".");
                    let origin_key = if origin_prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{}.{}", origin_prefix, name)
                    };
                    structs.insert(origin_key, layout);
                }
            }
        }
    }

    Ok(ReflectedProfile { structs, entries })
}

pub fn reflect_manifest(manifest: &ShaderBundleManifest) -> Result<ReflectedManifest, String> {
    let mut reflected = BTreeMap::new();

    for profile in &manifest.profiles {
        if reflected.contains_key(&profile.profile_key) {
            return Err(format!(
                "duplicate reflected profile '{}'",
                profile.profile_key
            ));
        }
        reflected.insert(profile.profile_key.clone(), reflect_profile(profile)?);
    }

    Ok(reflected)
}

fn reflect_structs(module: &Module) -> Result<BTreeMap<String, StructLayout>, String> {
    let mut reflected = BTreeMap::new();

    for (_, ty) in module.types.iter() {
        let Some(name) = ty.name.as_ref() else {
            continue;
        };
        let TypeInner::Struct { members, span } = &ty.inner else {
            continue;
        };

        let alignment = members
            .iter()
            .map(|member| type_layout(module, member.ty))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|layout| layout.alignment)
            .max()
            .unwrap_or(1);

        let fields = members
            .iter()
            .map(|member| {
                let size = type_layout(module, member.ty)
                    .map(|l| l.size.unwrap_or(0))
                    .unwrap_or(0);
                FieldLayout {
                    name: member
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("field_{}", member.offset)),
                    offset: member.offset,
                    size,
                }
            })
            .collect();

        reflected.insert(
            name.clone(),
            StructLayout {
                size: *span,
                alignment,
                fields,
            },
        );
    }

    Ok(reflected)
}

fn reflect_binding_sizes(module: &Module) -> Result<BindingSizes, String> {
    let mut sizes = BTreeMap::new();
    let mut push_constant_size: Option<u32> = None;

    for (_, global) in module.global_variables.iter() {
        let Some(name) = global.name.as_ref() else {
            continue;
        };

        // Detect push constants by address space (naga 29 uses "Immediate" for push constants)
        if global.space == naga::AddressSpace::Immediate {
            let layout = type_layout(module, global.ty)?;
            push_constant_size = Some(layout.size.unwrap_or(0));
            continue;
        }

        let layout = type_layout(module, global.ty)?;
        sizes.insert(name.clone(), layout.size.map(u64::from));
    }

    Ok((sizes, push_constant_size))
}

fn type_layout(module: &Module, handle: Handle<Type>) -> Result<TypeLayout, String> {
    match &module.types[handle].inner {
        TypeInner::Scalar(scalar) | TypeInner::Atomic(scalar) => scalar_layout(*scalar),
        TypeInner::Vector { size, scalar } => vector_layout(*size, *scalar),
        TypeInner::Matrix {
            columns,
            rows,
            scalar,
        } => matrix_layout(*columns, *rows, *scalar),
        TypeInner::Array { base, size, stride } => array_layout(module, *base, size, *stride),
        TypeInner::Struct { members, span } => {
            let alignment = members
                .iter()
                .map(|member| type_layout(module, member.ty))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|layout| layout.alignment)
                .max()
                .unwrap_or(1);
            Ok(TypeLayout {
                size: Some(*span),
                alignment,
            })
        }
        TypeInner::Pointer { .. } | TypeInner::ValuePointer { .. } => Ok(TypeLayout {
            size: Some(4),
            alignment: 4,
        }),
        // Opaque resource types (textures, samplers, binding arrays) have no CPU-side layout
        TypeInner::Image { .. } | TypeInner::Sampler { .. } => Ok(TypeLayout {
            size: None,
            alignment: 1,
        }),
        TypeInner::BindingArray { .. } => Ok(TypeLayout {
            size: None,
            alignment: 1,
        }),
        other => Err(format!("unsupported reflected type: {:?}", other)),
    }
}

fn scalar_layout(scalar: Scalar) -> Result<TypeLayout, String> {
    let width = u32::from(scalar.width);
    if !width.is_power_of_two() {
        return Err(format!("unsupported scalar width {}", width));
    }
    Ok(TypeLayout {
        size: Some(width),
        alignment: width,
    })
}

fn vector_layout(size: VectorSize, scalar: Scalar) -> Result<TypeLayout, String> {
    let scalar = scalar_layout(scalar)?;
    let lanes = vector_length(size);
    Ok(TypeLayout {
        size: Some(scalar.size.unwrap() * lanes),
        alignment: scalar.alignment * vector_alignment_factor(size),
    })
}

fn matrix_layout(
    columns: VectorSize,
    rows: VectorSize,
    scalar: Scalar,
) -> Result<TypeLayout, String> {
    let column = vector_layout(rows, scalar)?;
    let column_size = column.size.unwrap();
    let stride = round_up(column_size, column.alignment);
    Ok(TypeLayout {
        size: Some(stride * vector_length(columns)),
        alignment: column.alignment,
    })
}

fn array_layout(
    module: &Module,
    base: Handle<Type>,
    size: &ArraySize,
    stride: u32,
) -> Result<TypeLayout, String> {
    let base_layout = type_layout(module, base)?;
    let expected_stride = round_up(base_layout.size.unwrap_or(0), base_layout.alignment);
    if stride != expected_stride {
        // Log a warning but trust naga's reported stride. A mismatch here
        // usually indicates a naga bug or a version incompatibility.
        eprintln!(
            "warning: array stride mismatch for {:?}: naga reports stride={}, expected={} (element size={}, alignment={})",
            module.types[base].name.as_deref().unwrap_or("<unnamed>"),
            stride,
            expected_stride,
            base_layout.size.unwrap_or(0),
            base_layout.alignment
        );
    }
    let size = match size {
        ArraySize::Constant(length) => Some(stride * length.get()),
        ArraySize::Dynamic => None,
        ArraySize::Pending(_) => None,
    };
    Ok(TypeLayout {
        size,
        alignment: base_layout.alignment,
    })
}

fn vector_length(size: VectorSize) -> u32 {
    match size {
        VectorSize::Bi => 2,
        VectorSize::Tri => 3,
        VectorSize::Quad => 4,
    }
}

fn vector_alignment_factor(size: VectorSize) -> u32 {
    match size {
        VectorSize::Bi => 2,
        VectorSize::Tri | VectorSize::Quad => 4,
    }
}

fn round_up(value: u32, alignment: u32) -> u32 {
    let remainder = value % alignment;
    if remainder == 0 {
        value
    } else {
        value + (alignment - remainder)
    }
}
