//! Dead-code elimination via reachability analysis.
//!
//! Starting from entry points, walk the call graph to find all reachable
//! functions, structs, globals, and constants. Then filter the MIR program
//! to only include reachable declarations.

use std::collections::{HashMap, HashSet};

use crate::*;

/// The set of declarations reachable from entry points.
#[derive(Debug, Default)]
pub struct ReachableSet {
    pub functions: HashSet<String>,
    pub structs: HashSet<String>,
    pub globals: HashSet<String>,
    pub constants: HashSet<String>,
}

/// Compute which declarations are reachable from entry points.
pub fn compute_reachable(program: &MirProgram) -> ReachableSet {
    let mut reachable = ReachableSet::default();

    // Build O(1) lookup indices
    let functions_by_name: HashMap<&str, &MirFunction> =
        program.functions.iter().map(|f| (f.name, f)).collect();
    let constants_by_name: HashMap<&str, &MirConst> =
        program.constants.iter().map(|c| (c.name, c)).collect();
    let globals_by_name: HashMap<&str, &MirGlobal> =
        program.globals.iter().map(|g| (g.name, g)).collect();
    let structs_by_name: HashMap<&str, &MirStruct> =
        program.structs.iter().map(|s| (s.name, s)).collect();

    // Seed: walk all entry points
    for ep in &program.entry_points {
        walk_params(&ep.params, &mut reachable);
        walk_type(&ep.return_ty, &mut reachable);
        walk_stmts(&ep.body, &mut reachable);
        if let Some(expr) = &ep.return_expr {
            walk_expr(expr, &mut reachable);
        }
    }

    // Transitively resolve: functions can call other functions
    let mut worklist: Vec<String> = reachable.functions.iter().cloned().collect();
    let mut visited: HashSet<String> = reachable.functions.clone();

    while let Some(name) = worklist.pop() {
        if let Some(func) = functions_by_name.get(name.as_str()) {
            walk_params(&func.params, &mut reachable);
            walk_type(&func.return_ty, &mut reachable);
            walk_stmts(&func.body, &mut reachable);
            if let Some(expr) = &func.return_expr {
                walk_expr(expr, &mut reachable);
            }
            // Check for newly discovered functions
            for new_fn in reachable
                .functions
                .difference(&visited)
                .cloned()
                .collect::<Vec<_>>()
            {
                visited.insert(new_fn.clone());
                worklist.push(new_fn);
            }
        }
    }

    // Constants can reference other things too
    let reachable_consts: Vec<String> = reachable.constants.iter().cloned().collect();
    for name in reachable_consts {
        if let Some(c) = constants_by_name.get(name.as_str()) {
            walk_type(&c.ty, &mut reachable);
            walk_expr(&c.value, &mut reachable);
        }
    }

    // Transitively resolve struct dependencies
    let struct_names: Vec<String> = reachable.structs.iter().cloned().collect();
    for name in struct_names {
        mark_struct_deps(&name, &structs_by_name, &mut reachable);
    }

    // Also mark structs from globals
    let global_names: Vec<String> = reachable.globals.iter().cloned().collect();
    for name in global_names {
        if let Some(g) = globals_by_name.get(name.as_str()) {
            walk_type(&g.ty, &mut reachable);
        }
    }

    // One more pass on struct deps after globals may have added new structs
    let struct_names: Vec<String> = reachable.structs.iter().cloned().collect();
    for name in struct_names {
        mark_struct_deps(&name, &structs_by_name, &mut reachable);
    }

    reachable
}

/// Filter a MIR program to only include reachable declarations.
pub fn filter_reachable<'a>(program: &MirProgram<'a>, reachable: &ReachableSet) -> MirProgram<'a> {
    MirProgram {
        structs: program
            .structs
            .iter()
            .filter(|s| reachable.structs.contains(s.name) || s.bitfield_fields.is_some())
            .cloned()
            .collect(),
        globals: program
            .globals
            .iter()
            .filter(|g| reachable.globals.contains(g.name))
            .cloned()
            .collect(),
        functions: program
            .functions
            .iter()
            .filter(|f| reachable.functions.contains(f.name))
            .cloned()
            .collect(),
        entry_points: program.entry_points.clone(),
        constants: program
            .constants
            .iter()
            .filter(|c| reachable.constants.contains(c.name))
            .cloned()
            .collect(),
        render_blocks: program.render_blocks.clone(),
    }
}

