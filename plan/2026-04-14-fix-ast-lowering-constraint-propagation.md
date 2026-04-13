## Fix AST Lowering Constraint Propagation

### Summary

Make AST→HIR lowering preserve and resolve qualified trait constraints the same way semantic analysis already does, so generic functions with explicit constraints compile correctly through the full pipeline.

### Key Changes

- Change AstLowering::lower_expr to accept active_constraints: &[Predicate].
- Thread active_constraints from lower_fun_decl and lower_entry_point into every recursive lower_expr call.
- Replace hardcoded empty constraint resolution in Expr::Let with the surrounding active_constraints, matching semantic analysis.
- Audit any other local predicate-resolution boundaries in lowering and ensure they compare against the active outer constraints before reporting missing trait constraint.
- Keep the current qualified instantiation path (instantiate_qualified) and current missing-impl diagnostics; do not add ad hoc trait inference or special handling for Light.

### Tests

- Add a regression for the full compile path of examples/slang-generics.shadml.
- Add a minimal compile regression for a constrained generic function with a let binding that calls a trait method.
- Keep or add a control test showing check and compile both succeed for the same constrained example.

### Assumptions

- The language rule remains: explicit constraints on the function signature are sufficient.
- The bug is in lowering-time predicate handling, not in parsing, semantic analysis, or the example source.
