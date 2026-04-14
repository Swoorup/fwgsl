# Fix LSP Type Presentation and Operator Definition Resolution

## Summary

This is primarily an IDE/LSP correctness pass, not a compiler/codegen change. The existing slang-generics integration test already passes, so the plan should target three layers:

- fix parser type spans so source-backed type hints are not truncated
- make hover/type hints use the right source of truth for authored vs inferred types
- add operator go-to-definition that resolves to the concrete prelude builtin impl when that impl is uniquely determined

## Key Changes

- Parser span fix
    - Update angle-type parsing so Type::span() for Vec<3, F32> and nested generic forms includes the closing >.
    - Keep source-backed type extraction viable for record fields and explicit type signatures.
- Type display policy in IDE/LSP
    - Add a source-authored signature cache in the IDE state for explicit type signatures.
    - When a symbol has an explicit user-written signature, display that exact surface syntax from source spans instead of reformatting the inferred scheme.
    - This preserves:
        - Vec<3, F32> when the signature was written with angle syntax
        - Vec 3 F32 when the signature was written with space application
    - Add canonical inferred-type formatting for non-authored types so free variables become stable names like a, b instead of t138, and internal forms like ((Vec 3) F32) never reach hover output.
- Parameter and local binding hover fixes
    - Extend semantic/IDE metadata so local bindings get inferred types, not declaration excerpts.
    - Record expression result types during semantic inference, then map let/where/do/loop bindings to the type of their initializer expression.
    - Use enclosing callable scheme context when formatting parameter types so:
        - worldPos in lighting shows Vec<3, F32>
        - light in lighting shows a rather than t138
        - lambert shows F32
- Field/type-hint fix
    - Keep field hover/type hints source-backed once parser spans are corrected so lightDirection shows the full Vec<3, F32>.
- Operator go-to-definition
    - Detect operator tokens under the cursor, not just identifiers.
    - Resolve the enclosing operator application and use inferred operand/result types to find the matching builtin prelude impl.
    - Return the location in prelude/prelude.shadml for the concrete builtin impl method when the match is unique.
    - If no unique builtin impl matches, fall back to the existing definition behavior rather than returning a wrong location.
- Public/internal API additions
    - Expose a canonical standalone type formatter in shadml_typechecker for inferred Ty values.
    - Add read-only semantic metadata for expression result types so IDE features can consume inference results without re-implementing inference.
    - Add IDE/LSP helper(s) for compiler-prelude builtin-impl lookup by operator name + concrete types.

## Test Plan

- Add parser tests proving Type::span().source_text(...) includes the full text for:
    - Vec<3, F32>
    - nested generics like Vec<3, Vec<2, F32>>
- Add IDE hover tests on examples/slang-generics.shadml for:
    - worldPos in lighting -> Vec<3, F32>
    - lambert reference -> F32
    - lightDirection field declaration/access -> full Vec<3, F32>
    - light in lighting -> a
- Add signature-display tests proving:
    - explicit angle-style signatures stay angle-style
    - explicit space-application signatures stay space-style
- Add go-to-definition tests for symbolic operators in slang-generics that assert the returned URI is prelude/prelude.shadml and the target is the matching builtin impl method.

## Assumptions

- “Go to definition for operator” should prefer the concrete prelude builtin impl entry, not the trait declaration or generic extern signature, when that impl is uniquely determined.
- For inferred-only types with no authored source syntax, canonicalized pretty output is acceptable; exact original syntax preservation is only required for explicitly written signatures/types.
- The scope of this fix is IDE/LSP behavior plus the parser span bug needed to support correct presentation; it should not alter WGSL generation or trait resolution semantics.
