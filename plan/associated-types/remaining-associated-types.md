# Remaining Issues — Associated Types Implementation

Created: 2026-04-17
Progress: 17/23 issues

Code review (2026-04-17, updated 2026-04-18).

---

## 1. Permissive `AssocProj` Unification — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- Extended the permissive rule from "only when trait_params are concrete" to "any AssocProj" (the concrete-param guard was too strict — free type variables from e.g. matrix indexing caused false type errors)
- Fixed `check_function` and `check_impl_method` in semantic to resolve inferred predicates BEFORE unifying body type with declared type
- Fixed same ordering in AST lowering for `lower_function` and entry point processing

**Remaining concern:** There is no post-resolution verification that the resolved `AssocProj` type is actually compatible with the type it was unified against. If `AssocProj { [F32, F32], "Output", "Add" } ~ Bool` is accepted, and the resolution produces `F32`, the `F32 ≠ Bool` mismatch is never reported. In practice, predicate resolution catches the common cases (missing impls → "missing trait constraint" error), but a truly incompatible resolution would be silently accepted.

---

## 2. Empty-String `trait_name` Sentinel Creates Unresolvable `AssocProj` — FIXED

**Status:** Fixed (2026-04-19)

**What changed:**
- Both semantic and lowering crates now emit a diagnostic error + return `Ty::Error` instead of creating `AssocProj` with empty `trait_name`
- Added `debug_assert!(!trait_name.is_empty())` in `Display` for `AssocProj` (typechecker) and `resolve_assoc_projections_with` (semantic)
- The UI test `self-unknown-assoc` now correctly reports the error

---

## 3. Incomplete Mangling for `AssocProj` Risks Symbol Collisions — FIXED

**Status:** Fixed (2026-04-19)

**What changed:**
- `ty_to_mono_suffix_local` for `AssocProj` now includes `trait_name`: `format!("{}_{}", trait_name.to_lowercase(), name.to_lowercase())`
- Added `debug_assert!(!trait_name.is_empty())` in the same arm
- Added documentation comment on `mono_mangled_function_name` noting that unresolved `AssocProj` indicates an earlier pipeline error

---

## 4. Duplicated Eager Resolution Block in `Expr::Infix` Lowering — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- Extracted `eagerly_resolve_assoc_proj_in_ret(&mut self, ret_ty: &Ty, span: Span)` method on `LoweringContext`
- Both the `BinOp` branch and the desugared function-application branch now call this shared method

---

## 5. Magic-String `"type"` Keyword Check in Parser — FIXED

**Status:** Fixed (2026-04-19)

**What changed:**
- Added `SyntaxKind::KwType` to the keyword enum (between `KwCfg` and `KwSelf`, preserving `is_keyword()` range)
- Added `"type" => Some(SyntaxKind::KwType)` to `keyword_from_str`
- Added `SyntaxKind::KwType => "'type'"` to `Display` impl
- Replaced all 4 magic-string checks in the parser with `SyntaxKind::KwType` pattern matching
- `parse_builtin_type_decl` now uses `self.expect(SyntaxKind::KwType)` instead of `self.expect(SyntaxKind::Ident)` + runtime string validation
- Added test assertions for `is_keyword()` and `keyword_from_str("type")`

---

## 6. Triplicated Associated Type Parsing Logic — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- Extracted `try_parse_assoc_type_def` for `type UpperIdent = Type` (used by `parse_builtin_impl_decl` and `parse_impl_decl`)
- Extracted `try_parse_assoc_type_decl` for `type UpperIdent` (used by `parse_trait_decl`)
- Both helpers return `Option<T>` so callers use `if let Some(...) { ... continue; }`

---

## 7. Redundant `resolve_assoc_projections_with_impls` Calls

**Severity:** Low (performance, mitigated)
**File:** `crates/shadml_semantic/src/lib.rs`, `resolve_predicates_fixpoint`

**Problem:**
`resolve_assoc_projections_with_impls` is called 4-5 times per predicate per loop iteration inside the fixpoint loop. With the extraction to a shared function and `MAX_PREDICATE_ITERATIONS = 16`, the worst case is bounded but not eliminated.

**Fix direction:** Cache resolved types between iterations. Extract `Predicate::resolve_assoc_projections` helper.

---

## 8. `lookup_assoc_type_binding` Normalizes Params on Every Call

**Severity:** Low (performance)
**File:** `crates/shadml_semantic/src/lib.rs`

