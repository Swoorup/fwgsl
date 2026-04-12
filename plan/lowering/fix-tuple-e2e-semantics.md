# Fix Tuple Semantics End-to-End

Created: 2026-04-14
Progress: Completed

## Summary

The failure comes from a source-semantics/backend-semantics mismatch. Semantic analysis infers test2 a b (k, j) = a as a 3-parameter function a -> b -> (c, d) -> a, but AST lowering currently flattens tuple-pattern parameters into 4 parameters and then reuses the semantic analyzer's inferred scheme from env. That forces unification between a -> b -> (c, d) -> a and a -> b -> c -> d -> a, which produces the occurs-check error t339 ~ (t338 -> t339).

The correct fix is to preserve tuple-argument semantics through typing and HIR, then do tuple ABI/codegen flattening in an explicit later lowering step. Do not "fix" this by special-casing unannotated functions or by flattening inferred schemes to match the current bug.

## Key Changes

- Make AST→HIR lowering preserve source tuple semantics.
  - In crates/shadml_ast_lowering/src/lib.rs, stop flattening tuple patterns into multiple function parameters during lower_fun_decl and lower_entry_point.
  - Stop applying flatten_tuple_arrows to function signatures as part of type collection for lowering.
  - Treat a tuple-pattern parameter as one parameter with a tuple type, matching semantic analysis arity and inferred schemes.
- Extend HIR to represent tuple values and tuple destructuring explicitly.
  - In crates/shadml_hir/src/lib.rs, add tuple-capable expression forms:
      - HirExpr::Tuple(Vec<HirExpr>, Ty, Span)
      - HirExpr::TupleIndex(Box<HirExpr>, usize, Ty, Span) or an equivalent tuple projection node
  - Keep HirFunction.params as one entry per source parameter.
  - For tuple-pattern parameters, lower to a synthetic single HIR param plus body-local scalar bindings using TupleIndex, e.g. _arg2 : (kTy, jTy) then let k = tuple_index _arg2 0, let j = tuple_index _arg2 1.
- Add an explicit tuple-elimination / ABI-flattening pass after HIR typing and before MIR WGSL lowering.
  - This pass is responsible for removing all tuple-typed params/locals/expressions before MIR/WGSL conversion.
  - Function params: expand tuple-typed params to flat scalar params only here.
  - Call sites: expand tuple arguments to flat args based on the callee's lowered ABI, including tuple variables and tuple-valued lets, not just literal (a, b) syntax.
  - Local tuple values: scalarize tuple lets and tuple projections so no Ty::Tuple survives into MIR.
  - Residual tuple types after this pass should be treated as compiler bugs and diagnosed clearly.
- Keep source typing rules unchanged.
  - (A, B) -> R remains distinct from A -> B -> R.
  - A tuple-pattern parameter is still one source parameter.
  - The existing semantic rejection of pairSum : (I32, I32) -> I32 with pairSum a b = ... remains correct and should stay in semantic analysis.

## Internal Interfaces

- HirExpr gains tuple constructs as described above.
- Add a dedicated tuple-lowering pass between HIR construction and MIR lowering, rather than baking tuple flattening into AstLowering.
- Remove the current tuple-parameter flattening helper behavior from AST lowering; if a helper remains, it should move into the new tuple-elimination pass and operate on typed HIR, not on source-level signatures.

## Test Plan

- Add a regression for the exact failing minimal case:
  - test2 a b (k, j) = a
  - check and compile must both succeed.
- Keep and extend tuple function coverage:
  - pairSum : (I32, I32) -> I32
  - pairSum (a, b) = a + b
  - result = pairSum (1, 2)
- Add a tuple-variable call regression:
  - p = (1, 2)
  - result = pairSum p
  - This must compile, proving the fix is not limited to literal tuple arguments.
- Keep the negative semantic regression:
  - pairSum : (I32, I32) -> I32
  - pairSum a b = a + b
  - Must still fail with the existing arity error.
- Add an invariant test that no tuple type reaches WGSL type conversion after tuple elimination.
- Add an example-level regression for examples/tuple.shadml on the full compile path.

## Assumptions

- Tuple types are first-class in the surface language and type system, but must be eliminated before MIR/WGSL because WGSL has no tuple type.
- The correct architectural boundary for tuple flattening is backend lowering, not semantic analysis and not AST lowering.
- Existing compile behavior for tuple functions that only works via early flattening is incidental; preserving source tuple semantics takes priority over that implementation shortcut.