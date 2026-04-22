
    use super::*;
    use std::io::Write;

    const RENDER_BLOCK_SOURCE: &str = r#"
data Globals = Globals {
  time : F32,
}

data VertexInput = VertexInput {
  @builtin(vertex_index) vertex_index : U32,
}

data VertexOutput = VertexOutput {
  @builtin(position) position : Vec<4, F32>,
  @location(0) color : Vec<4, F32>,
}

render test_render
  @group(0) @binding(0) uniform globals : Globals

  vsMain : VertexInput -> VertexOutput
  @vertex
  vsMain input =
    let pos = vec4 0.0 0.0 0.0 1.0
        col = vec4 1.0 0.0 0.0 1.0
    in VertexOutput { position = pos, color = col }

  fsMain : VertexOutput -> Vec<4, F32>
  @fragment
  fsMain input = input.color
"#;

    #[test]
    fn render_block_pipeline_compiles_to_valid_wgsl() {
        let mut parser = Parser::new(RENDER_BLOCK_SOURCE);
        let mut program = parser.parse_program();

        assert!(
            !parser.diagnostics().has_errors(),
            "parse errors: {:?}",
            parser.diagnostics().iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        with_prelude(&mut program);

        let mut sa = SemanticAnalyzer::new();
        sa.analyze(&program);

        assert!(
            !sa.has_errors(),
            "semantic errors: {:?}",
            sa.diagnostics().iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        let mut lowering = AstLowering::new(&sa);
        let hir = lowering.lower_program(&program);

        assert!(
            !lowering.has_errors(),
            "lowering errors: {:?}",
            lowering.diagnostics().iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        let arena = shadml_allocator::Allocator::new();
        let mir = shadml_mir::lower::lower_hir_to_mir(&arena, &hir).expect("MIR lowering should succeed");

        // Assert MIR contains the expected render block
        assert_eq!(mir.render_blocks.len(), 1, "expected exactly one render block");
        let rb = &mir.render_blocks[0];
        assert_eq!(rb.name, "test_render");
        assert_eq!(rb.vertex_entry, "vsMain");
        assert_eq!(rb.fragment_entry, "fsMain");
        assert_eq!(rb.binding_names.len(), 1);
        assert_eq!(rb.binding_names[0], "globals");

        // Assert MIR validation passes
        let validation_result = shadml_mir::validate::validate_program(&mir);
        assert!(
            validation_result.is_ok(),
            "MIR validation should pass, got: {:?}",
            validation_result.unwrap_err()
        );

        // Emit WGSL and assert it contains both entry points
        let wgsl = emit_wgsl(&mir);
        assert!(wgsl.contains("@vertex"), "WGSL should contain @vertex, got:\n{}", wgsl);
        assert!(wgsl.contains("@fragment"), "WGSL should contain @fragment, got:\n{}", wgsl);
        assert!(wgsl.contains("fn vsMain("), "WGSL should contain vertex entry, got:\n{}", wgsl);
        assert!(wgsl.contains("fn fsMain("), "WGSL should contain fragment entry, got:\n{}", wgsl);
        assert!(
            wgsl.contains("@group(0) @binding(0)"),
            "WGSL should contain binding from render block, got:\n{}",
            wgsl
        );
    }

    #[test]
    fn render_block_bundler_produces_entries_with_render_block() {
        let tmp_dir = std::env::temp_dir().join("shadml_render_block_test");
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("should create temp dir");

        let shader_path = tmp_dir.join("TestRender.shadml");
        let mut file = std::fs::File::create(&shader_path).expect("should create shader file");
        file.write_all(RENDER_BLOCK_SOURCE.as_bytes())
            .expect("should write shader source");
        drop(file);

        let config = shadml_bundler::BundleConfig {
            entries: vec![shader_path.clone()],
            source_roots: vec![tmp_dir.clone()],
            output_dir: tmp_dir.join("dist"),
            features: vec![],
            preserve_comments: false,
            split_entry_points: true,
        };

        let manifest = shadml_bundler::bundle_manifest(&config, "test_profile"
        ).expect("bundle_manifest should succeed");

        assert_eq!(manifest.profiles.len(), 1);
        let profile = &manifest.profiles[0];
        assert_eq!(profile.entries.len(), 2, "expected two split entries (vertex + fragment)");

        let vertex_entry = profile
            .entries
            .iter()
            .find(|e| e.stage == shadml_mir::ShaderStage::Vertex)
            .expect("should have vertex entry");
        let fragment_entry = profile
            .entries
            .iter()
            .find(|e| e.stage == shadml_mir::ShaderStage::Fragment)
            .expect("should have fragment entry");

        assert_eq!(
            vertex_entry.render_block,
            Some("test_render".to_string()),
            "vertex entry should belong to render block"
        );
        assert_eq!(
            fragment_entry.render_block,
            Some("test_render".to_string()),
            "fragment entry should belong to render block"
        );

        // Each entry should have the binding from the render block
        assert!(
            vertex_entry.bind_groups.iter().any(|bg| {
                bg.bindings.iter().any(|b| b.name == "globals")
            }),
            "vertex entry should have globals binding"
        );
        assert!(
            fragment_entry.bind_groups.iter().any(|bg| {
                bg.bindings.iter().any(|b| b.name == "globals")
            }),
            "fragment entry should have globals binding"
        );

        // Clean up temp directory
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn render_block_bindgen_generates_pipeline_layout_function() {
        let tmp_dir = std::env::temp_dir().join("shadml_render_block_bindgen_test");
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("should create temp dir");

        let shader_path = tmp_dir.join("TestRender.shadml");
        let mut file = std::fs::File::create(&shader_path).expect("should create shader file");
        file.write_all(RENDER_BLOCK_SOURCE.as_bytes())
            .expect("should write shader source");
        drop(file);

        let config_path = tmp_dir.join("shadml.toml");
        let mut config_file = std::fs::File::create(&config_path).expect("should create config file");
        config_file
            .write_all(
                br#"[bundle]
source_roots = ["."]
split_entry_points = true

[[entry]]
file = "TestRender.shadml"

[rust]
output = "generated.rs"
source_mode = "EmbeddedDebug"
"#,
            )
            .expect("should write config file");
        drop(config_file);

        let output_path = tmp_dir.join("generated.rs");

        let bindgen = shadml_bindgen::ShadmlBindgenBuilder::default()
            .project_root(&tmp_dir)
            .config(&config_path)
            .output(&output_path)
            .source_mode(shadml_bindgen::SourceMode::EmbeddedDebug)
            .build()
            .expect("bindgen build should succeed");

        bindgen.generate().expect("bindgen generate should succeed");

        let rust_source = std::fs::read_to_string(&output_path)
            .expect("should read generated rust source");

        assert!(
            rust_source.contains("create_test_render_render_pipeline_layout"),
            "bindgen should generate render block pipeline layout function, got:\n{}",
            rust_source
        );
        assert!(
            rust_source.contains("group0::create_bind_group_layout"),
            "bindgen should deduplicate bind groups at module level, got:\n{}",
            rust_source
        );

        // Clean up temp directory
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
