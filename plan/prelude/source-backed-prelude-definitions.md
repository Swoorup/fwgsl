# Source-Backed Prelude Definitions, Docs, and Go-To-Definition

Created: 2026-04-14
Progress: Completed

## Summary

Make prelude/prelude.shadml the single source of truth for builtin symbols, documentation, and declaration locations. The implementation should stop treating prelude docs as a Rust-side catalog problem and instead treat the prelude as a real indexed document, with parser/semantic support for currently compiler-internal builtin type constructors.

This plan covers both requested fixes:

- go-to-definition for ordinary prelude function declarations
- documentation for builtin/prelude symbols sourced from Haddock comments instead of Rust string tables

## Key Changes

- Extend the language surface so all builtin type constructors are declared in source.
    - Add a new declaration form: builtin type Name with optional arity, e.g. builtin type Vec 2, builtin type Mat 3, builtin type F32.
    - Use this for compiler-internal type constructors that are currently only hard-coded in Rust: I32, U32, F32, Bool, String, Vec, Mat, Tensor, Scalar, and any other compiler-known type constructor that currently has IDE docs or completion entries.
    - Keep source aliases in prelude/prelude.shadml for surface synonyms that should remain user-visible, e.g. alias Vector = Vec, alias Matrix = Mat, alias Ten = Tensor, alias Sca = Scalar.
    - Add Haddock comments above every builtin declaration in the prelude, including existing data, alias, extern, and builtin impl entries.
- Make semantic builtin registration derive from prelude declarations instead of a hard-coded whitelist.
    - Add Decl::BuiltinTypeDecl to the parser AST and semantic passes.
    - Track builtin type constructors and their arity in semantic analysis so convert_syntax_type_with_scope recognizes source-declared builtin types rather than relying on the current name whitelist.
    - Keep lowering-specific normalization where needed, but move alias visibility and documentation ownership into source declarations.
    - Ensure ExternDecl and the new builtin type declarations participate in the same symbol/metadata pipeline as other top-level declarations.
- Introduce a real prelude IDE index/state and use it for hover, completion docs, and definitions.
    - Build a separate indexed IdeState for prelude/prelude.shadml, with its own source text, symbol index, signatures, and extracted Haddock docs.
    - In user documents, resolve builtin/prelude symbols through that prelude state when they are not defined locally.
    - Hover for builtin names should render the actual prelude declaration/signature plus the Haddock comment from the prelude source.
    - Completion items for prelude symbols should keep Rust-side sort/context/snippet behavior if needed, but their signature/detail/documentation must come from the prelude state, not from catalog.rs.
    - Reduce the Rust catalog to language keywords and attributes only; remove hard-coded documentation for prelude-defined values and types.
- Unify go-to-definition around declaration ownership.
    - Ordinary builtin/prelude functions like normalize, sin, distance, vec3, and builtin types like F32 or Vec should go to their declaration in prelude/prelude.shadml.
    - Operator go-to-definition should keep its current concrete builtin-impl behavior and continue preferring the matching builtin impl method.
    - Fix the IDE indexer to include ExternDecl and BuiltinTypeDecl so opening the prelude file itself also supports hover and go-to-definition correctly.
    - Move prelude-definition lookup behind a single IDE resolver instead of relying on LSP-side ad-hoc fallbacks and string scanning.

## Important Interface Changes

- Parser / AST
    - Add Decl::BuiltinTypeDecl { name, arity, comments, span }.
    - Parse builtin type Name and builtin type Name N in prelude/prelude.shadml.
- Semantic
    - Add builtin type metadata keyed by source declaration, including arity.
    - Replace the current hard-coded "known builtin type names" check with source-declared builtin types plus aliases.
- IDE / LSP
    - Add a prelude-backed symbol/doc lookup path used by hover, completions, and go-to-definition.
    - Make definition results carry the prelude URI/range for source-backed builtin declarations.

## Test Plan

- Parser
    - Parse builtin type F32, builtin type Vec 2, and doc-commented builtin type declarations.
    - Snapshot/update prelude parsing so builtin type declarations and their comments are preserved.
- Semantic
    - Verify builtin type constructors are registered from prelude source, not from the old whitelist path.
    - Verify source aliases like Vector, Matrix, Ten, and Sca resolve through prelude declarations.
    - Verify existing builtin value registration from prelude still works unchanged.
- IDE / LSP
    - Hover on sin, normalize, vec3, Option, F32, and Vec from a user file should show Haddock text from prelude/prelude.shadml.
    - Go-to-definition on normalize, distance, vec3, Option, F32, and Vec from a user file should jump to prelude/prelude.shadml.
    - Go-to-definition on +/- should still jump to the matching builtin impl method, not the trait or declaration.
    - Opening prelude/prelude.shadml directly should support hover and go-to-definition for extern and builtin type declarations.

## Assumptions

- "Function declaration in the prelude" means ordinary named builtin declarations should resolve to the declaration site in prelude/prelude.shadml, not to intrinsic/lowering metadata.
- Keyword and attribute documentation remain Rust-catalog-backed; only prelude-defined symbols move to source-backed docs.
- The prelude file becomes the authoritative content source for builtin symbol docs, while Rust may still keep non-doc completion behavior such as sort groups or snippet templates.