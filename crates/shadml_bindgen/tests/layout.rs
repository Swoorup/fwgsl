use shadml_bindgen::layout::{reflect_profile, FieldLayout};
use shadml_bundler::{CompiledEntry, CompiledProfile};
use shadml_mir::ShaderStage;

fn entry_with_wgsl(name: &str, wgsl: &str) -> CompiledEntry {
    CompiledEntry {
        rust_mod_path: vec![],
        shader_name: name.into(),
        stage: ShaderStage::Compute,
        entry_point: "main".into(),
        wgsl_source: wgsl.into(),
        bind_groups: vec![],
        push_constants: None,
        workgroup_size: None,
        source_files: vec![],
        exported_type_names: vec![],
        render_block: None,
        comments: vec![],
    }
}

fn profile_with_entries(entries: Vec<CompiledEntry>) -> CompiledProfile {
    CompiledProfile {
        profile_key: "test".into(),
        enabled_features: vec![],
        source_files: vec![],
        modules: vec![],
        entries,
        exported_types: vec![],
    }
}

#[test]
fn reflect_scalar_struct() {
    let wgsl = r#"
        struct ScalarStruct {
            a: f32,
            b: i32,
            c: u32,
        }
        @compute @workgroup_size(1)
        fn main() {}
    "#;
    let profile = profile_with_entries(vec![entry_with_wgsl("scalar", wgsl)]);
    let reflected = reflect_profile(&profile).expect("reflection should succeed");
    let layout = reflected
        .structs
        .get("ScalarStruct")
        .expect("ScalarStruct should be reflected");
    assert_eq!(layout.size, 12);
    assert_eq!(layout.alignment, 4);
    assert_eq!(layout.fields.len(), 3);
    assert_eq!(
        layout.fields[0],
        FieldLayout {
            name: "a".into(),
            offset: 0,
            size: 4,
        }
    );
    assert_eq!(
        layout.fields[1],
        FieldLayout {
            name: "b".into(),
            offset: 4,
            size: 4,
        }
    );
    assert_eq!(
        layout.fields[2],
        FieldLayout {
            name: "c".into(),
            offset: 8,
            size: 4,
        }
    );
}

#[test]
fn reflect_vec3_struct() {
    let wgsl = r#"
        struct Vec3Struct {
            v: vec3<f32>,
            x: f32,
        }
        @compute @workgroup_size(1)
        fn main() {}
    "#;
    let profile = profile_with_entries(vec![entry_with_wgsl("vec3", wgsl)]);
    let reflected = reflect_profile(&profile).expect("reflection should succeed");
    let layout = reflected
        .structs
        .get("Vec3Struct")
        .expect("Vec3Struct should be reflected");
    // naga reports vec3<f32> as 12 bytes with 16-byte alignment
    assert_eq!(layout.size, 16);
    assert_eq!(layout.alignment, 16);
    assert_eq!(layout.fields[0].offset, 0);
    assert_eq!(layout.fields[1].offset, 12);
}

#[test]
fn reflect_array_struct() {
    let wgsl = r#"
        struct ArrayStruct {
            data: array<f32, 4>,
        }
        @compute @workgroup_size(1)
        fn main() {}
    "#;
    let profile = profile_with_entries(vec![entry_with_wgsl("array", wgsl)]);
    let reflected = reflect_profile(&profile).expect("reflection should succeed");
    let layout = reflected
        .structs
        .get("ArrayStruct")
        .expect("ArrayStruct should be reflected");
    // array<f32, 4> = 16 bytes, alignment 4
    assert_eq!(layout.size, 16);
    assert_eq!(layout.alignment, 4);
}

#[test]
fn reflect_mat4x4_struct() {
    let wgsl = r#"
        struct MatStruct {
            m: mat4x4<f32>,
        }
        @compute @workgroup_size(1)
        fn main() {}
    "#;
    let profile = profile_with_entries(vec![entry_with_wgsl("mat", wgsl)]);
    let reflected = reflect_profile(&profile).expect("reflection should succeed");
    let layout = reflected
        .structs
        .get("MatStruct")
        .expect("MatStruct should be reflected");
    // mat4x4<f32> = 64 bytes (4 columns of vec4<f32>), alignment 16
    assert_eq!(layout.size, 64);
    assert_eq!(layout.alignment, 16);
}

#[test]
fn reflect_nested_struct() {
    let wgsl = r#"
        struct Inner {
            x: f32,
            y: f32,
        }
        struct Outer {
            inner: Inner,
            z: u32,
        }
        @compute @workgroup_size(1)
        fn main() {}
    "#;
    let profile = profile_with_entries(vec![entry_with_wgsl("nested", wgsl)]);
    let reflected = reflect_profile(&profile).expect("reflection should succeed");

    let inner = reflected
        .structs
        .get("Inner")
        .expect("Inner should be reflected");
    assert_eq!(inner.size, 8);
    assert_eq!(inner.alignment, 4);

    let outer = reflected
        .structs
        .get("Outer")
        .expect("Outer should be reflected");
    // Inner is 8 bytes, alignment 4. z: u32 is 4 bytes. Total = 12.
    assert_eq!(outer.size, 12);
    assert_eq!(outer.alignment, 4);
}
