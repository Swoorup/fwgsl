use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use shadml_mir::{AddressSpace, MirField, MirGlobal, MirStruct, MirType, ShaderStage};

// ---------------------------------------------------------------------------
// WGSL layout rules (alignment + size)
// ---------------------------------------------------------------------------

/// Computed layout (size and alignment) for a WGSL type.
struct TypeLayout {
    size: u32,
    alignment: u32,
}

/// Round `value` up to the next multiple of `alignment`.
fn round_up(value: u32, alignment: u32) -> u32 {
    (value + alignment - 1) / alignment * alignment
}

/// Compute the WGSL layout (size + alignment) of a MIR type.
///
/// Implements the alignment and size rules from the WGSL spec so that
/// struct-typed push constants get accurate sizes without needing naga
/// reflection.
fn mir_type_layout<'a>(ty: &MirType<'a>, structs: &HashMap<&str, &MirStruct<'a>>) -> TypeLayout {
    match ty {
        MirType::I32 | MirType::U32 | MirType::F32 | MirType::Bool => TypeLayout {
            size: 4,
            alignment: 4,
        },
        MirType::Vec(n, _) => match *n {
            2 => TypeLayout { size: 8, alignment: 8 },
            3 => TypeLayout { size: 12, alignment: 16 },
            4 => TypeLayout { size: 16, alignment: 16 },
            _ => TypeLayout { size: *n as u32 * 4, alignment: 16 },
        },
        MirType::Mat(cols, rows, _) => {
            let col_layout = mir_type_layout(&MirType::Vec(*rows, &MirType::F32), structs);
            let col_stride = round_up(col_layout.size, col_layout.alignment);
            TypeLayout {
                size: col_stride * *cols as u32,
                alignment: col_layout.alignment,
            }
        }
        MirType::Struct(name) => {
            let s = match structs.get(name) {
                Some(s) => s,
                None => return TypeLayout { size: 0, alignment: 1 },
            };
            struct_layout(s, structs)
        }
        MirType::Array(inner, len) => {
            let elem = mir_type_layout(inner, structs);
            let stride = round_up(elem.size, elem.alignment);
            TypeLayout {
                size: stride * len,
                alignment: elem.alignment,
            }
        }
        MirType::RuntimeArray(_)
        | MirType::Unit
        | MirType::Texture2d(_)
        | MirType::Texture2dMultisampled(_)
        | MirType::Texture2dArray(_)
        | MirType::Sampler
        | MirType::SamplerComparison
        | MirType::BindingArray(..) => TypeLayout { size: 0, alignment: 1 },
    }
}

/// Compute the WGSL layout of a struct by iterating fields with alignment-based offsets.
fn struct_layout<'a>(
    s: &MirStruct<'a>,
    structs: &HashMap<&str, &MirStruct<'a>>,
) -> TypeLayout {
    let mut offset: u32 = 0;
    let mut struct_align: u32 = 1;
    for field in &s.fields {
        let field_layout = mir_type_layout(&field.ty, structs);
        struct_align = struct_align.max(field_layout.alignment);
        offset = round_up(offset, field_layout.alignment);
        offset += field_layout.size;
    }
    let size = if s.fields.is_empty() { 0 } else { round_up(offset, struct_align) };
    TypeLayout { size, alignment: struct_align }
}

/// Compiler artifact consumed by the Rust bindgen layer.
#[derive(Debug, Clone, Default)]
pub struct ShaderBundleManifest {
    pub profiles: Vec<CompiledProfile>,
}

/// One compiled feature/profile variant.
#[derive(Debug, Clone)]
pub struct CompiledProfile {
    pub profile_key: String,
    pub enabled_features: Vec<String>,
    pub source_files: Vec<PathBuf>,
    pub modules: Vec<CompiledModule>,
    pub entries: Vec<CompiledEntry>,
    pub exported_types: Vec<ExportedType>,
}

/// Logical module metadata captured during bundling.
#[derive(Debug, Clone)]
pub struct CompiledModule {
    pub name: String,
    pub path: PathBuf,
    pub dependencies: Vec<String>,
}

/// One compiled shader entry emitted by the bundler.
#[derive(Debug, Clone)]
pub struct CompiledEntry {
    pub rust_mod_path: Vec<String>,
    pub shader_name: String,
    pub stage: ShaderStage,
    pub entry_point: String,
    pub wgsl_source: String,
    pub bind_groups: Vec<BindGroupInfo>,
    pub push_constants: Option<PushConstantInfo>,
    pub workgroup_size: Option<[u32; 3]>,
    pub source_files: Vec<PathBuf>,
    pub exported_type_names: Vec<String>,
}