/// Compute reachable declarations in library mode (no entry points).
///
/// Seeds reachability from ALL functions and ALL constants, keeping all
/// functions and constants but only structs/globals that they actually reference.
pub fn compute_reachable_library(program: &MirProgram) -> ReachableSet {
    let mut reachable = ReachableSet::default();

    // Build O(1) lookup indices
    let globals_by_name: HashMap<&str, &MirGlobal> =
        program.globals.iter().map(|g| (g.name, g)).collect();
    let structs_by_name: HashMap<&str, &MirStruct> =
        program.structs.iter().map(|s| (s.name, s)).collect();

    // Seed: walk all functions
    for func in &program.functions {
        reachable.functions.insert(func.name.to_string());
        walk_params(&func.params, &mut reachable);
        walk_type(&func.return_ty, &mut reachable);
        walk_stmts(&func.body, &mut reachable);
        if let Some(expr) = &func.return_expr {
            walk_expr(expr, &mut reachable);
        }
    }

    // Seed: keep all constants in library mode (they may have been promoted
    // from zero-param functions and are part of the module's public API).
    for c in &program.constants {
        reachable.constants.insert(c.name.to_string());
        walk_type(&c.ty, &mut reachable);
        walk_expr(&c.value, &mut reachable);
    }

    // Transitively resolve struct dependencies
    let struct_names: Vec<String> = reachable.structs.iter().cloned().collect();
    for name in struct_names {
        mark_struct_deps(&name, &structs_by_name, &mut reachable);
    }

    // Also mark structs from globals
    let global_names: Vec<String> = reachable.globals.iter().cloned().collect();
    for name in global_names {
        if let Some(g) = globals_by_name.get(name.as_str()) {
            walk_type(&g.ty, &mut reachable);
        }
    }

    // One more pass on struct deps after globals may have added new structs
    let struct_names: Vec<String> = reachable.structs.iter().cloned().collect();
    for name in struct_names {
        mark_struct_deps(&name, &structs_by_name, &mut reachable);
    }

    reachable
}

/// Filter a MIR program in library mode: keep all functions and constants,
/// but only reachable structs/globals.
pub fn filter_reachable_library<'a>(
    program: &MirProgram<'a>,
    reachable: &ReachableSet,
) -> MirProgram<'a> {
    MirProgram {
        structs: program
            .structs
            .iter()
            .filter(|s| reachable.structs.contains(s.name))
            .cloned()
            .collect(),
        globals: program
            .globals
            .iter()
            .filter(|g| reachable.globals.contains(g.name))
            .cloned()
            .collect(),
        functions: program.functions.clone(), // keep all functions in library mode
        entry_points: program.entry_points.clone(),
        constants: program.constants.clone(), // keep all constants in library mode
        render_blocks: program.render_blocks.clone(),
    }
}

/// Eliminate unreachable declarations from a MIR program.
///
/// If there are no entry points (library mode), all top-level functions are
/// kept but only structs/globals/constants reachable from those functions
/// survive.  This prevents unused prelude ADTs with unresolved type variables
/// from leaking into the output.
pub fn eliminate_dead_code<'a>(program: &MirProgram<'a>) -> MirProgram<'a> {
    if program.entry_points.is_empty() {
        let reachable = compute_reachable_library(program);
        return filter_reachable_library(program, &reachable);
    }
    let reachable = compute_reachable(program);
    filter_reachable(program, &reachable)
}

// ── Walkers ──────────────────────────────────────────────────────────────

