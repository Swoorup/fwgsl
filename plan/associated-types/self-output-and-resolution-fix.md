# Add `Self.Output` and Fix Associated Type Resolution

Created: 2026-04-18
Progress: 7/7 phases (Completed)

Bare `Output` in type position inside trait bodies silently shadows top-level types like `data Output`. Fix: bare names always resolve to top-level types; associated types require `Self.Output` (inside trait bodies) or `a.Output` (via dot-projection).

## Key Changes

- [x] 1. Add `KwSelf` to `shadml_syntax/src/lib.rs`
- [x] 2. Recognize `Self` in `lex_upper_ident` in `shadml_parser/src/lexer.rs`
- [x] 3. Add `Type::Self_` and parse it in `shadml_parser/src/parser.rs`
- [x] 4. Fix associated type resolution in `shadml_semantic/src/lib.rs`
- [x] 5. Handle `Self` in lowering in `shadml_ast_lowering/src/lib.rs`
- [x] 6. Update `.shadml` files (13 bare `Output` → `Self.Output`)
- [x] 7. Add UI tests and update snapshots

## Test Plan

- [x] `cargo test` — full suite passes
- [x] `cargo test -p shadml_integration_tests --test ui` — snapshots correct
- [x] `assoc-type-no-shadow.shadml` verifies `data Output` + `Self.Output` coexist

## Assumptions

- `Self` is only valid inside trait bodies, followed by `.Name`
- `Self` alone is an error
- `Self.Output` outside a trait body is an error
- Bare `Output` always resolves to top-level type constructors (data, alias, bitfield, builtin)