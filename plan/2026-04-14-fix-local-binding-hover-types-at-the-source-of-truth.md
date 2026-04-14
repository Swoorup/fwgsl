# Fix Local Binding Hover Types at the Source of Truth

## Summary

Replace the current IDE-side reconstruction of local binding types with first-class semantic metadata for local bindings. The bug is not just formatting: atten is being shown from initializer-expression
state instead of the finalized binding scheme produced by local let inference. The correct fix is to carry exact local-binding declaration spans through the AST, record binding schemes during semantic
analysis, and make hover/type hints read those schemes directly.

## Key Changes

- Add explicit local-binding AST nodes
    - Introduce a parser AST struct such as LocalBind { name, name_span, expr, span }.
    - Use it in:
        - Decl::FunDecl.where_binds
        - Expr::Let
        - Expr::Loop
        - DoStmt::Bind and DoStmt::Let
    - Keep record fields and record updates unchanged; only declaration-like local bindings need this.
    - Preserve original spans through desugar_where so semantic analysis can still attribute where bindings to their authored declaration site.
- Refactor parser to produce exact binding spans
    - Update parse_where_binds, parse_let, parse_do, and parse_loop to capture the binding name token span and full binding span when parsing.
    - Update parser unit/integration tests that currently assert tuple-shaped bindings.
- Record finalized binding schemes in semantic analysis
    - Add semantic metadata for local bindings keyed by authored name_span, for example local_binding_schemes: HashMap<Span, Scheme>.
    - Populate it at the exact point where local bindings are inserted into local_env:
        - Expr::Let
        - desugared where
        - DoStmt::Let
        - DoStmt::Bind if it should surface a bound name type after monadic bind
        - loop parameter bindings if they are hoverable declarations
    - Store the scheme after:
        - initializer inference
        - predicate resolution / improvement
        - final generalization against the local environment
    - Keep expr_types for expression-oriented features such as operator definition resolution, but stop using it as the source of hover types for local binding declarations.
- Simplify IDE hover/type collection
    - Remove the current collect_local_binding_types / token-search path for local declaration hovers.
    - Resolve local declaration hover/type hints by symbol primary_span into semantic local_binding_schemes.
    - Format local binding schemes with the same surface formatter used elsewhere:
        - monomorphic F32 stays F32
        - qualified locals render constraints if they truly remain constrained
        - no dropping of constraints, no synthetic a from bare Ty formatting unless that is the actual scheme
    - Keep explicit-signature source-text preservation for top-level declarations as-is.
- Update dependent consumers
    - Update ast_lowering and any parser/semantic tests that destructure tuple-shaped local bindings.
    - HIR can remain tuple-shaped unless there is a separate need for declaration-span metadata there; the important change is that semantic analysis now owns the authoritative hover metadata before lowering
      consumers erase source detail.

## Test Plan

- Parser
    - Add tests that let, where, do let, do bind, and loop bindings retain correct name_span / binding span data.
    - Update existing tuple-binding parser tests to assert the new structured binding nodes.
- Semantic
    - Add focused tests for local binding schemes:
        - dist in distance light.lightPosition worldPos records as F32
        - spotFactor records as F32
        - atten records as F32
    - Add one negative/control case where a truly constrained local binding remains qualified, to verify hover reflects the real scheme rather than forcing monomorphism.
- IDE/LSP
    - Add hover regressions on examples/slang-generics.shadml for:
        - dist : F32
        - spotFactor : F32
        - atten : F32
        - existing lambert, worldPos, light, and lightDirection cases remain correct
    - Ensure no IDE code path still computes local-binding types by scanning tokens and pairing names with initializer spans.

## Assumptions

- The desired behavior is to show the finalized local binding scheme at the declaration/reference site, not the raw initializer expression type before local generalization and later predicate improvement.
- atten should ultimately be F32; if the new semantic binding-scheme test shows it remains constrained/generic, that is a separate semantic inference bug and should be fixed in semantic analysis rather than
  masked in hover formatting.
- This plan intentionally avoids span-reconstruction heuristics or name-based postprocessing in the IDE; the source of truth should live in parser + semantic metadata.