**Problem:**
Every call allocates a new `Vec<Ty>` by normalizing every parameter. Called frequently from `resolve_assoc_projections_with_impls`.

**Fix direction:** Pre-normalize types when they enter the system, or memoize normalization.

---

## 9. `Display` for `Ty::AssocProj` Drops Non-First Trait Params and Trait Name — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- `Display` impl now shows `(Add<F32, F32>).Output` instead of `(F32.Output)`
- `format_ty_surface` now shows `(Add<F32, F32>).Output` instead of `F32.Output`
- Both include the trait name and all trait params

---

## 10. `var_only_in_assoc_proj_params` / `contains_var_structural` Subtlety

**Severity:** Low
**File:** `crates/shadml_typechecker/src/lib.rs`

**Problem:**
`contains_var_structural` returns `false` for any `AssocProj` unconditionally, without recursing into `trait_params`. If a trait param contains a structural type (e.g., `Arrow(Var(a), Var(a))`), the occurs-check bypass would apply incorrectly. Current code only has simple `Var` params, so this is latent.

**Fix direction:** Add a comment. Consider recursing into `trait_params` with `contains_var_structural` if complex param types ever appear.

---

## 11. `resolve_hir_expr_assoc_projections` Fragility — FIXED

**Status:** Fixed (2026-04-19)

**What changed:**
- Extracted `map_hir_expr_types<F>(expr: HirExpr, f: &F)` — a generic traversal that applies a type-mapping function to every type annotation in an `HirExpr` tree
- Reimplemented `resolve_hir_expr_assoc_projections` as a 4-line function delegating to `map_hir_expr_types`
- Added documentation comment on `mono_mangled_function_name` and `map_hir_expr_types` noting the relationship with `finalize_expr`
- Rust's exhaustiveness check ensures new `HirExpr` variants update both `map_hir_expr_types` and `finalize_expr`

---

## 12. Duplicated `resolve_inferred_predicates` — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- Extracted `resolve_predicates_fixpoint` as a shared free function in `shadml_semantic`
- Both `SemanticAnalyzer::resolve_inferred_predicates` and `LoweringContext::resolve_inferred_predicates` now delegate to this shared function
- The AST lowering version no longer has the single-pass / stale-subst bug
- Added `MAX_PREDICATE_ITERATIONS = 16` to bound the fixpoint loop
- Also extracted `resolve_assoc_projections_in_subst` as a shared free function (was duplicated as a method on both structs)
- Made `format_impl_head` public in `shadml_semantic`, removed duplicate `format_impl_head_local` from lowering
- Removed unused `try_improve_predicate` and `has_impl_for_predicate` methods from `SemanticAnalyzer`

---

## 13. No Error Recovery in `parse_type_projections`

**Severity:** Low
**File:** `crates/shadml_parser/src/parser.rs`

**Fix direction:** Add `break` after failed `expect` to prevent cascading diagnostics.

---

## 14. `Type::Proj` Only Allows `UpperIdent`

**Severity:** Low
**File:** `crates/shadml_parser/src/parser.rs`

**Fix direction:** Document the convention. Change if lowercase associated types are desired.

---

## 15. `try_improve_predicate_with_impls` Discards Associated Type Bindings — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- Added `apply_assoc_type_bindings` function that, when a single impl candidate matches a predicate, scans the substitution for type variables mapped to `AssocProj` nodes referencing the matching impl's trait and params, and updates them to the concrete binding value directly.
- Both user impl and builtin impl paths now propagate associated type bindings.
- This accelerates convergence of the fixpoint loop — `AssocProj` is resolved eagerly rather than waiting for `resolve_assoc_projections_in_subst` on the next iteration.

---

## 16. No Validation That Impls Define All Trait Associated Types — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- Added validation in Pass 2c for both `ImplDecl` and `BuiltinImplDecl` that checks all trait associated types have bindings
- Reports a clear error: "missing associated type(s): Output" with help text suggesting `type Output = ...`
- Added regression test `impl_missing_associated_type_binding_errors`

---

## 17. `AssocTypeContext` Only Uses First Constraint With Associated Types — FIXED

**Status:** Fixed (2026-04-19)

