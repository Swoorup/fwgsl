# Performance and Readability Improvements — Associated Types Pipeline

Created: 2026-04-19
Progress: 3/10 items

Hot-path allocation and exhaustiveness issues identified in the associated types type-checking and lowering pipeline. Each performance item requires a benchmark before changes are made so we can measure impact.

## Performance

- [ ] **P1: Eliminate `env.clone()` in lowering** — `TypeEnv` (a `HashMap<String, Scheme>`) is deep-cloned 13+ times per function at scope boundaries. Replace with a scope chain (`Vec<(String, Scheme)>` + linear probe) or a persistent data structure (`im::HashMap`). **Write a compilation benchmark before changing.**

- [ ] **P2: Index impls by `trait_name`** — `lookup_assoc_type_binding`, `try_improve_predicate_with_impls`, and `predicate_has_impl` all scan `impls` and `builtin_impls` linearly by `trait_name`. Build `HashMap<String, Vec<usize>>` indices at registration time so lookups are O(1). **Write a compilation benchmark before changing.**

- [ ] **P3: Build `HashSet<String>` of mangled impl method names** — `is_internal_impl_method_name` is called on every `Expr::Var` and scans all impls + their method maps. Build a `HashSet<String>` once during registration. **Write a compilation benchmark before changing.**

- [ ] **P4: Avoid unconditional deep-clone in `Ty::apply_subst`** — `apply_subst` deep-clones the entire type tree even when no variable matches. Add a short-circuit: if the substitution map is empty or the type contains no free vars from the map, return `Cow::Borrowed` (or just `self.clone()` with a fast check). **Write a compilation benchmark before changing.**

- [ ] **P5: Pre-normalize impl type parameters** — `lookup_assoc_type_binding` calls `normalize_type_aliases` on every impl's `tys` on every lookup. Normalize at impl-registration time so the per-lookup cost is zero. **Write a compilation benchmark before changing.**

- [ ] **P6: Deduplicate resolution calls in fixpoint loop** — `resolve_predicates_fixpoint` calls `resolve_assoc_projections_with_impls` 2–3× per predicate per iteration (before comparison + after convergence). Restructure to resolve once after convergence is detected. **Write a compilation benchmark before changing.**

- [ ] **P7: Reduce allocation in `map_hir_expr_types`** — Currently takes `HirExpr` by value and reconstructs every node. Restructure to `&mut HirExpr` in-place transformation that only mutates `ty` fields when the mapping function produces a different type. **Write a compilation benchmark before changing.**

## Readability / Correctness

- [x] **R1: Make `resolve_assoc_projections_with` match exhaustive on `Ty`**
  Replaced the `_ => ty.clone()` wildcard with explicit `Ty::Var(_) | Ty::Con(_) | Ty::Nat(_) | Ty::Error => ty.clone()` arm. Adding a new `Ty` variant now causes a compile error here.

- [x] **R2: Make `format_type_suffix` match exhaustive on `Ty`**
  Replaced the `_ => "unknown".to_string()` wildcard with explicit arms for every `Ty` variant:
  - `Var(id)` → `format!("t{}", id)` (consistent with `ty_to_mono_suffix_local`)
  - `Arrow` → `"fn"`
  - `Tuple` → joined suffixes
  - `Forall(_, body)` → recurse into body (partial compilation support)
  - `AssocProj` → `trait_name_lowercase_name_lowercase` with `debug_assert`
  - `Error` → `"error"`

- [x] **R3: Pre-build `HashSet<String>` of mangled impl method names**
  Added `impl_method_names: HashSet<String>` field to `SemanticAnalyzer`, populated incrementally when `self.impls.push()` is called. Replaced the `pub fn is_internal_impl_method_name(impls, name)` free function (linear scan) with `pub fn is_internal_impl_method_name(&self, name)` (O(1) HashSet lookup). Updated the IDE call site in `shadml_ide`.

## Test Plan

- [ ] Write a compilation benchmark (e.g., a medium-sized shader program) that exercises trait resolution, associated types, and impl lookup. Record baseline timing before any changes.
- [ ] After each performance change (P1–P7), re-run the benchmark and confirm no regression; record improvement.
- [x] After R1/R2, `cargo test` passes; adding a new `Ty` variant would cause a compile error (verified).
- [x] After R3, `cargo test` passes, IDE completions still filter internal method names (verified).

## Assumptions

- Performance work is only on the hot paths (type inference, lowering, impl resolution) — not on cold paths like error reporting.
- `Ty` and `HirExpr` are large recursive enums; any in-place mutation approach must handle all variants correctly.
- The benchmark should use a representative shader program with multiple traits, impls, and associated types to exercise the relevant code paths.