/// Bind group metadata for one entry.
#[derive(Debug, Clone)]
pub struct BindGroupInfo {
    pub group: u32,
    pub bindings: Vec<BindingInfo>,
}

/// Binding metadata derived from MIR globals.
#[derive(Debug, Clone)]
pub struct BindingInfo {
    pub name: String,
    pub binding: u32,
    pub address_space: BindingAddressSpace,
    pub ty: ManifestType,
}

/// Bind group address space exposed to bindgen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingAddressSpace {
    Uniform,
    StorageRead,
    StorageReadWrite,
    /// `var<immediate>` — push constants (no @group/@binding)
    Immediate,
    /// Opaque resource (texture/sampler) — no address space keyword
    Opaque,
}

/// Push constant metadata.
#[derive(Debug, Clone)]
pub struct PushConstantInfo {
    pub name: String,
    pub ty: ManifestType,
    pub size: u32,
}

/// Extract push constant info from MIR globals with `Immediate` address space.
/// Returns `Some(PushConstantInfo)` if any immediate bindings exist, `None` otherwise.
pub(crate) fn push_constants_from_globals<'a>(
    globals: &[MirGlobal<'a>],
    structs: &[MirStruct<'a>],
) -> Option<PushConstantInfo> {
    let structs_map: HashMap<&str, &MirStruct<'a>> =
        structs.iter().map(|s| (s.name, s)).collect();
    let mut name = String::new();
    let mut ty = ManifestType::Unit;
    let mut total_size: u32 = 0;
    let mut found = false;
    for global in globals {
        if global.address_space == AddressSpace::Immediate {
            found = true;
            name = global.name.to_string();
            ty = manifest_type_from_mir(&global.ty);
            total_size += mir_type_size(&global.ty, &structs_map);
        }
    }
    if found {
        Some(PushConstantInfo {
            name,
            ty,
            size: total_size,
        })
    } else {
        None
    }
}

/// Compute the byte size of a MIR type using WGSL alignment rules.
fn mir_type_size<'a>(ty: &MirType<'a>, structs: &HashMap<&str, &MirStruct<'a>>) -> u32 {
    mir_type_layout(ty, structs).size
}

/// A named type that survived into an entry's shader-facing surface.
#[derive(Debug, Clone)]
pub struct ExportedType {
    pub name: String,
    pub fields: Vec<ExportedField>,
}

/// A field in a generated type.
#[derive(Debug, Clone)]
pub struct ExportedField {
    pub name: String,
    pub ty: ManifestType,
}

/// Lifetime-free type information suitable for downstream code generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestType {
    I32,
    U32,
    F32,
    Bool,
    Vec(u8, Box<ManifestType>),
    Mat(u8, u8, Box<ManifestType>),
    Struct(String),
    Array(Box<ManifestType>, u32),
    RuntimeArray(Box<ManifestType>),
    Texture2d(Box<ManifestType>),
    Texture2dMultisampled(Box<ManifestType>),
    Texture2dArray(Box<ManifestType>),
    Sampler,
    SamplerComparison,
    BindingArray(Box<ManifestType>, u32),
    Unit,
}

pub(crate) fn bind_groups_from_globals(globals: &[MirGlobal<'_>]) -> Vec<BindGroupInfo> {
    let mut groups: BTreeMap<u32, Vec<BindingInfo>> = BTreeMap::new();

    for global in globals {
        // Immediate bindings go into push_constants, not bind groups
        if global.address_space == AddressSpace::Immediate {
            continue;
        }
        groups.entry(global.group).or_default().push(BindingInfo {
            name: global.name.to_string(),
            binding: global.binding,
            address_space: convert_address_space(global.address_space),
            ty: manifest_type_from_mir(&global.ty),
        });
    }

    groups
        .into_iter()
        .map(|(group, mut bindings)| {
            bindings.sort_by_key(|binding| binding.binding);
            BindGroupInfo { group, bindings }
        })
        .collect()
}

pub(crate) fn exported_types_from_structs(structs: &[MirStruct<'_>]) -> Vec<ExportedType> {
    let mut exported = structs
        .iter()
        .map(|structure| ExportedType {
            name: structure.name.to_string(),
            fields: structure
                .fields
                .iter()
                .map(exported_field_from_mir)
                .collect(),
        })
        .collect::<Vec<_>>();
    exported.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));
    exported
}