fn walk_params(params: &[MirParam], reachable: &mut ReachableSet) {
    for p in params {
        walk_type(&p.ty, reachable);
    }
}

fn walk_type(ty: &MirType, reachable: &mut ReachableSet) {
    match ty {
        MirType::Struct(name) => {
            reachable.structs.insert(name.to_string());
        }
        MirType::Vec(_, inner) | MirType::Array(inner, _) | MirType::RuntimeArray(inner) => {
            walk_type(inner, reachable)
        }
        MirType::Mat(_, _, inner) => walk_type(inner, reachable),
        MirType::Texture2d(inner)
        | MirType::Texture2dMultisampled(inner)
        | MirType::Texture2dArray(inner) => walk_type(inner, reachable),
        MirType::BindingArray(inner, _) => walk_type(inner, reachable),
        _ => {}
    }
}

fn walk_stmts(stmts: &[MirStmt], reachable: &mut ReachableSet) {
    for stmt in stmts {
        walk_stmt(stmt, reachable);
    }
}

fn walk_stmt(stmt: &MirStmt, reachable: &mut ReachableSet) {
    match stmt {
        MirStmt::Let(_, ty, expr) | MirStmt::Var(_, ty, expr) => {
            walk_type(ty, reachable);
            walk_expr(expr, reachable);
        }
        MirStmt::Assign(_, expr) => walk_expr(expr, reachable),
        MirStmt::IndexAssign(base, index, val) => {
            walk_expr(base, reachable);
            walk_expr(index, reachable);
            walk_expr(val, reachable);
        }
        MirStmt::If(cond, then_stmts, else_stmts) => {
            walk_expr(cond, reachable);
            walk_stmts(then_stmts, reachable);
            walk_stmts(else_stmts, reachable);
        }
        MirStmt::Return(expr) => walk_expr(expr, reachable),
        MirStmt::Block(stmts) => walk_stmts(stmts, reachable),
        MirStmt::Switch(expr, cases, default) => {
            walk_expr(expr, reachable);
            for case in cases {
                walk_stmts(&case.body, reachable);
            }
            walk_stmts(default, reachable);
        }
        MirStmt::Loop(stmts) => walk_stmts(stmts, reachable),
        MirStmt::Break | MirStmt::Continue => {}
    }
}

fn walk_expr(expr: &MirExpr, reachable: &mut ReachableSet) {
    match expr {
        MirExpr::Lit(_) => {}
        MirExpr::Var(name, ty) => {
            // Globals are referenced by name via Var
            reachable.globals.insert(name.to_string());
            // Also could be a constant
            reachable.constants.insert(name.to_string());
            walk_type(ty, reachable);
        }
        MirExpr::BinOp(_, lhs, rhs, ty) => {
            walk_expr(lhs, reachable);
            walk_expr(rhs, reachable);
            walk_type(ty, reachable);
        }
        MirExpr::UnaryOp(_, operand, ty) => {
            walk_expr(operand, reachable);
            walk_type(ty, reachable);
        }
        MirExpr::Call(name, args, ty) => {
            reachable.functions.insert(name.to_string());
            for arg in args {
                walk_expr(arg, reachable);
            }
            walk_type(ty, reachable);
        }
        MirExpr::ConstructStruct(name, fields) => {
            reachable.structs.insert(name.to_string());
            for field in fields {
                walk_expr(field, reachable);
            }
        }
        MirExpr::FieldAccess(base, _, ty) => {
            walk_expr(base, reachable);
            walk_type(ty, reachable);
        }
        MirExpr::Index(base, index, ty) => {
            walk_expr(base, reachable);
            walk_expr(index, reachable);
            walk_type(ty, reachable);
        }
        MirExpr::Cast(inner, ty) => {
            walk_expr(inner, reachable);
            walk_type(ty, reachable);
        }
    }
}

