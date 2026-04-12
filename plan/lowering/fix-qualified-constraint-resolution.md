# Fix Qualified Constraint Resolution in AST/HIR Lowering

Created: 2026-04-14
Progress: Completed

## Summary

Fix the bug at the real source: AstLowering is discarding typeclass constraints when it instantiates schemes, so overloaded operator results can remain as unresolved types like Vec n a and leak into MIR. The fix should mirror semantic analysis, not patch MIR and not special-case length, Sub, or negate.

negate is not the current cause. Keep unary-negation behavior unchanged and cover it with regressions.

## Implementation Changes

- Add shared predicate-resolution support that both semantic analysis and AST/HIR lowering use.
    - Extract the reusable parts of semantic predicate improvement from shadml_semantic:
        - candidate matching against user impls and builtin impls
        - builtin preference logic for numeric/vector operator heads
        - predicate resolution/improvement against an InferEngine
    - Keep this as an internal compiler helper API; no user-facing language change.
- Teach AstLowering to preserve qualified constraints.
    - Add inferred_predicates: Vec<Predicate> to AstLowering, mirroring SemanticAnalyzer.
    - Add a local helper that uses engine.instantiate_qualified(...), appends constraints to inferred_predicates, and returns the instantiated type.
    - Replace expression-level instantiate(...) uses with qualified instantiation at the relevant lookup sites:
        - variable references
        - operator lookup for infix expressions
        - dot-call sugar lookup
        - operator sections
        - any other expression-level callable lookup that currently pulls a scheme from env
- Resolve pending predicates during AST/HIR lowering using the shared helper.
    - Port the semantic-style "capture predicate start, infer/unify, then improve/resolve" flow into AST lowering.
    - Apply it at the same boundaries where AST lowering is currently re-running inference:
        - function bodies
        - entry-point bodies
        - let/where bindings
        - expression forms that introduce/consume overloaded callables (App, Infix, dot-call sugar)
    - Preserve current diagnostics behavior for unresolved or missing impls; do not silently default overloaded operators.
- Keep MIR strict.
    - Do not change MIR type conversion to accept partially applied or unresolved Vec/Mat types.
    - The invariant after AST/HIR lowering should remain: MIR only sees concrete WGSL-representable types.

## Interfaces / Internal API Changes

- Add a shared semantic helper for predicate improvement/resolution, callable from both SemanticAnalyzer and AstLowering.
- Add inferred_predicates state plus one or two small helper methods inside AstLowering for:
    - qualified scheme instantiation
    - resolving/improving pending predicates after inference steps

No source-language syntax, CLI, MIR, or WGSL output interfaces should change.

## Test Plan

- Add a regression that compiles the minimal failing shape:
    - dist = length (fragCoord - mouse)
- Add a regression for the full examples/shadorial/02-uniforms.shadml compile path.
- Add a regression for where-bound intermediates:
    - uv = fragCoord / resolution
    - mouseN = mouse / resolution
    - dist = length (uv - mouseN)
- Add unary-negation regressions to prove it is not broken by the refactor:
    - length (-fragCoord)
    - length (negate fragCoord)
- Add one higher-order/operator-value regression so qualified constraints are preserved outside direct infix syntax:
    - binding/passing ((-)) or equivalent operator-as-function use, then applying it to vectors
- Add one negative test that still reports a missing impl when no unique trait implementation exists.

## Assumptions

- The correct fix is to make AST/HIR lowering honor qualified schemes and shared predicate improvement, not to relax MIR or add ad hoc operator special cases.
- Unary negate is not the root cause; it only needs regression coverage.
- The change should stay scoped to compiler-internal inference/lowering behavior and should not alter the surface language or WGSL semantics.