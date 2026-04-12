## Concrete Builtin Inventory and Prelude Docs

  ### Summary

  Keep extern as the value-level declaration keyword, but split compiler-provided builtin support into two prelude-only forms:

  - builtin extern for concrete callable overloads
  - builtin impl ... where for concrete compiler-provided trait instances with per-method lowering entries

  This makes the prelude the authoritative source for builtin availability, lowering, and documentation, and removes the current hardcoded builtin support matrix from semantic/lowering code.

  ### Public Syntax

  - Keep ordinary extern unchanged for non-builtin callable declarations.
  - Add prelude-only builtin extern:
      - builtin extern sin : F32 -> F32 = intrinsic(sin)
      - builtin extern dot : Vec<3, F32> -> Vec<3, F32> -> F32 = intrinsic(dot)
  - Add prelude-only builtin impl block form:
      - builtin impl Mul F32 F32 F32 where (*) = native_binop(*)
      - builtin impl Light PointLight where position = intrinsic(light_position)
  - Lowering spec syntax in v1:
      - intrinsic(name)
      - native_binop(op)
      - native_unary(op)
  - builtin extern is overloadable by name.
  - builtin impl is exact-match on the full concrete impl head and supports any number of trait methods.
  - -- | docs on builtin extern / builtin impl / trait methods are the only builtin documentation source.

  ### Implementation Changes

  - Parser / AST:
      - Add BuiltinExternDecl and BuiltinImplDecl.
      - Add builtin-lowering AST nodes for intrinsic, native_binop, and native_unary.
      - Add builtin-impl method entries of the form methodName = loweringSpec.
      - Preserve attached doc comments on builtin declarations and builtin impl methods.
      - Reject builtin declarations outside the compiler-owned prelude/internal module context.
  - Semantic collection:
      - Build a builtin callable overload inventory from builtin extern.
      - Build a builtin trait-instance inventory from builtin impl.
      - Replace builtin_trait_impl_exists and builtin_head_for_predicate hardcoded logic with lookup against collected builtin impl inventory.
      - Resolve builtin extern calls by exact concrete overload match after inference.
  - Lowering:
      - Use builtin impl method metadata for trait-backed operator/method lowering.
      - Use builtin extern metadata for direct callable lowering.
      - Keep user trait impls lowering to mangled helper functions; only builtin decls use direct intrinsic/native lowering.
  - IDE / docs:
      - Remove hardcoded builtin descriptions from IDE catalog/hover/completion paths where they overlap with prelude symbols.
      - Surface docs from prelude comments for builtin externs and builtin impl methods.
      - For overloaded builtin externs, show the concrete overload signatures and shared/attached doc text.

  ### Prelude Reshape

  - Remove generic placeholder builtin operator declarations like extern (*) : a -> a -> a.
  - Replace them with concrete builtin impl inventory:
      - scalar arithmetic
      - vector-scalar and scalar-vector arithmetic
      - matrix-scalar and matrix-vector arithmetic
      - bitwise and unary operator coverage
  - Replace generic named builtin declarations with concrete builtin extern overloads for supported WGSL builtins.
  - Keep ordinary trait declarations in the prelude so user impls still target the same traits.
  - Keep docs directly above each builtin entry or builtin overload group with -- |.

  ### Test Plan

  - Parser:
      - parse builtin extern with concrete type, lowering spec, and doc comments
      - parse builtin impl ... where with one and multiple method entries
      - reject builtin declarations outside prelude/internal context
  - Semantic:
      - builtin operator support is driven only by builtin impl inventory
      - unsupported concrete operator/type combinations fail when absent from prelude
      - builtin extern overload resolution succeeds only for declared concrete cases
      - multi-method builtin trait impls are collected and resolved correctly
  - IDE:
      - hover/completion docs for builtin names come from prelude -- |
      - overloaded builtin externs show coherent signatures
  - Integration:
      - existing WGSL builtin examples continue to compile after prelude conversion
      - removing a builtin inventory entry causes a predictable semantic failure
      - user trait impl behavior remains unchanged

  ### Assumptions

  - builtin extern and builtin impl are legal only in the compiler-owned prelude/internal modules.
  - v1 builtin lowering metadata is limited to intrinsic, native_binop, and native_unary.
  - Ordinary user extern remains simple and non-overloaded; only builtin extern forms overload by name.
  - extern remains value-level only; trait instances use builtin impl, never extern.