/// Transitively mark struct dependencies (structs containing other structs).
fn mark_struct_deps(
    name: &str,
    structs_by_name: &HashMap<&str, &MirStruct>,
    reachable: &mut ReachableSet,
) {
    if let Some(s) = structs_by_name.get(name) {
        for field in &s.fields {
            walk_type_for_struct_deps(&field.ty, structs_by_name, reachable);
        }
    }
}

fn walk_type_for_struct_deps(
    ty: &MirType,
    structs_by_name: &HashMap<&str, &MirStruct>,
    reachable: &mut ReachableSet,
) {
    match ty {
        MirType::Struct(dep) if reachable.structs.insert(dep.to_string()) => {
            mark_struct_deps(dep, structs_by_name, reachable);
        }
        MirType::Vec(_, inner) | MirType::Array(inner, _) | MirType::RuntimeArray(inner) => {
            walk_type_for_struct_deps(inner, structs_by_name, reachable);
        }
        MirType::Mat(_, _, inner) => {
            walk_type_for_struct_deps(inner, structs_by_name, reachable);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shadml_allocator::Allocator;

    fn make_simple_fn<'a>(arena: &'a Allocator, name: &str, calls: &[&str]) -> MirFunction<'a> {
        let body: Vec<MirStmt<'a>> = calls
            .iter()
            .map(|c| {
                MirStmt::Let(
                    arena.alloc_str(&format!("_tmp_{}", c)),
                    MirType::I32,
                    MirExpr::Call(arena.alloc_str(c), vec![], MirType::I32),
                )
            })
            .collect();
        MirFunction {
            name: arena.alloc_str(name),
            params: vec![],
            return_ty: MirType::I32,
            body,
            return_expr: Some(MirExpr::Lit(MirLit::I32(0))),
            comments: vec![],
            is_const: false,
        }
    }

    fn make_entry_point<'a>(arena: &'a Allocator, name: &str, calls: &[&str]) -> MirEntryPoint<'a> {
        let body: Vec<MirStmt<'a>> = calls
            .iter()
            .map(|c| {
                MirStmt::Let(
                    arena.alloc_str(&format!("_tmp_{}", c)),
                    MirType::I32,
                    MirExpr::Call(arena.alloc_str(c), vec![], MirType::I32),
                )
            })
            .collect();
        MirEntryPoint {
            name: arena.alloc_str(name),
            stage: ShaderStage::Compute,
            workgroup_size: Some([64, 1, 1]),
            params: vec![],
            return_ty: MirType::Unit,
            body,
            return_expr: None,
            comments: vec![],
        }
    }

    #[test]
    fn unused_function_is_eliminated() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![
                make_simple_fn(&arena, "used", &[]),
                make_simple_fn(&arena, "unused", &[]),
            ],
            entry_points: vec![make_entry_point(&arena, "main", &["used"])],
            constants: vec![],
            render_blocks: vec![],
        };

        let result = eliminate_dead_code(&program);
        assert_eq!(result.functions.len(), 1);
        assert_eq!(result.functions[0].name, "used");
    }

    #[test]
    fn transitive_call_keeps_both() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![],
            functions: vec![
                make_simple_fn(&arena, "a", &["b"]),
                make_simple_fn(&arena, "b", &[]),
                make_simple_fn(&arena, "c", &[]),
            ],
            entry_points: vec![make_entry_point(&arena, "main", &["a"])],
            constants: vec![],
            render_blocks: vec![],
        };

        let result = eliminate_dead_code(&program);
        let names: HashSet<&str> = result.functions.iter().map(|f| f.name).collect();
        assert!(names.contains("a"));
        assert!(names.contains("b"));
        assert!(!names.contains("c"));
    }

    #[test]
    fn unused_struct_is_eliminated() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![
                MirStruct {
                    name: arena.alloc_str("Used"),
                    fields: vec![MirField {
                        name: arena.alloc_str("x"),
                        ty: MirType::F32,
                        attributes: vec![],
                    }],
                    origin_module: None,
                    adt_variants: None,
                    bitfield_fields: None,
                },
                MirStruct {
                    name: arena.alloc_str("Unused"),
                    fields: vec![MirField {
                        name: arena.alloc_str("y"),
                        ty: MirType::I32,
                        attributes: vec![],
                    }],
                    origin_module: None,
                    adt_variants: None,
                    bitfield_fields: None,
                },
            ],
            globals: vec![],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("main"),
                stage: ShaderStage::Compute,
                workgroup_size: Some([1, 1, 1]),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![MirStmt::Let(
                    arena.alloc_str("s"),
                    MirType::Struct(arena.alloc_str("Used")),
                    MirExpr::ConstructStruct(
                        arena.alloc_str("Used"),
                        vec![MirExpr::Lit(MirLit::F32(1.0))],
                    ),
                )],
                return_expr: None,
                comments: vec![],
            }],
            constants: vec![],
            render_blocks: vec![],
        };

        let result = eliminate_dead_code(&program);
        assert_eq!(result.structs.len(), 1);
        assert_eq!(result.structs[0].name, "Used");
    }

    #[test]
    fn unused_global_is_eliminated() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![],
            globals: vec![
                MirGlobal {
                    name: arena.alloc_str("used_buf"),
                    address_space: AddressSpace::StorageReadWrite,
                    ty: MirType::Array(arena.alloc(MirType::F32), 64),
                    group: 0,
                    binding: 0,
                    origin_module: None,
                },
                MirGlobal {
                    name: arena.alloc_str("unused_buf"),
                    address_space: AddressSpace::Uniform,
                    ty: MirType::F32,
                    group: 0,
                    binding: 1,
                    origin_module: None,
                },
            ],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("main"),
                stage: ShaderStage::Compute,
                workgroup_size: Some([64, 1, 1]),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![MirStmt::IndexAssign(
                    MirExpr::Var(
                        arena.alloc_str("used_buf"),
                        MirType::Array(arena.alloc(MirType::F32), 64),
                    ),
                    MirExpr::Lit(MirLit::I32(0)),
                    MirExpr::Lit(MirLit::F32(1.0)),
                )],
                return_expr: None,
                comments: vec![],
            }],
            constants: vec![],
            render_blocks: vec![],
        };

        let result = eliminate_dead_code(&program);
        assert_eq!(result.globals.len(), 1);
        assert_eq!(result.globals[0].name, "used_buf");
    }

    #[test]
    fn struct_used_via_global_binding_is_kept() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("Particle"),
                fields: vec![MirField {
                    name: arena.alloc_str("pos"),
                    ty: MirType::Vec(3, arena.alloc(MirType::F32)),
                    attributes: vec![],
                }],
                origin_module: None,
                adt_variants: None,
                bitfield_fields: None,
            }],
            globals: vec![MirGlobal {
                name: arena.alloc_str("particles"),
                address_space: AddressSpace::StorageReadWrite,
                ty: MirType::Array(
                    arena.alloc(MirType::Struct(arena.alloc_str("Particle"))),
                    256,
                ),
                group: 0,
                binding: 0,
                origin_module: None,
            }],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("main"),
                stage: ShaderStage::Compute,
                workgroup_size: Some([64, 1, 1]),
                params: vec![],
                return_ty: MirType::Unit,
                body: vec![MirStmt::Let(
                    arena.alloc_str("p"),
                    MirType::Struct(arena.alloc_str("Particle")),
                    MirExpr::Index(
                        arena.alloc(MirExpr::Var(
                            arena.alloc_str("particles"),
                            MirType::Array(
                                arena.alloc(MirType::Struct(arena.alloc_str("Particle"))),
                                256,
                            ),
                        )),
                        arena.alloc(MirExpr::Lit(MirLit::I32(0))),
                        MirType::Struct(arena.alloc_str("Particle")),
                    ),
                )],
                return_expr: None,
                comments: vec![],
            }],
            constants: vec![],
            render_blocks: vec![],
        };

        let result = eliminate_dead_code(&program);
        assert_eq!(result.structs.len(), 1);
        assert_eq!(result.structs[0].name, "Particle");
        assert_eq!(result.globals.len(), 1);
    }

    #[test]
    fn no_entry_points_keeps_functions_eliminates_unused_structs() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![MirStruct {
                name: arena.alloc_str("Foo"),
                fields: vec![],
                origin_module: None,
                adt_variants: None,
                bitfield_fields: None,
            }],
            globals: vec![],
            functions: vec![make_simple_fn(&arena, "helper", &[])],
            entry_points: vec![],
            constants: vec![],
            render_blocks: vec![],
        };

        let result = eliminate_dead_code(&program);
        assert_eq!(result.functions.len(), 1);
        // Foo is not referenced by any function, so it gets eliminated
        assert_eq!(result.structs.len(), 0);
    }

    #[test]
    fn no_entry_points_keeps_used_structs() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![
                MirStruct {
                    name: arena.alloc_str("Used"),
                    fields: vec![MirField {
                        name: arena.alloc_str("x"),
                        ty: MirType::F32,
                        attributes: vec![],
                    }],
                    origin_module: None,
                    adt_variants: None,
                    bitfield_fields: None,
                },
                MirStruct {
                    name: arena.alloc_str("Unused"),
                    fields: vec![],
                    origin_module: None,
                    adt_variants: None,
                    bitfield_fields: None,
                },
            ],
            globals: vec![],
            functions: vec![MirFunction {
                name: arena.alloc_str("helper"),
                params: vec![MirParam {
                    name: arena.alloc_str("s"),
                    ty: MirType::Struct(arena.alloc_str("Used")),
                }],
                return_ty: MirType::Struct(arena.alloc_str("Used")),
                body: vec![],
                return_expr: Some(MirExpr::Var(
                    arena.alloc_str("s"),
                    MirType::Struct(arena.alloc_str("Used")),
                )),
                comments: vec![],
                is_const: false,
            }],
            entry_points: vec![],
            constants: vec![],
            render_blocks: vec![],
        };

        let result = eliminate_dead_code(&program);
        assert_eq!(result.functions.len(), 1);
        assert_eq!(result.structs.len(), 1);
        assert_eq!(result.structs[0].name, "Used");
    }

    #[test]
    fn nested_struct_deps_are_kept() {
        let arena = Allocator::new();
        let program = MirProgram {
            structs: vec![
                MirStruct {
                    name: arena.alloc_str("Inner"),
                    fields: vec![MirField {
                        name: arena.alloc_str("v"),
                        ty: MirType::F32,
                        attributes: vec![],
                    }],
                    origin_module: None,
                    adt_variants: None,
                    bitfield_fields: None,
                },
                MirStruct {
                    name: arena.alloc_str("Outer"),
                    fields: vec![MirField {
                        name: arena.alloc_str("inner"),
                        ty: MirType::Struct(arena.alloc_str("Inner")),
                        attributes: vec![],
                    }],
                    origin_module: None,
                    adt_variants: None,
                    bitfield_fields: None,
                },
                MirStruct {
                    name: arena.alloc_str("Unrelated"),
                    fields: vec![],
                    origin_module: None,
                    adt_variants: None,
                    bitfield_fields: None,
                },
            ],
            globals: vec![],
            functions: vec![],
            entry_points: vec![MirEntryPoint {
                name: arena.alloc_str("main"),
                stage: ShaderStage::Compute,
                workgroup_size: Some([1, 1, 1]),
                params: vec![MirParam {
                    name: arena.alloc_str("o"),
                    ty: MirType::Struct(arena.alloc_str("Outer")),
                }],
                return_ty: MirType::Unit,
                body: vec![],
                return_expr: None,
                comments: vec![],
            }],
            constants: vec![],
            render_blocks: vec![],
        };

        let result = eliminate_dead_code(&program);
        let names: HashSet<&str> = result.structs.iter().map(|s| s.name).collect();
        assert!(names.contains("Inner"));
        assert!(names.contains("Outer"));
        assert!(!names.contains("Unrelated"));
    }
}