**What changed:**
- Deleted `AssocTypeContext` struct and its `to_assoc_proj` method
- Replaced single `Option<&AssocTypeContext>` with `&[(String, Vec<Ty>)]` (trait name + trait params), matching the lowering crate's pattern
- `convert_syntax_type_sig` now always calls `convert_syntax_type_with_scope_assoc` with all constraint contexts (empty slice when none)
- `Type::Proj` arm now searches all constraint contexts for matching associated type names
- Detects ambiguity when multiple constraints have the same associated type name, emitting a diagnostic: "ambiguous associated type `.Output` — found in traits: Add, Mul"
- Fallback global trait search (`find_assoc_type_context`) still used when no constraint context has the associated type
- `Type::Self_` arm now uses `constraint_contexts.first()` instead of `assoc_ctx`

---

## 18. Bare Associated Type Names Shadow Type Constructors — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- Bare names in type position now always resolve to top-level types (data, alias, bitfield, builtin)
- Associated types are accessed via `Self.Output` (inside trait bodies) or `a.Output` (dot-projection)
- Removed bare-name `assoc_ctx` checks from `Type::Con` and `Type::Var` arms in semantic and lowering
- Added `KwSelf` keyword token, `Type::Self_` AST node, and full error diagnostics for misuse

---

## 19. No Unit Tests for `AssocProj` in Typechecker — FIXED

**Status:** Fixed (2026-04-19)

**What changed:**
- Added 11 comprehensive unit tests covering all `AssocProj` behaviors:
  - `test_assoc_proj_contains_var` — finds var in `trait_params`, not in `name`/`trait_name`
  - `test_assoc_proj_var_only_in_params_true` — returns true when var is only in params
  - `test_assoc_proj_var_only_in_params_false_structural` — returns false when var also appears structurally
  - `test_assoc_proj_contains_var_structural` — returns false for AssocProj (params are type-level inputs)
  - `test_assoc_proj_apply_subst` — substitution applies to `trait_params` only, preserves `name`/`trait_name`
  - `test_assoc_proj_free_vars` — collects free vars from `trait_params`
  - `test_assoc_proj_display` — format: `(Add<F32, F32>).Output`
  - `test_unify_assoc_proj_matching` — two AssocProj with same name/trait unify params
  - `test_unify_assoc_proj_permissive` — AssocProj unifies with any type
  - `test_unify_assoc_proj_occurs_bypass` — `a = (Add<a, F32>).Output` is NOT an infinite type
  - `test_unify_assoc_proj_occurs_structural` — `a = a -> (Add<a, F32>).Output` IS an infinite type
  - `test_unify_assoc_proj_different_trait` — permissive unification accepts different traits

---

## 20. `convert_syntax_type_pure` Drops `Type::Proj` Base

**Severity:** Low
**File:** `crates/shadml_ast_lowering/src/lib.rs`, ~line 3224

**Problem:**
`Type::Proj(_base, name, _)` in `convert_syntax_type_pure` discards `_base` entirely and converts the projection to just `Ty::Con(name)`. This loses all type information. If this path is ever exercised with types containing `Proj`, it would produce incorrect types.

**Impact:** Depends on whether `convert_syntax_type_pure` is ever called with types containing `Proj`. If not, it's dead code; if so, it's a bug.

**Fix direction:** Verify whether `convert_syntax_type_pure` is ever called with `Proj` types. If not, add a panic or comment. If so, fix the conversion.

---

## 21. `"Output"` Hardcoded in `resolve_binop_trait_impl` — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- Replaced hardcoded `"Output"` with `trait_info.associated_types.first()` so the function uses the trait's actual first associated type name

---

## 22. `BuiltinImplInfo.impl_tys` Not Normalized — FIXED

**Status:** Fixed (2026-04-18)

**What changed:**
- `BuiltinImplInfo.impl_tys` now applies `normalize_type_aliases` consistently with `ImplInfo.impl_tys`, preventing failed lookups in `lookup_assoc_type_binding`

---

## 23. `resolve_assoc_projections_in_subst` Non-Transitive

**Severity:** Low
**File:** `crates/shadml_semantic/src/lib.rs`, `resolve_assoc_projections_in_subst`

**Problem:**
The function collects all entries first, then updates. If resolving entry A produces a type that references entry B, and B was already processed and found "unchanged" in the same pass, B is missed.

**Mitigation:** Now called inside the fixpoint loop of `resolve_predicates_fixpoint`, so transitive dependencies are handled across iterations rather than within a single pass. The `MAX_PREDICATE_ITERATIONS = 16` cap bounds this.

**Fix direction:** Run a second pass or iterate until stable within `resolve_assoc_projections_in_subst` itself.