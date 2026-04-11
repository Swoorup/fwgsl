use std::collections::BTreeMap;
use std::path::PathBuf;

use shadml_mir::{AddressSpace, MirField, MirGlobal, MirStruct, MirType, ShaderStage};

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
}

/// Push constant metadata placeholder for future expansion.
#[derive(Debug, Clone)]
pub struct PushConstantInfo {
    pub size: u32,
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
    Unit,
}

pub(crate) fn bind_groups_from_globals(globals: &[MirGlobal<'_>]) -> Vec<BindGroupInfo> {
    let mut groups: BTreeMap<u32, Vec<BindingInfo>> = BTreeMap::new();

    for global in globals {
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
        MirType::Unit => ManifestType::Unit,
    }
}