pub(crate) fn exported_type_names(exported: &[ExportedType]) -> Vec<String> {
    exported.iter().map(|ty| ty.name.clone()).collect()
}

fn exported_field_from_mir(field: &MirField<'_>) -> ExportedField {
    ExportedField {
        name: field.name.to_string(),
        ty: manifest_type_from_mir(&field.ty),
    }
}

fn convert_address_space(address_space: AddressSpace) -> BindingAddressSpace {
    match address_space {
        AddressSpace::Uniform => BindingAddressSpace::Uniform,
        AddressSpace::StorageRead => BindingAddressSpace::StorageRead,
        AddressSpace::StorageReadWrite => BindingAddressSpace::StorageReadWrite,
        AddressSpace::Immediate => BindingAddressSpace::Immediate,
        AddressSpace::Opaque => BindingAddressSpace::Opaque,
    }
}

pub(crate) fn manifest_type_from_mir(ty: &MirType<'_>) -> ManifestType {
    match ty {
        MirType::I32 => ManifestType::I32,
        MirType::U32 => ManifestType::U32,
        MirType::F32 => ManifestType::F32,
        MirType::Bool => ManifestType::Bool,
        MirType::Vec(size, inner) => {
            ManifestType::Vec(*size, Box::new(manifest_type_from_mir(inner)))
        }
        MirType::Mat(columns, rows, inner) => {
            ManifestType::Mat(*columns, *rows, Box::new(manifest_type_from_mir(inner)))
        }
        MirType::Struct(name) => ManifestType::Struct((*name).to_string()),
        MirType::Array(inner, len) => {
            ManifestType::Array(Box::new(manifest_type_from_mir(inner)), *len)
        }
        MirType::RuntimeArray(inner) => {
            ManifestType::RuntimeArray(Box::new(manifest_type_from_mir(inner)))
        }
        MirType::Texture2d(inner) => {
            ManifestType::Texture2d(Box::new(manifest_type_from_mir(inner)))
        }
        MirType::Texture2dMultisampled(inner) => {
            ManifestType::Texture2dMultisampled(Box::new(manifest_type_from_mir(inner)))
        }
        MirType::Texture2dArray(inner) => {
            ManifestType::Texture2dArray(Box::new(manifest_type_from_mir(inner)))
        }
        MirType::Sampler => ManifestType::Sampler,
        MirType::SamplerComparison => ManifestType::SamplerComparison,
        MirType::BindingArray(inner, count) => {
            ManifestType::BindingArray(Box::new(manifest_type_from_mir(inner)), *count)
        }
        MirType::Unit => ManifestType::Unit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_structs<'a>() -> HashMap<&'a str, &'a MirStruct<'a>> {
        HashMap::new()
    }

    fn make_structs<'a>(structs: &'a [MirStruct<'a>]) -> HashMap<&'a str, &'a MirStruct<'a>> {
        structs.iter().map(|s| (s.name, s)).collect()
    }

    #[test]
    fn scalar_layout() {
        let structs = empty_structs();
        for ty in [MirType::I32, MirType::U32, MirType::F32, MirType::Bool] {
            let layout = mir_type_layout(&ty, &structs);
            assert_eq!(layout.size, 4, "size for {ty:?}");
            assert_eq!(layout.alignment, 4, "alignment for {ty:?}");
        }
    }

    #[test]
    fn vec3_layout() {
        let structs = empty_structs();
        let layout = mir_type_layout(&MirType::Vec(3, &MirType::F32), &structs);
        assert_eq!(layout.size, 12);
        assert_eq!(layout.alignment, 16);
    }

    #[test]
    fn vec2_layout() {
        let structs = empty_structs();
        let layout = mir_type_layout(&MirType::Vec(2, &MirType::F32), &structs);
        assert_eq!(layout.size, 8);
        assert_eq!(layout.alignment, 8);
    }

    #[test]
    fn vec4_layout() {
        let structs = empty_structs();
        let layout = mir_type_layout(&MirType::Vec(4, &MirType::F32), &structs);
        assert_eq!(layout.size, 16);
        assert_eq!(layout.alignment, 16);
    }

    #[test]
    fn mat3x3_layout() {
        // mat3x3<f32>: column stride = round_up(12, 16) = 16, size = 16 * 3 = 48
        let structs = empty_structs();
        let layout = mir_type_layout(&MirType::Mat(3, 3, &MirType::F32), &structs);
        assert_eq!(layout.size, 48);
        assert_eq!(layout.alignment, 16);
    }

    #[test]
    fn mat4x4_layout() {
        // mat4x4<f32>: column stride = round_up(16, 16) = 16, size = 16 * 4 = 64
        let structs = empty_structs();
        let layout = mir_type_layout(&MirType::Mat(4, 4, &MirType::F32), &structs);
        assert_eq!(layout.size, 64);
        assert_eq!(layout.alignment, 16);
    }

    #[test]
    fn mat2x3_layout() {
        // mat2x3<f32>: column stride = round_up(12, 16) = 16, size = 16 * 2 = 32
        let structs = empty_structs();
        let layout = mir_type_layout(&MirType::Mat(2, 3, &MirType::F32), &structs);
        assert_eq!(layout.size, 32);
        assert_eq!(layout.alignment, 16);
    }

    #[test]
    fn struct_with_vec3_field() {
        // struct S { offset: f32, dir: vec3<f32> }
        // offset at 0 (size 4, align 4), dir at 16 (size 12, align 16)
        // total = round_up(16 + 12, 16) = 32
        let s = MirStruct {
            name: "S",
            fields: vec![
                MirField { name: "offset", ty: MirType::F32, attributes: vec![] },
                MirField { name: "dir", ty: MirType::Vec(3, &MirType::F32), attributes: vec![] },
            ],
        };
        let arr = [s];
        let structs = make_structs(&arr);
        let layout = mir_type_layout(&MirType::Struct("S"), &structs);
        assert_eq!(layout.size, 32);
        assert_eq!(layout.alignment, 16);
    }

    #[test]
    fn struct_with_mat3x3() {
        // struct M { transform: mat3x3<f32> }
        // transform at 0 (size 48, align 16)
        // total = round_up(48, 16) = 48
        let s = MirStruct {
            name: "M",
            fields: vec![
                MirField { name: "transform", ty: MirType::Mat(3, 3, &MirType::F32), attributes: vec![] },
            ],
        };
        let arr = [s];
        let structs = make_structs(&arr);
        let layout = mir_type_layout(&MirType::Struct("M"), &structs);
        assert_eq!(layout.size, 48);
        assert_eq!(layout.alignment, 16);
    }

    #[test]
    fn array_of_scalars() {
        // array<f32, 4>: stride = round_up(4, 4) = 4, size = 4 * 4 = 16
        let structs = empty_structs();
        let layout = mir_type_layout(&MirType::Array(&MirType::F32, 4), &structs);
        assert_eq!(layout.size, 16);
        assert_eq!(layout.alignment, 4);
    }

    #[test]
    fn array_of_vec3() {
        // array<vec3<f32>, 3>: stride = round_up(12, 16) = 16, size = 16 * 3 = 48
        let structs = empty_structs();
        let layout = mir_type_layout(&MirType::Array(&MirType::Vec(3, &MirType::F32), 3), &structs);
        assert_eq!(layout.size, 48);
        assert_eq!(layout.alignment, 16);
    }

    #[test]
    fn unknown_struct_returns_zero() {
        let structs = empty_structs();
        let layout = mir_type_layout(&MirType::Struct("NonExistent"), &structs);
        assert_eq!(layout.size, 0);
        assert_eq!(layout.alignment, 1);
    }

    #[test]
    fn push_constants_from_globals_with_struct() {
        let s = MirStruct {
            name: "Params",
            fields: vec![
                MirField { name: "offset", ty: MirType::F32, attributes: vec![] },
                MirField { name: "dir", ty: MirType::Vec(3, &MirType::F32), attributes: vec![] },
            ],
        };
        let globals = vec![MirGlobal {
            name: "imm",
            address_space: AddressSpace::Immediate,
            ty: MirType::Struct("Params"),
            group: 0,
            binding: 0,
        }];
        let result = push_constants_from_globals(&globals, &[s]);
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.name, "imm");
        assert_eq!(info.size, 32); // 4 + 12 bytes padding + 12 for vec3 = 32
    }

    #[test]
    fn push_constants_no_immediate() {
        let globals: Vec<MirGlobal> = vec![MirGlobal {
            name: "buf",
            address_space: AddressSpace::Uniform,
            ty: MirType::F32,
            group: 0,
            binding: 0,
        }];
        let result = push_constants_from_globals(&globals, &[]);
        assert!(result.is_none());
    }
}
